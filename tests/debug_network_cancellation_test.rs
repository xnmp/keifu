#![cfg(unix)]

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

use keifu::action::Action;
use keifu::app::{App, AppMode, ConfirmAction, FocusedPanel};
use keifu::debug_server::{handle_request, DebugRequest};
use keifu::git::GitRepository;
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
fn cancellation_completion_toasts_once_releases_busy_and_allows_a_later_op() {
    let mut app = App::test_fixture();
    app.network
        .activate_for_test(NetworkOperation::Fetch, Instant::now());
    app.handle_action(Action::CancelNetworkOperation).unwrap();
    app.network
        .finish_cancelled_for_test(CancellationReason::User);

    assert!(app.update_fetch_status());
    assert!(!app.is_network_busy());
    let cancellation_toasts = app
        .toasts
        .visible()
        .iter()
        .filter(|toast| toast.text == "Fetch cancelled")
        .count();
    assert_eq!(cancellation_toasts, 1);
    assert!(!app.update_fetch_status());
    assert_eq!(
        app.toasts
            .visible()
            .iter()
            .filter(|toast| toast.text == "Fetch cancelled")
            .count(),
        1,
        "polling after completion must not report the outcome twice"
    );

    app.network
        .activate_for_test(NetworkOperation::Push, Instant::now());
    assert!(app.is_pushing(), "a later network operation can start");
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
fn integrating_pull_blocks_checkout_through_the_app_action_seam() {
    let repo_dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "Test"],
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(repo_dir.path())
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(repo_dir.path().join("file.txt"), "initial").unwrap();
    assert!(std::process::Command::new("git")
        .args(["add", "file.txt"])
        .current_dir(repo_dir.path())
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("git")
        .args(["commit", "-qm", "initial"])
        .current_dir(repo_dir.path())
        .status()
        .unwrap()
        .success());
    let mut app = App::from_repo(GitRepository::open(repo_dir.path()).unwrap()).unwrap();
    let original_head = app.repo.head_oid();
    let original_name = app.repo.head_name();
    assert!(std::process::Command::new("git")
        .args(["branch", "other"])
        .current_dir(&app.repo_path)
        .status()
        .unwrap()
        .success());
    app.network.activate_integrating_pull_for_test();
    app.mode = AppMode::Confirm {
        message: "Checkout branch 'other'?".to_string(),
        action: ConfirmAction::Checkout {
            name: "other".to_string(),
            is_remote: false,
        },
    };

    app.handle_action(Action::Confirm).unwrap();

    assert_eq!(app.repo.head_oid(), original_head);
    assert_eq!(app.repo.head_name(), original_name);
    assert!(matches!(app.mode, AppMode::Confirm { .. }));
    assert!(app.toasts.visible().iter().any(|toast| {
        toast.text == "Pull integration in progress; wait before changing the repository"
    }));
}
