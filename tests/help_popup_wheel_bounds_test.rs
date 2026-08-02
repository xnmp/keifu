//! Regression coverage for Help mouse-wheel routing at the rendered seam.

mod common;

use keifu::{action::Action, app::App, ui};
use ratatui::{backend::TestBackend, Terminal};

fn screen(terminal: &Terminal<TestBackend>) -> String {
    let width = terminal.size().unwrap().width as usize;
    terminal
        .backend()
        .buffer()
        .content
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn wheel_scrolls_only_inside_the_rendered_help_popup() {
    let (_tmp, repo) = common::init_repo(common::Seed::TrackedFile);
    let mut app = App::from_repo(repo).unwrap();
    let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();

    app.handle_action(Action::ToggleHelp).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let popup = app.popup_rect.expect("Help records its rendered bounds");
    let selected_before = app.graph_nav.graph_list_state.selected();

    // A wheel event outside the modal must not move either the sheet or the
    // graph behind it.
    app.handle_action(Action::MouseScroll {
        col: popup.x.saturating_sub(1),
        row: popup.y,
        down: true,
    })
    .unwrap();
    assert_eq!(app.help_scroll, 0);
    assert_eq!(app.graph_nav.graph_list_state.selected(), selected_before);

    app.handle_action(Action::MouseScroll {
        col: popup.x + 1,
        row: popup.y + 1,
        down: true,
    })
    .unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let rendered = screen(&terminal);
    assert!(
        app.help_scroll > 0,
        "wheel over Help advances its visible rows"
    );
    assert!(
        !rendered.contains("Navigation"),
        "the first help row scrolled out"
    );
    assert_eq!(app.graph_nav.graph_list_state.selected(), selected_before);

    app.handle_action(Action::ToggleHelp).unwrap();
    assert!(matches!(app.mode, keifu::app::AppMode::Normal));
}
