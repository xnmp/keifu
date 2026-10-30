//! #201: the new-issue composer must remind users that Ctrl+Enter submits,
//! in both the popup hint row and the status bar.

use keifu::app::{App, AppMode, IssueComposePurpose, IssueCreateTarget};
use keifu::ui;
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
fn new_issue_composer_shows_ctrl_enter_submit_in_popup_and_status_bar() {
    let mut app = App::test_fixture();
    app.mode = AppMode::IssueCompose {
        purpose: IssueComposePurpose::NewIssue {
            target: IssueCreateTarget::CurrentRepository,
        },
    };

    let screen = rendered_screen(&mut app, 120, 40);

    assert!(
        screen.contains("Ctrl+S / Ctrl+Enter submit  Esc cancel  Ctrl+E editor  Tab image"),
        "the popup hint row lists every compose control: {screen}"
    );
    assert!(
        screen
            .lines()
            .last()
            .unwrap()
            .contains("Ctrl+S/Ctrl+Enter submit"),
        "the status bar names Ctrl+Enter as a submit shortcut: {screen}"
    );
}
