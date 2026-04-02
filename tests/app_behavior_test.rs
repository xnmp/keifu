//! Pre-refactor regression tests for App behavior.
//! Tests the behavioral contracts of subsystems that will be extracted:
//! - Graph navigation
//! - Files pane state (grouping, filtering, index mapping)
//! - Action dispatch (mode transitions, panel navigation)
//! - SearchState
//! - filter_remote_duplicates

use std::fs;
use std::path::Path;

use git2::{Oid, Repository, Signature};
use tempfile::TempDir;

use keifu::action::Action;
use keifu::app::{AppMode, FocusedPanel};
use keifu::git::GitRepository;

// ── Helpers ─────────────────────────────────────────────────────────

fn init_repo() -> (TempDir, GitRepository) {
    let tempdir = tempfile::tempdir().unwrap();
    Repository::init(tempdir.path()).unwrap();
    let repo = GitRepository::open(tempdir.path()).unwrap();
    (tempdir, repo)
}

fn commit_file(repo: &Repository, path: &str, contents: &str, message: &str) -> Oid {
    let workdir = repo.workdir().unwrap();
    fs::write(workdir.join(path), contents).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(path)).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let signature = Signature::now("Test User", "test@example.com").unwrap();
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let parents = parent.iter().collect::<Vec<_>>();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parents,
    )
    .unwrap()
}

fn make_app(repo: GitRepository) -> keifu::app::App {
    keifu::app::App::from_repo(repo).unwrap()
}

// ── Graph Navigation ────────────────────────────────────────────────

#[test]
fn move_selection_down_then_up() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    commit_file(repo.repo(), "b.txt", "b", "second");
    commit_file(repo.repo(), "c.txt", "c", "third");
    let mut app = make_app(repo);

    // Starts at top (index 0)
    assert_eq!(app.graph_list_state.selected(), Some(0));

    app.handle_action(Action::MoveDown).unwrap();
    assert_eq!(app.graph_list_state.selected(), Some(1));

    app.handle_action(Action::MoveDown).unwrap();
    assert_eq!(app.graph_list_state.selected(), Some(2));

    app.handle_action(Action::MoveUp).unwrap();
    assert_eq!(app.graph_list_state.selected(), Some(1));
}

#[test]
fn move_selection_clamps_at_boundaries() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    commit_file(repo.repo(), "b.txt", "b", "second");
    let mut app = make_app(repo);

    // Move up past top
    app.handle_action(Action::MoveUp).unwrap();
    assert_eq!(app.graph_list_state.selected(), Some(0));

    // Go to bottom, then try to go past
    app.handle_action(Action::GoToBottom).unwrap();
    let bottom = app.graph_list_state.selected().unwrap();
    app.handle_action(Action::MoveDown).unwrap();
    assert_eq!(app.graph_list_state.selected(), Some(bottom));
}

#[test]
fn page_up_down_moves_by_10() {
    let (_td, repo) = init_repo();
    for i in 0..20 {
        commit_file(repo.repo(), "a.txt", &format!("{i}"), &format!("commit {i}"));
    }
    let mut app = make_app(repo);

    app.handle_action(Action::PageDown).unwrap();
    assert_eq!(app.graph_list_state.selected(), Some(10));

    app.handle_action(Action::PageUp).unwrap();
    assert_eq!(app.graph_list_state.selected(), Some(0));
}

#[test]
fn go_to_top_and_bottom() {
    let (_td, repo) = init_repo();
    for i in 0..5 {
        commit_file(repo.repo(), "a.txt", &format!("{i}"), &format!("commit {i}"));
    }
    let mut app = make_app(repo);
    let max_idx = app.graph_layout.nodes.len() - 1;

    app.handle_action(Action::GoToBottom).unwrap();
    assert_eq!(app.graph_list_state.selected(), Some(max_idx));

    app.handle_action(Action::GoToTop).unwrap();
    assert_eq!(app.graph_list_state.selected(), Some(0));
}

#[test]
fn graph_navigation_resets_commit_detail_scroll() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    commit_file(repo.repo(), "b.txt", "b", "second");
    let mut app = make_app(repo);
    app.commit_detail_scroll = 5;

    app.handle_action(Action::MoveDown).unwrap();
    assert_eq!(app.commit_detail_scroll, 0);
}

// ── Branch Navigation ───────────────────────────────────────────────

#[test]
fn next_prev_branch_traverses_branches() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "initial");
    // Create a second branch
    {
        let head = repo.repo().head().unwrap().peel_to_commit().unwrap();
        repo.repo().branch("feature", &head, false).unwrap();
    }
    let mut app = make_app(repo);

    // Should have at least one branch position
    assert!(!app.branch_positions.is_empty());

    let initial_pos = app.selected_branch_position;
    app.handle_action(Action::NextBranch).unwrap();
    // If there's more than one branch, position should change
    if app.branch_positions.len() > 1 {
        assert_ne!(app.selected_branch_position, initial_pos);
    }
}

