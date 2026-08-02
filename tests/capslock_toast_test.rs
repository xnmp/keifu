use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use keifu::{app::App, git::repository::GitRepository};

fn test_app() -> (tempfile::TempDir, App) {
    let tempdir = tempfile::tempdir().unwrap();
    git2::Repository::init(tempdir.path()).unwrap();
    let repo = GitRepository::open(tempdir.path()).unwrap();
    let app = App::from_repo(repo).unwrap();
    (tempdir, app)
}

fn key_with_state(state: KeyEventState) -> KeyEvent {
    KeyEvent::new_with_kind_and_state(
        KeyCode::Char('k'),
        KeyModifiers::NONE,
        KeyEventKind::Press,
        state,
    )
}

#[test]
fn capslock_warning_is_once_per_reported_session_and_rearms_when_inactive() {
    let (_tmp, mut app) = test_app();

    app.maybe_hint_capslock(&key_with_state(KeyEventState::CAPS_LOCK));
    assert_eq!(app.toasts.visible().len(), 1, "first reported key warns");

    app.maybe_hint_capslock(&key_with_state(KeyEventState::CAPS_LOCK));
    assert_eq!(
        app.toasts.visible().len(),
        1,
        "a continuing reported session is suppressed"
    );

    app.maybe_hint_capslock(&key_with_state(KeyEventState::empty()));
    app.maybe_hint_capslock(&key_with_state(KeyEventState::CAPS_LOCK));
    assert_eq!(
        app.toasts.visible().len(),
        2,
        "an inactive key re-arms the next reported session"
    );
}

#[test]
fn absent_capslock_state_does_not_infer_a_warning_from_uppercase_text() {
    let (_tmp, mut app) = test_app();
    let unreported = KeyEvent::new(KeyCode::Char('K'), KeyModifiers::NONE);

    app.maybe_hint_capslock(&unreported);

    assert!(app.toasts.is_empty());
}
