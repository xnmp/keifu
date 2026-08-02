//! Regression coverage for scrolling the help popup in a short terminal.

mod common;

use keifu::{action::Action, app::App, ui};
use ratatui::{backend::TestBackend, layout::Rect, Terminal};

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
fn end_reveals_the_last_help_entry_in_a_short_terminal() {
    let (_tmp, repo) = common::init_repo(common::Seed::TrackedFile);
    let mut app = App::from_repo(repo).unwrap();
    let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();

    app.handle_action(Action::ToggleHelp).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    assert!(app.help_max_scroll > 0, "the short popup must overflow");

    app.handle_action(Action::HelpScrollToBottom).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let rendered = screen(&terminal);
    assert!(rendered.contains("Ctrl+Q") && rendered.contains("anywhere)"));
}

#[test]
fn draw_clamps_help_scroll_after_context_change_and_resize() {
    let (tmp, repo) = common::init_repo(common::Seed::TrackedFile);
    std::fs::write(tmp.path().join("tracked.txt"), "modified\n").unwrap();
    let mut app = App::from_repo(repo).unwrap();
    let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();

    // The initial uncommitted selection produces the longer context-specific
    // sheet. Draw establishes the real geometry before scrolling to its end.
    assert!(app.is_uncommitted_selected());
    app.handle_action(Action::ToggleHelp).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    app.handle_action(Action::HelpScrollToBottom).unwrap();
    let uncommitted_max = app.help_max_scroll;

    // Select a committed row, reopen help, and let draw recompute its shorter
    // content range rather than assigning/clamping state in the test.
    app.handle_action(Action::ToggleHelp).unwrap();
    app.handle_action(Action::MoveDown).unwrap();
    assert!(!app.is_uncommitted_selected());
    app.handle_action(Action::ToggleHelp).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    assert!(app.help_max_scroll < uncommitted_max);
    assert_eq!(app.help_scroll, app.help_max_scroll);
    assert!(screen(&terminal).contains("Ctrl+Q"));
    let committed_max = app.help_max_scroll;

    // A taller terminal further reduces the maximum; draw clamps the old
    // position and still renders the final entry at the new viewport bottom.
    terminal.backend_mut().resize(60, 80);
    terminal.resize(Rect::new(0, 0, 60, 80)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    assert!(app.help_max_scroll < committed_max);
    assert_eq!(app.help_scroll, app.help_max_scroll);
    assert!(screen(&terminal).contains("Ctrl+Q"));
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
