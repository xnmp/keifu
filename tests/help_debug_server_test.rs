use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

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

fn start_app(repo: &Path, port: u16) -> Child {
    let binary = env!("CARGO_BIN_EXE_keifu");
    let command = format!(
        "'{}' --debug-listen 127.0.0.1:{port}",
        binary.replace('\'', "'\\''")
    );
    Command::new("script")
        .args(["-qec", &command, "/dev/null"])
        .current_dir(repo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
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
    let mut app = start_app(repo, port);
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
    let dump = request(port, r#"{"cmd":"dump","width":140,"height":300}"#);
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
    app.wait().unwrap();
    screen
}

#[test]
fn debug_server_renders_updated_help_for_clean_and_uncommitted_repositories() {
    let clean = fixture_repo(false);
    help_screen(clean.path(), false);

    let uncommitted = fixture_repo(true);
    help_screen(uncommitted.path(), true);
}
