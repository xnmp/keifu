//! Regression coverage for scrolling the help popup in a short terminal.

mod common;

use keifu::{action::Action, app::App, ui};
use ratatui::{backend::TestBackend, Terminal};

#[test]
fn end_reveals_the_last_help_entry_in_a_short_terminal() {
    let (_tmp, repo) = common::init_repo(common::Seed::TrackedFile);
    let mut app = App::from_repo(repo).unwrap();
    let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();

    app.handle_action(Action::ToggleHelp).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    assert!(app.help_max_scroll > 0, "the short popup must overflow");

    app.handle_action(Action::HelpScrollToBottom).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let screen = terminal
        .backend()
        .buffer()
        .content
        .chunks(60)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("Ctrl+Q") && screen.contains("anywhere)"));
}
