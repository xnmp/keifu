use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use keifu::app::{AppMode, FocusedPanel};
use keifu::{action::Action, app::App, keybindings::map_key_to_action, ui};
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
}
