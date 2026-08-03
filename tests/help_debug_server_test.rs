#![cfg(not(windows))]

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
use tempfile::TempDir;

fn run_git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        .status()
        .unwrap();
    assert!(status.success(), "git command failed: git {args:?}");
}

fn fixture_repo(with_uncommitted_file: bool) -> TempDir {
    let repo = tempfile::tempdir().unwrap();
    run_git(repo.path(), &["init", "-q"]);
    run_git(repo.path(), &["config", "user.name", "Help Test"]);
    run_git(repo.path(), &["config", "user.email", "help@example.test"]);
    std::fs::write(repo.path().join("README.md"), "fixture\n").unwrap();
    run_git(repo.path(), &["add", "README.md"]);
    run_git(repo.path(), &["commit", "-qm", "fixture"]);
    if with_uncommitted_file {
        std::fs::write(repo.path().join("untracked.txt"), "pending\n").unwrap();
    }
    repo
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
    _master: Box<dyn MasterPty + Send>,
    reader_thread: thread::JoinHandle<()>,
}

impl RunningApp {
    fn wait(mut self) {
        self.child.wait().unwrap();
        drop(self._master);
        self.reader_thread.join().unwrap();
    }
}

fn start_app(repo: &Path, port: u16, extra_args: &[&str]) -> RunningApp {
    let binary = env!("CARGO_BIN_EXE_keifu");
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 300,
            cols: 140,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let address = format!("127.0.0.1:{port}");
    #[cfg(windows)]
    let mut command = {
        let mut command = CommandBuilder::new("cmd.exe");
        command.args(["/C", binary, "--debug-listen", &address]);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = CommandBuilder::new(binary);
        command.args(["--debug-listen", &address]);
        command
    };
    command.args(extra_args);
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
        _master: pty.master,
        reader_thread,
    }
}

fn request(port: u16, body: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => break stream,
            Err(error) if Instant::now() < deadline => {
                assert!(error.kind() == std::io::ErrorKind::ConnectionRefused);
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

fn help_screen(repo: &Path, with_uncommitted_file: bool) -> String {
    let port = reserve_port();
    let app = start_app(repo, port, &[]);
    let key_sequence = if with_uncommitted_file {
        "<home> ?"
    } else {
        "?"
    };
    let keys = request(
        port,
        &format!(r#"{{"cmd":"keys","keys":"{key_sequence}"}}"#),
    );
    assert_eq!(keys["ok"], true);
    let dump = request(port, r#"{"cmd":"dump","width":140,"height":160}"#);
    assert_eq!(dump["ok"], true);
    let screen = dump["screen"].as_str().unwrap().to_owned();

    for expected in [
        "Alt+I",
        "Alt+K",
        "Ctrl+P / Ctrl+Alt+P / :",
        "Ctrl+, / ,",
        "Ctrl+Shift+F",
        "l",
        "Filter files",
    ] {
        assert!(screen.contains(expected), "debug dump omitted {expected:?}");
    }
    assert!(
        screen
            .lines()
            .any(|line| line.contains("Ctrl+Shift+F") && line.contains("Filter commits")),
        "debug dump mismatched commit-filter shortcut and description"
    );
    assert!(
        screen.lines().any(|line| {
            line.split("Pull (fetch + integrate)")
                .next()
                .is_some_and(|prefix| prefix.trim_end().ends_with('l'))
        }),
        "debug dump mismatched pull shortcut and description"
    );
    assert!(
        screen
            .lines()
            .any(|line| line.contains("/ ") && line.contains("Filter files")),
        "debug dump mismatched file-filter shortcut and description"
    );
    if with_uncommitted_file {
        for expected in [
            "Stage/unstage file",
            "Accept ours (on conflicted file)",
            "Abort the in-progress operation",
        ] {
            assert!(screen.contains(expected), "debug dump omitted {expected:?}");
        }
    }

    let quit = request(port, r#"{"cmd":"keys","keys":"<c-q>"}"#);
    assert_eq!(quit["ok"], true);
    app.wait();
    screen
}

#[test]
fn debug_server_renders_updated_help_for_clean_and_uncommitted_repositories() {
    let clean = fixture_repo(false);
    help_screen(clean.path(), false);

    let uncommitted = fixture_repo(true);
    help_screen(uncommitted.path(), true);
}

fn launch_mode_screen(repo: &Path, mode: &str) -> String {
    let port = reserve_port();
    let app = start_app(repo, port, &[mode]);
    let dump = request(port, r#"{"cmd":"dump","width":140,"height":40}"#);
    assert_eq!(dump["ok"], true);
    let screen = dump["screen"].as_str().unwrap().to_owned();
    let quit = request(port, r#"{"cmd":"keys","keys":"<c-q>"}"#);
    assert_eq!(quit["ok"], true);
    app.wait();
    screen
}

#[test]
fn debug_server_renders_the_requested_reduced_launch_layouts() {
    let repo = fixture_repo(false);

    let bare = launch_mode_screen(repo.path(), "--bare");
    assert!(bare
        .lines()
        .next()
        .is_some_and(|line| !line.contains("Commit")));
    for absent in ["Changed Files", "Commit Detail", "? help"] {
        assert!(
            !bare.contains(absent),
            "bare dump rendered {absent:?}: {bare}"
        );
    }

    let scm = launch_mode_screen(repo.path(), "--scm");
    for expected in ["README.md", "Author:"] {
        assert!(
            scm.contains(expected),
            "SCM dump omitted {expected:?}: {scm}"
        );
    }
    for absent in ["Commits", "Changed Files", "Commit Detail", "? help"] {
        assert!(!scm.contains(absent), "SCM dump rendered {absent:?}: {scm}");
    }
}

#[test]
fn debug_server_keeps_bare_mode_focus_on_the_graph_after_tab() {
    let repo = fixture_repo(false);
    let port = reserve_port();
    let app = start_app(repo.path(), port, &["--bare"]);

    let tab = request(port, r#"{"cmd":"keys","keys":"<tab>"}"#);
    assert_eq!(tab["ok"], true);
    let state = request(port, r#"{"cmd":"state"}"#);
    assert_eq!(
        state["focused_panel"], "graph",
        "bare mode must not focus a removed pane"
    );

    let quit = request(port, r#"{"cmd":"keys","keys":"<c-q>"}"#);
    assert_eq!(quit["ok"], true);
    app.wait();
}
