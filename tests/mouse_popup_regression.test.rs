//! Regression coverage for issue-popup mouse row selection.

use keifu::action::Action;
use keifu::app::{App, AppMode, IssueListState, IssueListView};
use keifu::issue::{IssueFilter, IssueInfo, IssueState, IssueViewFilter};
use ratatui::layout::Rect;

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