#[test]
fn jump_to_head_selects_head_branch() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    commit_file(repo.repo(), "b.txt", "b", "second");
    let mut app = make_app(repo);
    app.head_name = Some("main".to_string());

    // Move away from HEAD
    app.handle_action(Action::GoToBottom).unwrap();

    // Jump back
    app.handle_action(Action::JumpToHead).unwrap();

    // Should be at a node with the HEAD branch
    if let Some(pos) = app.selected_branch_position {
        let (_, name) = &app.branch_positions[pos];
        assert_eq!(name, "main");
    }
}

// ── Panel Navigation ────────────────────────────────────────────────

#[test]
fn panel_right_cycles_graph_files_detail() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    assert_eq!(app.focused_panel, FocusedPanel::Graph);

    app.handle_action(Action::PanelRight).unwrap();
    assert_eq!(app.focused_panel, FocusedPanel::Files);

    app.handle_action(Action::PanelRight).unwrap();
    assert_eq!(app.focused_panel, FocusedPanel::CommitDetail);

    app.handle_action(Action::PanelRight).unwrap();
    assert_eq!(app.focused_panel, FocusedPanel::Graph);
}

#[test]
fn panel_left_cycles_reverse() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    assert_eq!(app.focused_panel, FocusedPanel::Graph);

    app.handle_action(Action::PanelLeft).unwrap();
    assert_eq!(app.focused_panel, FocusedPanel::CommitDetail);

    app.handle_action(Action::PanelLeft).unwrap();
    assert_eq!(app.focused_panel, FocusedPanel::Files);

    app.handle_action(Action::PanelLeft).unwrap();
    assert_eq!(app.focused_panel, FocusedPanel::Graph);
}

#[test]
fn focus_graph_returns_from_files() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);
    app.focused_panel = FocusedPanel::Files;

    app.handle_action(Action::FocusGraph).unwrap();
    assert_eq!(app.focused_panel, FocusedPanel::Graph);
}

#[test]
fn panel_switch_clears_editing() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);
    app.editing_commit_message = true;
    app.focused_panel = FocusedPanel::CommitDetail;

    app.handle_action(Action::PanelRight).unwrap();
    assert!(!app.editing_commit_message);
}

// ── Mode Transitions ────────────────────────────────────────────────

#[test]
fn force_quit_sets_should_quit() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    app.handle_action(Action::ForceQuit).unwrap();
    assert!(app.should_quit);
}

#[test]
fn toggle_layout_flips_side_panel() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);
    let original = app.side_panel_layout;

    app.handle_action(Action::ToggleLayout).unwrap();
    assert_ne!(app.side_panel_layout, original);

    app.handle_action(Action::ToggleLayout).unwrap();
    assert_eq!(app.side_panel_layout, original);
}

#[test]
fn toggle_debug_keys() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    assert!(!app.debug_keys);
    app.handle_action(Action::ToggleDebugKeys).unwrap();
    assert!(app.debug_keys);
}

#[test]
fn toggle_help_enters_help_mode() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    app.handle_action(Action::ToggleHelp).unwrap();
    assert!(matches!(app.mode, AppMode::Help));
}

#[test]
fn esc_in_help_returns_to_normal() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);
    app.mode = AppMode::Help;

    app.handle_action(Action::ToggleHelp).unwrap();
    assert!(matches!(app.mode, AppMode::Normal));
}

#[test]
fn quit_from_graph_sets_should_quit() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    app.handle_action(Action::Quit).unwrap();
    assert!(app.should_quit);
}

#[test]
fn search_opens_input_mode() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    app.handle_action(Action::Search).unwrap();
    assert!(matches!(
        app.mode,
        AppMode::Input {
            action: keifu::app::InputAction::Search,
            ..
        }
    ));
}

#[test]
fn create_branch_opens_input_mode() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    app.handle_action(Action::CreateBranch).unwrap();
    assert!(matches!(
        app.mode,
        AppMode::Input {
            action: keifu::app::InputAction::CreateBranch,
            ..
        }
    ));
}

// ── Files Pane ──────────────────────────────────────────────────────

#[test]
fn files_filter_mode_lifecycle() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);
    app.focused_panel = FocusedPanel::Files;

    // Start filter
    app.handle_action(Action::StartFilesFilter).unwrap();
    assert!(app.files_pane.files_filter_active);
    assert!(app.files_pane.files_filter.is_empty());

    // Type chars
    app.handle_action(Action::FilesFilterChar('a')).unwrap();
    app.handle_action(Action::FilesFilterChar('b')).unwrap();
    assert_eq!(app.files_pane.files_filter, "ab");

    // Backspace
    app.handle_action(Action::FilesFilterBackspace).unwrap();
    assert_eq!(app.files_pane.files_filter, "a");

    // Confirm keeps filter text
    app.handle_action(Action::Confirm).unwrap();
    assert!(!app.files_pane.files_filter_active);
    assert_eq!(app.files_pane.files_filter, "a");
}

#[test]
fn files_filter_cancel_clears_filter() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);
    app.focused_panel = FocusedPanel::Files;
    app.files_pane.files_filter_active = true;
    app.files_pane.files_filter = "test".to_string();

    app.handle_action(Action::Cancel).unwrap();
    assert!(!app.files_pane.files_filter_active);
    assert!(app.files_pane.files_filter.is_empty());
}

