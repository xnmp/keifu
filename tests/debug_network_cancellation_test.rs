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
use keifu::network::{CancellationReason, NetworkOperation, NetworkPhase};
use keifu::ui::{status_bar::StatusBar, theme::Theme};

fn status_bar_text(app: &App) -> String {
    let area = Rect::new(0, 0, 160, 1);
    let mut buffer = Buffer::empty(area);
    StatusBar::new(app, &Theme::dark()).render(area, &mut buffer);
    buffer.content.iter().map(|cell| cell.symbol()).collect()
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
        app.network
            .start_fetch(&repo_path, "origin", false, false, None);
        assert!(
            app.is_fetching(),
            "a production network operation can start after {label} cancellation"
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
