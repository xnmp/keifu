#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

use keifu::action::Action;
use keifu::app::{App, AppMode, ConfirmAction, FocusedPanel};
use keifu::debug_server::{handle_request, DebugRequest};
use keifu::git::{operations::PullMode, GitRepository};
use keifu::keybindings::map_active_network_key;
use keifu::network::{CancellationReason, NetworkOperation, NetworkPhase, PushSpec};
use keifu::ui::{status_bar::StatusBar, theme::Theme};

const DETACHED_HELPER_PID_ENV: &str = "KEIFU_TEST_DETACHED_HELPER_PID";
const DETACHED_HELPER_PARENT_EXIT_ENV: &str = "KEIFU_TEST_DETACHED_HELPER_PARENT_EXIT";

/// Process-group member that launches the detached pipe owner, stays alive
/// long enough for production ownership discovery, and then exits first.
#[test]
#[ignore = "subprocess entrypoint; invoked by the lifecycle test"]
fn inherited_transport_parent_entrypoint() {
    let Ok(helper_pid_log) = std::env::var(DETACHED_HELPER_PID_ENV) else {
        return;
    };
    let parent_exit_log = std::env::var(DETACHED_HELPER_PARENT_EXIT_ENV).unwrap();
    let mut helper = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "inherited_transport_helper_entrypoint",
        ])
        .env(DETACHED_HELPER_PID_ENV, helper_pid_log)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    thread::spawn(move || {
        let _ = helper.wait();
    });
    eprint!("remote: Counting objects: 50% (1/2)\r");
    thread::sleep(Duration::from_millis(600));
    std::fs::write(parent_exit_log, "exited").unwrap();
}

/// Subprocess entrypoint for the inherited-pipe shutdown regression. Spawning
/// the current Rust test binary avoids relying on the Linux-only `setsid`
/// command: POSIX `setsid(2)` is available on every Unix target we support.
#[test]
#[ignore = "subprocess entrypoint; invoked by the lifecycle test"]
fn inherited_transport_helper_entrypoint() {
    let Ok(pid_log) = std::env::var(DETACHED_HELPER_PID_ENV) else {
        return;
    };
    // SAFETY: setsid changes only this subprocess session. Restoring default
    // signal handlers makes the deliberately detached helper terminable even
    // when its parent shell started it as a background job.
    unsafe {
        assert_ne!(libc::setsid(), -1, "setsid failed");
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
    }
    std::fs::write(pid_log, std::process::id().to_string()).unwrap();
    thread::sleep(Duration::from_secs(8));
}

