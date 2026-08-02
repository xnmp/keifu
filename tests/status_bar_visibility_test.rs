use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use keifu::app::{AppMode, FocusedPanel};
use keifu::{
    action::Action, app::App, config::UiState, git::GitRepository, keybindings::map_key_to_action,
    ui,
};
use ratatui::{backend::TestBackend, Terminal};

fn rendered_screen(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| ui::draw(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(width as usize)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn help_menu_toggles_status_bar_and_returns_its_row_to_the_main_layout() {
    let mut app = App::test_fixture();
    let initial = rendered_screen(&mut app, 120, 40);
    assert!(
        initial.lines().last().unwrap().contains("help"),
        "the default screen must render the status-bar hints: {initial}"
    );
    assert!(
        initial.lines().nth(38).unwrap().contains('╰'),
        "with the status bar visible, the main panes end one row above it: {initial}"
    );

    app.handle_action(Action::ToggleHelp).unwrap();

    let help = rendered_screen(&mut app, 120, 100);

    assert!(
        help.contains("Toggle status bar (On)"),
        "the Help menu must expose the currently enabled status-bar control: {help}"
    );

    let toggle = map_key_to_action(
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        &AppMode::Help,
        FocusedPanel::Graph,
        false,
        false,
        false,
    );
    assert_eq!(toggle, Some(Action::ToggleStatusBar));
    app.handle_action(toggle.unwrap()).unwrap();

    let hidden_help = rendered_screen(&mut app, 120, 100);
    assert!(
        hidden_help.contains("Toggle status bar (Off)"),
        "the Help menu must report the updated control state: {hidden_help}"
    );

    app.handle_action(Action::ToggleHelp).unwrap();
    let hidden = rendered_screen(&mut app, 120, 40);
    assert!(
        !hidden.lines().last().unwrap().contains("help"),
        "hiding the status bar must return its bottom row to the main interface: {hidden}"
    );
    assert!(
        hidden.lines().last().unwrap().contains('╰'),
        "the main pane border must occupy the reclaimed final row: {hidden}"
    );
}

#[test]
fn help_toggle_persists_to_a_fresh_ui_state_and_app() {
    let config_dir = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    git2::Repository::init(repo_dir.path()).unwrap();
    let original_config_dir = std::env::var_os("XDG_CONFIG_HOME");
    std::env::set_var("XDG_CONFIG_HOME", config_dir.path());

    let repo = GitRepository::open(repo_dir.path()).unwrap();
    let mut app = App::from_repo(repo).unwrap();
    assert!(
        app.status_bar_visible,
        "new apps show the status bar by default"
    );

    app.handle_action(Action::ToggleHelp).unwrap();
    app.handle_action(Action::ToggleStatusBar).unwrap();

    let restored_state = UiState::load();
    assert!(
        !restored_state.status_bar_visible,
        "the Help toggle must be written to state.toml"
    );
    let fresh_repo = GitRepository::open(repo_dir.path()).unwrap();
    let fresh_app = App::from_repo_with_ui_state(fresh_repo, restored_state).unwrap();
    assert!(
        !fresh_app.status_bar_visible,
        "a fresh app must honor the persisted hidden status bar preference"
    );

    match original_config_dir {
        Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
        None => std::env::remove_var("XDG_CONFIG_HOME"),
    }
}
