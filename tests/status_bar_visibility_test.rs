use keifu::{
    action::Action,
    app::App,
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
fn help_menu_exposes_the_current_status_bar_visibility() {
    let mut app = App::test_fixture();
    app.handle_action(Action::ToggleHelp).unwrap();

    let screen = rendered_screen(&mut app, 120, 40);

    assert!(
        screen.contains("Toggle status bar (On)"),
        "the Help menu must expose the currently enabled status-bar control: {screen}"
    );
}
