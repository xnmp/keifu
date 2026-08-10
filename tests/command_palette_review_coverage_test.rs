#![cfg(not(windows))]

mod common;

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
use serde_json::Value;

use common::{init_repo, Seed};
use keifu::app::App;
use keifu::config::GraphRenderer;
use keifu::ui::{command_palette::CommandPaletteWidget, theme::Theme};

fn palette_screen(app: &App, query: &str) -> String {
    let results = app.palette_results(query);
    let area = Rect::new(0, 0, 100, 20);
    let mut buffer = Buffer::empty(area);
    CommandPaletteWidget::new(query, &results.items, results.more, 0, &Theme::dark())
        .render(area, &mut buffer);
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn every_named_palette_setting_renders_its_live_value() {
    let (_repo_dir, git_repo) = init_repo(Seed::TrackedFile);
    let mut app = App::from_repo(git_repo).unwrap();
    app.config.ui.graph_renderer = GraphRenderer::Auto;
    app.diff_word_wrap = false;
    app.commit_detail_word_wrap = true;
    app.hide_files_pane = false;
    app.hide_commit_pane = true;
    app.status_bar_visible = true;

    for (query, label, value) in [
        ("graph renderer", "Cycle Graph renderer", "auto"),
        ("diff line wrap", "Toggle Diff line wrap", "Off"),
        (
            "commit detail line wrap",
            "Toggle Commit detail line wrap",
            "On",
        ),
        ("hide files pane", "Toggle Hide files pane", "Off"),
        ("hide commit pane", "Toggle Hide commit pane", "On"),
        ("show status bar", "Toggle Show status bar", "On"),
    ] {
        let screen = palette_screen(&app, query);
        assert!(
            screen
                .lines()
                .any(|line| line.contains(label) && line.contains(value)),
            "palette omitted {label:?} with value {value:?}:\n{screen}"
        );
    }
}

fn run_git(repo: &Path, args: &[&str]) {
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
    reader_thread: thread::JoinHandle<()>,
}

impl RunningApp {
    fn wait(mut self) {
        self.child.wait().unwrap();
        drop(self.master);
        self.reader_thread.join().unwrap();
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
        reader_thread,
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
fn debug_state_reports_the_checked_out_branch_upstream() {
    let repo_dir = tempfile::tempdir().unwrap();
    run_git(repo_dir.path(), &["init", "-q", "-b", "main"]);
    run_git(repo_dir.path(), &["config", "user.name", "Palette Test"]);
    run_git(
        repo_dir.path(),
        &["config", "user.email", "palette@example.test"],
    );
    std::fs::write(repo_dir.path().join("README.md"), "fixture\n").unwrap();
    run_git(repo_dir.path(), &["add", "README.md"]);
    run_git(repo_dir.path(), &["commit", "-qm", "fixture"]);
    let remote_dir = tempfile::tempdir().unwrap();
    run_git(remote_dir.path(), &["init", "--bare", "-q"]);
    run_git(
        repo_dir.path(),
        &[
            "remote",
            "add",
            "origin",
            remote_dir.path().to_str().unwrap(),
        ],
    );
    run_git(repo_dir.path(), &["push", "-qu", "origin", "main"]);

    let port = reserve_port();
    let app = start_app(repo_dir.path(), port);
    let state = request(port, r#"{"cmd":"state"}"#);
    assert_eq!(state["head"], "main");
    assert_eq!(state["head_upstream"], "origin/main");
    assert_eq!(
        request(port, r#"{"cmd":"keys","keys":"<c-q>"}"#)["ok"],
        true
    );
    app.wait();
}
