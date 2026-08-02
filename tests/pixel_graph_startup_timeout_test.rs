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
    let status = Command::new("git").args(args).current_dir(repo).status().unwrap();
    assert!(status.success(), "git command failed: git {args:?}");
}

fn fixture_repo() -> TempDir {
    let repo = tempfile::tempdir().unwrap();
    run_git(repo.path(), &["init", "-q"]);
    run_git(repo.path(), &["config", "user.name", "Pixel Test"]);
    run_git(repo.path(), &["config", "user.email", "pixel@example.test"]);
    std::fs::write(repo.path().join("README.md"), "fixture\n").unwrap();
    run_git(repo.path(), &["add", "README.md"]);
    run_git(repo.path(), &["commit", "-qm", "fixture"]);
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
    _config_home: TempDir,
    reader_thread: thread::JoinHandle<()>,
}

impl RunningApp {
    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        drop(self._master);
        self.reader_thread.join().unwrap();
    }
}

fn start_app(repo: &Path, port: u16, tmux: bool, answer_query: bool) -> RunningApp {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 80,
            cols: 140,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let config_home = tempfile::tempdir().unwrap();
    let address = format!("127.0.0.1:{port}");
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_keifu"));
    command.args(["--debug-listen", &address]);
    command.cwd(repo);
    command.env("XDG_CONFIG_HOME", config_home.path());
    command.env("TERM", if tmux { "tmux-256color" } else { "xterm-256color" });
    for variable in [
        "KEIFU_FORCE_PIXEL",
        "TERM_PROGRAM",
        "KITTY_WINDOW_ID",
        "ITERM_SESSION_ID",
        "WEZTERM_EXECUTABLE",
    ] {
        command.env_remove(variable);
    }
    let child = pty.slave.spawn_command(command).unwrap();
    drop(pty.slave);

    let mut reader = pty.master.try_clone_reader().unwrap();
    let mut writer = pty.master.take_writer().unwrap();
    let reader_thread = thread::spawn(move || {
        let mut output = Vec::new();
        let mut buf = [0; 8192];
        let mut answered_picker = false;
        let mut answered_keyboard = false;
        let mut answered_background = false;
        while let Ok(read) = reader.read(&mut buf) {
            if read == 0 {
                break;
            }
            output.extend_from_slice(&buf[..read]);
            if !answered_keyboard && output.windows(6).any(|part| part == b"\x1b[?u\x1b[") {
                // Crossterm's keyboard-enhancement probe has its own two
                // second timeout; report ordinary terminal attributes so the
                // startup measurement isolates Picker's query.
                writer.write_all(b"\x1b[?1;2c").unwrap();
                writer.flush().unwrap();
                answered_keyboard = true;
            }
            if !answered_background && output.windows(5).any(|part| part == b"]11;?") {
                writer.write_all(b"\x1b]11;rgb:0000/0000/0000\x07\x1b[0n").unwrap();
                writer.flush().unwrap();
                answered_background = true;
            }
            if answer_query
                && !answered_picker
                && output.windows(5).any(|part| part == b"_Gi=3")
            {
                // Kitty graphics, a nonzero cell size, then the terminal-status
                // response which completes the real Picker stdio query.
                writer
                    .write_all(b"\x1b_Gi=31;OK\x1b\\\x1b[6;10;20t\x1b[0n")
                    .unwrap();
                writer.flush().unwrap();
                answered_picker = true;
            }
        }
    });
    RunningApp {
        child,
        _master: pty.master,
        _config_home: config_home,
        reader_thread,
    }
}

fn request(port: u16, body: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut stream = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => break stream,
            Err(error) if Instant::now() < deadline => {
                assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
                thread::sleep(Duration::from_millis(10));
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

fn startup_state(tmux: bool, answer_query: bool) -> (Duration, Value, RunningApp) {
    let repo = fixture_repo();
    let port = reserve_port();
    let started = Instant::now();
    let app = start_app(repo.path(), port, tmux, answer_query);
    // A state request is handled only after the real terminal has rendered its
    // first frame, so its reply proves startup reached the event loop.
    let state = request(port, r#"{"cmd":"state"}"#);
    (started.elapsed(), state, app)
}

#[test]
fn unanswered_startup_query_reaches_unicode_first_frame_within_the_bound() {
    let (elapsed, state, app) = startup_state(false, false);
    app.stop();

    assert!(
        elapsed < Duration::from_millis(1500),
        "an unanswered Picker query delayed the first frame for {elapsed:?}"
    );
    assert_eq!(state["ok"], true);
    assert_eq!(state["pixel_graph_active"], false, "falls back to Unicode");
}

#[test]
fn responsive_startup_query_keeps_the_pixel_graph_active() {
    let (_elapsed, state, app) = startup_state(false, true);
    app.stop();

    assert_eq!(state["ok"], true);
    assert_eq!(state["pixel_graph_active"], true);
}

#[test]
fn tmux_without_a_response_keeps_its_unicode_degradation_and_bound() {
    let (elapsed, state, app) = startup_state(true, false);
    app.stop();

    assert!(
        elapsed < Duration::from_millis(1500),
        "tmux fallback delayed the first frame for {elapsed:?}"
    );
    assert_eq!(state["ok"], true);
    assert_eq!(state["pixel_graph_active"], false);
}
