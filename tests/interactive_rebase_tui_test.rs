#![cfg(not(windows))]

//! Binary-level outcome-seam coverage for the interactive-rebase TUI.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::Value;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn reserve_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct RunningApp {
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    reader: thread::JoinHandle<()>,
}

impl RunningApp {
    fn wait(mut self) {
        self.child.wait().unwrap();
        drop(self.master);
        self.reader.join().unwrap();
    }
}

fn start_app(repo: &Path, port: u16) -> RunningApp {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_keifu"));
    command.args(["--debug-listen", &format!("127.0.0.1:{port}")]);
    command.cwd(repo);
    let child = pty.slave.spawn_command(command).unwrap();
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().unwrap();
    let reader_thread = thread::spawn(move || {
        let mut buffer = [0; 8192];
        while matches!(reader.read(&mut buffer), Ok(n) if n > 0) {}
    });
    RunningApp {
        child,
        master: pty.master,
        reader: reader_thread,
    }
}

fn request(port: u16, body: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => break stream,
            Err(error) if Instant::now() < deadline => {
                assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("debug server did not start: {error}"),
        }
    };
    stream.write_all(format!("{body}\n").as_bytes()).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    serde_json::from_str(response.trim()).unwrap()
}

#[test]
fn debug_server_routes_commit_menu_into_review_and_cancel_without_rewriting_history() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    git(repo.path(), &["config", "user.name", "TUI Test"]);
    git(repo.path(), &["config", "user.email", "tui@example.test"]);
    for (file, message) in [("base.txt", "base"), ("one.txt", "one"), ("two.txt", "two")] {
        std::fs::write(repo.path().join(file), format!("{message}\n")).unwrap();
        git(repo.path(), &["add", file]);
        git(repo.path(), &["commit", "-qm", message]);
    }
    let original_head = git(repo.path(), &["rev-parse", "HEAD"]);
    let original_log = git(repo.path(), &["log", "--format=%H %s"]);
    let port = reserve_port();
    let app = start_app(repo.path(), port);

    assert_eq!(
        request(port, r#"{"cmd":"keys","keys":"<down> <down> <enter>"}"#)["ok"],
        true
    );
    let menu = request(port, r#"{"cmd":"dump","width":120,"height":34}"#);
    assert!(
        menu["screen"]
            .as_str()
            .unwrap()
            .contains("Rebase commits above this…"),
        "commit menu must expose the interactive-rebase route"
    );
    assert_eq!(
        request(
            port,
            r#"{"cmd":"keys","keys":"<down> <down> <enter> d <enter>"}"#
        )["ok"],
        true
    );
    let confirm = request(port, r#"{"cmd":"dump","width":120,"height":34}"#);
    let confirm_screen = confirm["screen"].as_str().unwrap();
    assert!(confirm_screen.contains("Rebase 2 commits onto"));
    assert!(confirm_screen.contains("drop 1"));
    assert_eq!(request(port, r#"{"cmd":"state"}"#)["mode"], "confirm");

    assert_eq!(
        request(port, r#"{"cmd":"keys","keys":"<esc>"}"#)["ok"],
        true
    );
    assert_eq!(request(port, r#"{"cmd":"state"}"#)["mode"], "normal");
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), original_head);
    assert_eq!(git(repo.path(), &["log", "--format=%H %s"]), original_log);

    assert_eq!(
        request(port, r#"{"cmd":"keys","keys":"<c-q>"}"#)["ok"],
        true
    );
    app.wait();
}