fn status_bar_text(app: &App) -> String {
    let area = Rect::new(0, 0, 160, 1);
    let mut buffer = Buffer::empty(area);
    StatusBar::new(app, &Theme::dark()).render(area, &mut buffer);
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

fn git(cwd: &std::path::Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

fn start_blocked_integrating_pull() -> (
    tempfile::TempDir,
    App,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let root = tempfile::tempdir().unwrap();
    let remote = root.path().join("remote.git");
    let local = root.path().join("local");
    let peer = root.path().join("peer");
    let release = root.path().join("release-integration");
    let entered = root.path().join("integration-entered");
    git(
        root.path(),
        &["init", "--bare", "-q", remote.to_str().unwrap()],
    );
    git(
        root.path(),
        &["init", "-q", "-b", "main", local.to_str().unwrap()],
    );
    git(&local, &["config", "user.email", "test@example.com"]);
    git(&local, &["config", "user.name", "Test"]);
    std::fs::write(local.join("file.txt"), "initial\n").unwrap();
    git(&local, &["add", "file.txt"]);
    git(&local, &["commit", "-qm", "initial"]);
    git(
        &local,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&local, &["push", "-qu", "origin", "main"]);
    git(
        root.path(),
        &[
            "clone",
            "-q",
            "--branch",
            "main",
            remote.to_str().unwrap(),
            peer.to_str().unwrap(),
        ],
    );
    git(&peer, &["config", "user.email", "test@example.com"]);
    git(&peer, &["config", "user.name", "Test"]);
    std::fs::write(peer.join("file.txt"), "advanced\n").unwrap();
    git(&peer, &["commit", "-qam", "advance remote"]);
    git(&peer, &["push", "-q", "origin", "main"]);

    let hook = local.join(".git/hooks/post-merge");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\nprintf ready > \"{}\"\ni=0\nwhile [ ! -f \"{}\" ] && [ \"$i\" -lt 500 ]; do i=$((i + 1)); sleep 0.01; done\n",
            entered.display(),
            release.display()
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&hook, permissions).unwrap();

    let mut app = App::from_repo(GitRepository::open(&local).unwrap()).unwrap();
    app.network.start_pull(
        local.to_str().unwrap(),
        Some("origin".to_string()),
        Some("main".to_string()),
        PullMode::FfOnly,
        None,
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.network_status().map(|status| status.phase) != Some(NetworkPhase::Integrating)
        || !entered.exists()
    {
        assert!(
            Instant::now() < deadline,
            "pull never blocked in integration"
        );
        app.update_network_state();
        thread::sleep(Duration::from_millis(10));
    }
    (root, app, local, release)
}

#[test]
fn running_job_exposes_cancel_hint_and_cancelling_state_in_the_status_bar() {
    let mut app = App::test_fixture();
    for (operation, label) in [
        (NetworkOperation::Fetch, "Fetching…"),
        (NetworkOperation::Pull, "Pulling…"),
        (NetworkOperation::Push, "Pushing…"),
    ] {
        app.network.activate_for_test(operation, Instant::now());
        let running = status_bar_text(&app);
        assert!(running.contains(label), "status bar: {running:?}");
        assert!(running.contains("x cancel"), "status bar: {running:?}");
    }

    app.network
        .activate_for_test(NetworkOperation::Fetch, Instant::now());

    let action = map_active_network_key(
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        &AppMode::Normal,
        FocusedPanel::Graph,
        false,
        false,
        false,
        true,
    );
    assert_eq!(action, Some(Action::CancelNetworkOperation));
    app.handle_action(action.unwrap()).unwrap();

    let state = handle_request(&mut app, 160, 30, DebugRequest::State);
    assert_eq!(state["network_operation"], "fetch");
    assert_eq!(state["network_phase"], "cancelling");
    let dump = handle_request(
        &mut app,
        160,
        30,
        DebugRequest::Dump {
            width: Some(160),
            height: Some(30),
        },
    );
    let cancelling = dump["screen"].as_str().unwrap();
    assert!(
        cancelling.contains("Cancelling…"),
        "status bar: {cancelling:?}"
    );
    assert_eq!(
        app.network.status().unwrap().phase,
        NetworkPhase::Cancelling(CancellationReason::User)
    );
}

#[test]
fn each_cancellation_completion_toasts_once_and_allows_a_production_restart() {
    for (operation, label) in [
        (NetworkOperation::Fetch, "Fetch"),
        (NetworkOperation::Pull, "Pull"),
        (NetworkOperation::Push, "Push"),
    ] {
        let mut app = App::test_fixture();
        app.network.activate_for_test(operation, Instant::now());
        app.handle_action(Action::CancelNetworkOperation).unwrap();
        app.network
            .finish_cancelled_for_test(CancellationReason::User);

        let completed = match operation {
            NetworkOperation::Fetch => app.update_fetch_status(),
            NetworkOperation::Pull => app.update_pull_status(),
            NetworkOperation::Push => app.update_push_status(),
        };
        assert!(completed);
        assert!(!app.is_network_busy());
        let expected_toast = format!("{label} cancelled");
        assert_eq!(
            app.toasts
                .visible()
                .iter()
                .filter(|toast| toast.text == expected_toast)
                .count(),
            1
        );
        let completed_again = match operation {
            NetworkOperation::Fetch => app.update_fetch_status(),
            NetworkOperation::Pull => app.update_pull_status(),
            NetworkOperation::Push => app.update_push_status(),
        };
        assert!(!completed_again);
        assert_eq!(
            app.toasts
                .visible()
                .iter()
                .filter(|toast| toast.text == expected_toast)
                .count(),
            1,
            "polling after {label} completion must not report the outcome twice"
        );

        let repo_path = app.repo_path.clone();
        match operation {
            NetworkOperation::Fetch => {
                app.network
                    .start_fetch(&repo_path, "origin", false, false, None);
            }
            NetworkOperation::Pull => {
                app.network
                    .start_pull(&repo_path, None, None, PullMode::FfOnly, None);
            }
            NetworkOperation::Push => {
                app.network.start_push(&repo_path, PushSpec::Current, None);
            }
        }
        assert_eq!(
            app.network_status().map(|status| status.operation),
            Some(operation),
            "the production {label} path can start after cancellation"
        );
    }
}

#[test]
fn inactivity_timeout_stays_visible_until_the_worker_returns_then_toasts() {
    let mut app = App::test_fixture();
    app.network.activate_for_test(
        NetworkOperation::Pull,
        Instant::now() - Duration::from_secs(60),
    );

    assert!(app.update_network_state());
    assert!(status_bar_text(&app).contains("Cancelling…"));
    assert!(app.is_network_busy());

    app.network
        .finish_cancelled_for_test(CancellationReason::InactivityTimeout);
    assert!(app.update_pull_status());
    assert!(!app.is_network_busy());
    assert!(app
        .toasts
        .visible()
        .iter()
        .any(|toast| { toast.text == "Pull timed out after 60 seconds without progress" }));
}

#[test]
fn normal_and_forced_quit_wait_for_a_real_inherited_helper_transport() {
    for action in [Action::Quit, Action::ForceQuit] {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let helper = root.path().join("remote-helper");
        let helper_pid_log = root.path().join("helper.pid");
        let helper_parent_exit_log = root.path().join("helper-parent-exited");
        let test_executable = std::env::current_exe().unwrap();
        git(root.path(), &["init", "-q", repo.to_str().unwrap()]);
        git(&repo, &["config", "protocol.ext.allow", "always"]);
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\n\
                 export {}=\"{}\"\n\
                 export {}=\"{}\"\n\
                 exec \"{}\" --ignored --exact inherited_transport_parent_entrypoint\n",
                DETACHED_HELPER_PID_ENV,
                helper_pid_log.display(),
                DETACHED_HELPER_PARENT_EXIT_ENV,
                helper_parent_exit_log.display(),
                test_executable.display()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&helper).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&helper, permissions).unwrap();

        let mut app = App::from_repo(GitRepository::open(&repo).unwrap()).unwrap();
        let remote = format!("ext::{}", helper.display());
        app.network
            .start_fetch(&app.repo_path.clone(), &remote, false, false, None);
        let start_deadline = Instant::now() + Duration::from_secs(5);
        while !helper_pid_log.exists() {
            assert!(
                Instant::now() < start_deadline,
                "transport helper never started"
            );
            app.update_network_state();
            thread::sleep(Duration::from_millis(10));
        }
        while !helper_parent_exit_log.exists() {
            assert!(
                Instant::now() < start_deadline,
                "transport helper parent never exited"
            );
            app.update_network_state();
            thread::sleep(Duration::from_millis(10));
        }
        app.handle_action(action.clone()).unwrap();

        assert!(!app.should_quit, "quit bypassed the active transport");
        assert!(app.shutdown_after_network);
        assert!(app.is_network_busy());
        let completion_deadline = Instant::now() + Duration::from_secs(8);
        while !app.should_quit {
            app.update_network_state();
            app.update_fetch_status();
            app.update_shutdown_state();
            assert!(
                Instant::now() < completion_deadline,
                "transport shutdown did not finish"
            );
            thread::sleep(Duration::from_millis(10));
        }

        assert!(!app.is_network_busy());
        let helper_pid = std::fs::read_to_string(&helper_pid_log).unwrap();
        assert!(
            !Command::new("kill")
                .args(["-0", helper_pid.trim()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success(),
            "quit returned before the inherited helper exited"
        );
        assert_eq!(
            app.repo.operation_state(),
            keifu::git::OperationState::Clean
        );
    }
}

#[test]
fn normal_and_forced_quit_wait_for_real_pull_integration() {
    for action in [Action::Quit, Action::ForceQuit] {
        let (_root, mut app, local, release) = start_blocked_integrating_pull();

        app.handle_action(action.clone()).unwrap();

        assert!(!app.should_quit, "quit bypassed pull integration");
        assert!(app.shutdown_after_network);
        assert_eq!(
            app.network_status().map(|status| status.phase),
            Some(NetworkPhase::Integrating)
        );
        std::fs::write(&release, "release\n").unwrap();
        let completion_deadline = Instant::now() + Duration::from_secs(5);
        while !app.should_quit {
            app.update_network_state();
            app.update_pull_status();
            app.update_shutdown_state();
            assert!(
                Instant::now() < completion_deadline,
                "pull integration shutdown did not finish"
            );
            thread::sleep(Duration::from_millis(10));
        }

        assert!(!app.is_network_busy());
        assert_eq!(
            app.repo.operation_state(),
            keifu::git::OperationState::Clean
        );
        assert_eq!(
            std::fs::read_to_string(local.join("file.txt")).unwrap(),
            "advanced\n"
        );
    }
}

#[test]
fn integrating_pull_blocks_checkout_through_the_real_manager_and_app_seam() {
    fn git(cwd: &std::path::Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(cwd)
                .status()
                .unwrap()
                .success(),
            "git {args:?} failed"
        );
    }

    let root = tempfile::tempdir().unwrap();
    let remote = root.path().join("remote.git");
    let local = root.path().join("local");
    let peer = root.path().join("peer");
    git(
        root.path(),
        &["init", "--bare", "-q", remote.to_str().unwrap()],
    );
    git(
        root.path(),
        &["init", "-q", "-b", "main", local.to_str().unwrap()],
    );
    git(&local, &["config", "user.email", "test@example.com"]);
    git(&local, &["config", "user.name", "Test"]);
    std::fs::write(local.join("file.txt"), "initial\n").unwrap();
    git(&local, &["add", "file.txt"]);
    git(&local, &["commit", "-qm", "initial"]);
    git(
        &local,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&local, &["push", "-qu", "origin", "main"]);
    git(
        root.path(),
        &[
            "clone",
            "-q",
            "--branch",
            "main",
            remote.to_str().unwrap(),
            peer.to_str().unwrap(),
        ],
    );
    git(&peer, &["config", "user.email", "test@example.com"]);
    git(&peer, &["config", "user.name", "Test"]);
    std::fs::write(peer.join("file.txt"), "advanced\n").unwrap();
    git(&peer, &["commit", "-qam", "advance remote"]);
    git(&peer, &["push", "-q", "origin", "main"]);
    git(&local, &["branch", "other"]);

    let hook = local.join(".git/hooks/post-merge");
    std::fs::write(&hook, "#!/bin/sh\nsleep 1\n").unwrap();
    let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&hook, permissions).unwrap();

    let mut app = App::from_repo(GitRepository::open(&local).unwrap()).unwrap();
    let original_head = app.repo.head_oid();
    let original_name = app.repo.head_name();
    app.network.start_pull(
        local.to_str().unwrap(),
        Some("origin".to_string()),
        Some("main".to_string()),
        PullMode::FfOnly,
        None,
    );
    let integration_deadline = Instant::now() + Duration::from_secs(5);
    while app.network_status().map(|status| status.phase) != Some(NetworkPhase::Integrating) {
        assert!(
            Instant::now() < integration_deadline,
            "pull never entered integration"
        );
        app.update_network_state();
        thread::sleep(Duration::from_millis(10));
    }
    app.mode = AppMode::Confirm {
        message: "Checkout branch 'other'?".to_string(),
        action: ConfirmAction::Checkout {
            name: "other".to_string(),
            is_remote: false,
        },
    };

    app.handle_action(Action::Confirm).unwrap();

    assert_eq!(app.repo.head_name(), original_name);
    assert!(matches!(app.mode, AppMode::Confirm { .. }));
    assert!(app.toasts.visible().iter().any(|toast| {
        toast.text == "Pull integration in progress; wait before changing the repository"
    }));

    app.mode = AppMode::Normal;
    let completion_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.update_network_state();
        if app.update_pull_status() {
            break;
        }
        assert!(Instant::now() < completion_deadline, "pull never completed");
        thread::sleep(Duration::from_millis(10));
    }
    assert_ne!(app.repo.head_oid(), original_head);
    assert_eq!(app.repo.head_name().as_deref(), Some("main"));
    assert_eq!(
        std::fs::read_to_string(local.join("file.txt")).unwrap(),
        "advanced\n"
    );
    assert_eq!(
        app.repo.operation_state(),
        keifu::git::OperationState::Clean
    );
}
