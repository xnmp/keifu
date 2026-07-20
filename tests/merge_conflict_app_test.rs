//! App-level integration tests for merge-conflict awareness: the files pane's
//! "Merge Changes" section, the status-bar indicator, and resolving a conflict
//! through the action dispatcher.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

use keifu::action::Action;
use keifu::app::{App, FilesPaneItem, FocusedPanel};
use keifu::git::operations::{checkout_branch, create_branch, merge_branch, OpOutcome};
use keifu::git::{CommitDiffInfo, OperationState, StageStatus};
use keifu::ui::status_bar::StatusBar;
use keifu::ui::theme::Theme;

mod common;
use common::{commit_file, init_repo, Seed};

/// Build an App sitting on a repo left mid-merge with a single conflicted file
/// (`f.txt`), with the uncommitted node selected and its diff primed so the
/// Merge Changes section is populated synchronously.
fn conflicted_merge_app() -> (tempfile::TempDir, App) {
    let (td, git_repo) = init_repo(Seed::Empty);
    {
        let repo = git_repo.repo();
        let base = commit_file(repo, "f.txt", "base\n", "base");
        let default = repo.head().unwrap().shorthand().unwrap().to_string();
        create_branch(repo, "feature", base).unwrap();
        commit_file(repo, "f.txt", "main\n", "main edit");
        checkout_branch(repo, "feature").unwrap();
        commit_file(repo, "f.txt", "feature\n", "feature edit");
        checkout_branch(repo, &default).unwrap();
        let outcome = merge_branch(repo, "feature", git2::BranchType::Local).unwrap();
        assert!(matches!(outcome, OpOutcome::Conflicts { .. }));
    }

    let mut app = App::from_repo(git_repo).unwrap();
    // Select the uncommitted node (index 0) and prime the quick diff so the
    // conflicted file is classified without waiting on the async loader.
    app.graph_nav.graph_list_state.select(Some(0));
    assert!(app.is_uncommitted_selected(), "uncommitted node should exist");
    app.diff_cache.set_quick_uncommitted(app.repo.repo());
    app.sync_file_list_cache();
    (td, app)
}

fn section_headers(items: &[FilesPaneItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            FilesPaneItem::SectionHeader(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn merge_conflict_surfaces_merge_changes_section_and_op_state() {
    let (_td, app) = conflicted_merge_app();

    // Operation state + conflict count are set (these drive the status bar).
    assert_eq!(app.op_state, OperationState::Merge);
    assert_eq!(app.conflict_count, 1);

    // Files pane shows a "Merge Changes" section, listed before any other.
    let items = app.display_items();
    let headers = section_headers(items);
    assert!(
        headers.first().map(String::as_str) == Some("Merge Changes"),
        "Merge Changes must be the first section, got {headers:?}"
    );

    // The conflicted file is classified as Conflicted.
    let conflicted: Vec<_> = items
        .iter()
        .filter_map(|i| match i {
            FilesPaneItem::File(f) if f.stage_status == Some(StageStatus::Conflicted) => {
                Some(f.path.to_string_lossy().to_string())
            }
            _ => None,
        })
        .collect();
    assert_eq!(conflicted, vec!["f.txt".to_string()]);
}

#[test]
fn status_bar_renders_merging_indicator_with_conflict_count() {
    let (_td, app) = conflicted_merge_app();

    let theme = Theme::dark();
    let area = Rect::new(0, 0, 160, 1);
    let mut buf = Buffer::empty(area);
    StatusBar::new(&app, &theme).render(area, &mut buf);

    let rendered: String = buf.content.iter().map(|c| c.symbol()).collect();
    assert!(
        rendered.contains("MERGING"),
        "status bar should show the MERGING indicator: {rendered:?}"
    );
    assert!(
        rendered.contains("1 conflict"),
        "status bar should show the conflict count: {rendered:?}"
    );
}

#[test]
fn accept_ours_action_resolves_conflict_and_clears_merge_section() {
    let (_td, mut app) = conflicted_merge_app();
    let repo_path = app.repo_path.clone();

    // Focus the files pane; the default selection resolves to the first file,
    // which is the conflicted entry in Merge Changes.
    app.focused_panel = FocusedPanel::Files;
    app.handle_action(Action::AcceptOurs).unwrap();

    // The conflict is resolved: no conflicts remain, and the file now holds our
    // (main) content.
    assert_eq!(app.conflict_count, 0);
    assert_eq!(
        std::fs::read_to_string(std::path::Path::new(&repo_path).join("f.txt")).unwrap(),
        "main\n"
    );

    // The Merge Changes section is gone; the resolved file moved to Staged.
    let headers = section_headers(app.display_items());
    assert!(
        !headers.contains(&"Merge Changes".to_string()),
        "resolved file should leave the Merge Changes section, got {headers:?}"
    );

    // The merge is still in progress (MERGE_HEAD present) until continued.
    assert_eq!(app.op_state, OperationState::Merge);
}

#[test]
fn from_working_tree_classifies_conflict_once() {
    // Directly exercise the full (async) diff path used by the diff cache.
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let base = commit_file(repo, "f.txt", "base\n", "base");
    let default = repo.head().unwrap().shorthand().unwrap().to_string();
    create_branch(repo, "feature", base).unwrap();
    commit_file(repo, "f.txt", "main\n", "main edit");
    checkout_branch(repo, "feature").unwrap();
    commit_file(repo, "f.txt", "feature\n", "feature edit");
    checkout_branch(repo, &default).unwrap();
    assert!(matches!(
        merge_branch(repo, "feature", git2::BranchType::Local).unwrap(),
        OpOutcome::Conflicts { .. }
    ));

    let diff = CommitDiffInfo::from_working_tree(repo).unwrap();

    // The conflicted file is classified Conflicted, on the unstaged side...
    let conflicted: Vec<_> = diff
        .unstaged_files
        .iter()
        .filter(|f| f.stage_status == Some(StageStatus::Conflicted))
        .collect();
    assert_eq!(conflicted.len(), 1);
    assert_eq!(conflicted[0].path, std::path::PathBuf::from("f.txt"));

    // ...never duplicated onto the staged side.
    assert!(diff
        .staged_files
        .iter()
        .all(|f| f.stage_status != Some(StageStatus::Conflicted)));
    let appearances = diff
        .staged_files
        .iter()
        .chain(diff.unstaged_files.iter())
        .filter(|f| f.path == std::path::Path::new("f.txt"))
        .count();
    assert_eq!(appearances, 1, "conflict must appear exactly once");
}

#[test]
fn abort_action_flow_returns_repo_to_clean() {
    let (_td, mut app) = conflicted_merge_app();

    // Abort is guarded behind the Confirm dialog; drive the full flow.
    app.focused_panel = FocusedPanel::Files;
    app.handle_action(Action::AbortOperation).unwrap();
    app.handle_action(Action::Confirm).unwrap();

    assert_eq!(app.op_state, OperationState::Clean);
    assert_eq!(app.conflict_count, 0);
}