#[test]
fn files_filter_backspace_on_empty_exits_filter() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);
    app.focused_panel = FocusedPanel::Files;
    app.files_pane.files_filter_active = true;
    app.files_pane.files_filter = String::new();

    app.handle_action(Action::FilesFilterBackspace).unwrap();
    assert!(!app.files_pane.files_filter_active);
}

#[test]
fn toggle_folder_view_flips_flag() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);
    app.focused_panel = FocusedPanel::Files;

    assert!(!app.files_pane.files_group_by_folder);
    app.handle_action(Action::ToggleFolderView).unwrap();
    assert!(app.files_pane.files_group_by_folder);
    app.handle_action(Action::ToggleFolderView).unwrap();
    assert!(!app.files_pane.files_group_by_folder);
}

// ── Commit Detail ───────────────────────────────────────────────────

#[test]
fn commit_detail_scroll_up_down() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);
    app.focused_panel = FocusedPanel::CommitDetail;
    app.commit_detail_max_scroll = 20;

    app.handle_action(Action::MoveDown).unwrap();
    assert_eq!(app.commit_detail_scroll, 1);

    app.handle_action(Action::MoveDown).unwrap();
    assert_eq!(app.commit_detail_scroll, 2);

    app.handle_action(Action::MoveUp).unwrap();
    assert_eq!(app.commit_detail_scroll, 1);
}

#[test]
fn commit_detail_scroll_clamped_to_max() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);
    app.focused_panel = FocusedPanel::CommitDetail;
    app.commit_detail_max_scroll = 5;
    app.commit_detail_scroll = 5;

    app.handle_action(Action::MoveDown).unwrap();
    assert_eq!(app.commit_detail_scroll, 5);
}

#[test]
fn commit_detail_go_to_top_resets_scroll() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);
    app.focused_panel = FocusedPanel::CommitDetail;
    app.commit_detail_scroll = 10;

    app.handle_action(Action::GoToTop).unwrap();
    assert_eq!(app.commit_detail_scroll, 0);
}

// ── Error Mode ──────────────────────────────────────────────────────

#[test]
fn cancel_in_error_mode_returns_to_normal() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);
    app.mode = AppMode::Error {
        message: "test error".to_string(),
    };

    app.handle_action(Action::Cancel).unwrap();
    assert!(matches!(app.mode, AppMode::Normal));
}

// ── Commit Menu ─────────────────────────────────────────────────────

#[test]
fn commit_menu_navigation_wraps() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    use keifu::app::CommitMenuItem;
    app.mode = AppMode::CommitMenu {
        items: vec![
            CommitMenuItem::Checkout,
            CommitMenuItem::CopyHash,
            CommitMenuItem::Revert,
        ],
        selected: 0,
    };

    // Down from 0 → 1
    app.handle_action(Action::MoveDown).unwrap();
    if let AppMode::CommitMenu { selected, .. } = &app.mode {
        assert_eq!(*selected, 1);
    }

    // Down from 1 → 2
    app.handle_action(Action::MoveDown).unwrap();
    if let AppMode::CommitMenu { selected, .. } = &app.mode {
        assert_eq!(*selected, 2);
    }

    // Down from 2 → wraps to 0
    app.handle_action(Action::MoveDown).unwrap();
    if let AppMode::CommitMenu { selected, .. } = &app.mode {
        assert_eq!(*selected, 0);
    }

    // Up from 0 → wraps to 2
    app.handle_action(Action::MoveUp).unwrap();
    if let AppMode::CommitMenu { selected, .. } = &app.mode {
        assert_eq!(*selected, 2);
    }
}

#[test]
fn commit_menu_cancel_returns_to_normal() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    use keifu::app::CommitMenuItem;
    app.mode = AppMode::CommitMenu {
        items: vec![CommitMenuItem::CopyHash],
        selected: 0,
    };

    app.handle_action(Action::Cancel).unwrap();
    assert!(matches!(app.mode, AppMode::Normal));
}

// ── Message Handling ────────────────────────────────────────────────

#[test]
fn set_message_and_get_message() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    app.set_message("hello");
    assert_eq!(app.get_message(), Some("hello"));
}

// ── Refresh ─────────────────────────────────────────────────────────

#[test]
fn refresh_preserves_selection_by_oid() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    commit_file(repo.repo(), "b.txt", "b", "second");
    let mut app = make_app(repo);

    // Select the second commit (index 1, since newest is 0)
    app.handle_action(Action::MoveDown).unwrap();
    let selected_before = app.graph_list_state.selected();

    app.refresh(false).unwrap();

    // Selection should be preserved (same commit still at same position)
    assert_eq!(app.graph_list_state.selected(), selected_before);
}

#[test]
fn force_refresh_clears_diff_cache() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    // After force refresh, cached_diff_or_quick should be None
    // (no diff has been loaded yet)
    app.refresh(true).unwrap();
    assert!(app.cached_diff_or_quick().is_none());
}
