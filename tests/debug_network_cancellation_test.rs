use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

use keifu::action::Action;
use keifu::app::{App, AppMode, FocusedPanel};
use keifu::keybindings::map_key_to_action;
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
    app.network
        .activate_for_test(NetworkOperation::Fetch, Instant::now());

    let running = status_bar_text(&app);
    assert!(running.contains("Fetching…"), "status bar: {running:?}");
    assert!(running.contains("x cancel"), "status bar: {running:?}");

    let action = map_key_to_action(
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        &AppMode::Normal,
        FocusedPanel::Graph,
        false,
        false,
        false,
    );
    assert_eq!(action, Some(Action::CancelNetworkOperation));
    app.handle_action(action.unwrap()).unwrap();

    let cancelling = status_bar_text(&app);
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
