//! Regression coverage for issue-popup mouse row selection.

use keifu::action::Action;
use keifu::app::{
    App, AppMode, FocusedPanel, IssueDetailState, IssueDetailView, IssueLabelPicker,
    IssueListState, IssueListView, MouseLayout,
};
use keifu::issue::{IssueFilter, IssueInfo, IssueLabel, IssueState, IssueViewFilter};
use ratatui::layout::Rect;
use ratatui::{backend::TestBackend, Terminal};

fn issue(number: u64) -> IssueInfo {
    IssueInfo {
        number,
        title: format!("issue {number}"),
        state: IssueState::Open,
        labels: vec![],
        assignees: vec![],
        author: "author".into(),
        updated_at: String::new(),
        url: String::new(),
    }
}

#[test]
fn mouse_click_opens_the_visible_row_after_issue_list_windowing() {
    let mut app = App::test_fixture();
    app.issue_list = Some(IssueListView {
        state: IssueListState::Ready((1..=5).map(issue).collect()),
        selected: 4,
        filter: IssueFilter::Open,
        view_filter: IssueViewFilter::default(),
        scroll: 0,
        pending_reselect: None,
    });
    app.mode = AppMode::IssueList;
    // The inner area has a header and three body rows. Selection at index four
    // windows the list at index two, so the first visible issue is #3.
    app.popup_rect = Some(Rect::new(0, 0, 40, 6));

    app.handle_action(Action::MouseClick { col: 5, row: 2 })
        .expect("mouse action should be handled");

    assert!(matches!(app.mode, AppMode::IssueDetail));
    assert_eq!(app.issue_detail.as_ref().map(|view| view.number), Some(3));
}

#[test]
fn rendering_full_screen_issue_list_enables_clicking_its_windowed_rows() {
    let mut app = App::test_fixture();
    app.issue_list = Some(IssueListView {
        state: IssueListState::Ready((1..=5).map(issue).collect()),
        selected: 4,
        filter: IssueFilter::Open,
        view_filter: IssueViewFilter::default(),
        scroll: 0,
        pending_reselect: None,
    });
    app.mode = AppMode::IssueList;

    // A six-row terminal leaves two visible issue rows after the bordered
    // list's header. Rendering must record that full-screen content as the
    // popup hit-test target before the click arrives.
    let mut term = Terminal::new(TestBackend::new(40, 6)).expect("test terminal");
    term.draw(|frame| keifu::ui::draw(frame, &mut app))
        .expect("render issue list");

    app.handle_action(Action::MouseClick { col: 5, row: 2 })
        .expect("mouse action should be handled");

    assert!(matches!(app.mode, AppMode::IssueDetail));
    assert_eq!(app.issue_detail.as_ref().map(|view| view.number), Some(4));
}

#[test]
fn rendering_windowed_label_picker_toggles_the_displayed_label() {
    let mut app = App::test_fixture();
    app.issue_label_picker = Some(IssueLabelPicker {
        number: 1,
        labels: (1..=10)
            .map(|n| IssueLabel {
                name: format!("label {n}"),
                color: "ffffff".into(),
            })
            .collect(),
        original: vec![false; 10],
        chosen: vec![false; 10],
    });
    app.mode = AppMode::IssueLabelPicker {
        number: 1,
        selected: 7,
    };

    // The rendered picker is clipped to the terminal height: five body rows
    // fit, so selection seven windows the first visible label to index three.
    let mut term = Terminal::new(TestBackend::new(40, 8)).expect("test terminal");
    term.draw(|frame| keifu::ui::draw(frame, &mut app))
        .expect("render label picker");

    app.handle_action(Action::MouseClick { col: 5, row: 1 })
        .expect("mouse action should be handled");

    assert!(matches!(
        app.mode,
        AppMode::IssueLabelPicker {
            number: 1,
            selected: 3
        }
    ));
    assert_eq!(
        app.issue_label_picker.as_ref().unwrap().chosen,
        vec![false, false, false, true, false, false, false, false, false, false]
    );
}

#[test]
fn rendered_issue_detail_click_does_not_reach_the_graph_panel() {
    let mut app = App::test_fixture();
    app.issue_detail = Some(IssueDetailView {
        number: 39,
        state: IssueDetailState::Loading,
        scroll: 0,
        max_scroll: 0,
    });
    app.mode = AppMode::IssueDetail;
    app.focused_panel = FocusedPanel::Files;
    app.mouse_layout = MouseLayout {
        graph: Rect::new(0, 0, 80, 23),
        files: Rect::default(),
        commit: Rect::default(),
        main: Rect::default(),
        side_layout: false,
    };

    let mut term = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
    term.draw(|frame| keifu::ui::draw(frame, &mut app))
        .expect("render issue detail");
    app.handle_action(Action::MouseClick { col: 5, row: 5 })
        .expect("detail click is handled");

    assert!(matches!(app.mode, AppMode::IssueDetail));
    assert_eq!(app.focused_panel, FocusedPanel::Files);
}
