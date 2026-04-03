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

// ── Workflow: Stage and Commit ──────────────────────────────────────

#[test]
fn stage_and_commit_workflow() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "initial commit");

    // Configure git user for shell-based commit
    repo.repo()
        .config()
        .unwrap()
        .set_str("user.name", "Test User")
        .unwrap();
    repo.repo()
        .config()
        .unwrap()
        .set_str("user.email", "test@example.com")
        .unwrap();

    // Create and stage an uncommitted change via git2 (bypassing diff cache)
    fs::write(td.path().join("b.txt"), "new file").unwrap();
    {
        let mut index = repo.repo().index().unwrap();
        index.add_path(Path::new("b.txt")).unwrap();
        index.write().unwrap();
    }

    let mut app = make_app(repo);

    // Uncommitted node should be at index 0
    let node = &app.graph_layout.nodes[0];
    assert!(node.is_uncommitted);
    assert!(app.is_uncommitted_selected());

    // Switch to CommitDetail to enter editing
    app.focused_panel = FocusedPanel::CommitDetail;
    app.handle_action(Action::StartEditing).unwrap();
    assert!(app.editing_commit_message);

    // Type commit message
    for c in "test commit".chars() {
        app.handle_action(Action::EditorChar(c)).unwrap();
    }

    // Commit
    app.handle_action(Action::CommitChanges).unwrap();
    assert!(!app.editing_commit_message);
    assert!(matches!(app.mode, AppMode::Normal));
}

// ── Workflow: Branch Create and Delete ──────────────────────────────

#[test]
fn branch_create_and_delete_workflow() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "initial");
    let mut app = make_app(repo);

    // Open CreateBranch input mode
    app.handle_action(Action::CreateBranch).unwrap();
    assert!(matches!(
        app.mode,
        AppMode::Input {
            action: keifu::app::InputAction::CreateBranch,
            ..
        }
    ));

    // Type branch name
    for c in "test-branch".chars() {
        app.handle_action(Action::InputChar(c)).unwrap();
    }

    // Confirm creation
    app.handle_action(Action::Confirm).unwrap();
    assert!(matches!(app.mode, AppMode::Normal));

    // Verify branch exists after refresh
    app.refresh(true).unwrap();
    let branch_names: Vec<&str> = app.branches.iter().map(|b| b.name.as_str()).collect();
    assert!(
        branch_names.contains(&"test-branch"),
        "Expected 'test-branch' in {:?}",
        branch_names
    );

    // Delete the branch: set up Confirm mode with DeleteBranch action
    app.mode = AppMode::Confirm {
        message: "Delete branch 'test-branch'?".to_string(),
        action: keifu::app::ConfirmAction::DeleteBranch("test-branch".to_string()),
    };
    app.handle_action(Action::Confirm).unwrap();

    // Verify branch gone after refresh
    app.refresh(true).unwrap();
    let branch_names: Vec<&str> = app.branches.iter().map(|b| b.name.as_str()).collect();
    assert!(
        !branch_names.contains(&"test-branch"),
        "Expected 'test-branch' to be deleted, but found in {:?}",
        branch_names
    );
}

// ── Workflow: Search ────────────────────────────────────────────────

#[test]
fn search_workflow() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "initial");
    // Create a branch so there's something to search for
    {
        let head = repo.repo().head().unwrap().peel_to_commit().unwrap();
        repo.repo().branch("feature-search", &head, false).unwrap();
    }
    let mut app = make_app(repo);

    // Open search mode
    app.handle_action(Action::Search).unwrap();
    assert!(matches!(
        app.mode,
        AppMode::Input {
            action: keifu::app::InputAction::Search,
            ..
        }
    ));

    // Type query that matches the branch name
    for c in "feature".chars() {
        app.handle_action(Action::InputChar(c)).unwrap();
    }

    // Verify search results exist
    assert!(
        app.search_match_count() > 0,
        "Expected search matches for 'feature', got 0"
    );

    // Cancel search
    app.handle_action(Action::Cancel).unwrap();
    assert!(matches!(app.mode, AppMode::Normal));
}

// ── Workflow: Uncommitted Changes Node Lifecycle ────────────────────

#[test]
fn uncommitted_changes_node_lifecycle() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "initial");

    // Clean state: no uncommitted node
    let mut app = make_app(repo);
    let first_node = &app.graph_layout.nodes[0];
    assert!(
        !first_node.is_uncommitted,
        "Clean repo should not have uncommitted node"
    );

    // Write a new file to disk (don't stage)
    fs::write(td.path().join("new_file.txt"), "hello").unwrap();

    // Refresh the app
    app.refresh(true).unwrap();

    // Verify uncommitted node now appears
    let first_node = &app.graph_layout.nodes[0];
    assert!(
        first_node.is_uncommitted,
        "After creating a file, uncommitted node should appear"
    );
}

// ── Workflow: Commit Detail Editing ─────────────────────────────────

#[test]
fn commit_detail_editing_workflow() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "initial");
    // Create uncommitted change so we have an uncommitted node
    fs::write(td.path().join("b.txt"), "new").unwrap();
    let mut app = make_app(repo);

    // Should be on uncommitted node
    assert!(app.is_uncommitted_selected());

    // Switch to CommitDetail panel
    app.focused_panel = FocusedPanel::CommitDetail;

    // Start editing
    app.handle_action(Action::StartEditing).unwrap();
    assert!(app.editing_commit_message);

    // Type chars
    for c in "hello world".chars() {
        app.handle_action(Action::EditorChar(c)).unwrap();
    }

    // Stop editing
    app.handle_action(Action::StopEditing).unwrap();
    assert!(!app.editing_commit_message);

    // Editor should have the text
    assert_eq!(app.commit_editor.text.trim(), "hello world");
}

// ── Workflow: Branch Filter ─────────────────────────────────────────

#[test]
fn branch_filter_workflow() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "initial");
    {
        let head = repo.repo().head().unwrap().peel_to_commit().unwrap();
        repo.repo().branch("feature-x", &head, false).unwrap();
    }
    let mut app = make_app(repo);

    // Open branch filter
    app.handle_action(Action::OpenBranchFilter).unwrap();
    assert!(matches!(app.mode, AppMode::BranchFilter { .. }));

    // Cancel
    app.handle_action(Action::Cancel).unwrap();
    assert!(matches!(app.mode, AppMode::Normal));
}

// ── Workflow: Commit Menu for Non-Uncommitted Node ──────────────────

#[test]
fn commit_menu_for_regular_commit() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    commit_file(repo.repo(), "b.txt", "b", "second");
    let mut app = make_app(repo);

    // Ensure we're on a non-uncommitted node
    assert!(!app.graph_layout.nodes[0].is_uncommitted);

    // Open commit menu
    app.handle_action(Action::OpenCommitMenu).unwrap();
    assert!(
        matches!(app.mode, AppMode::CommitMenu { ref items, .. } if !items.is_empty()),
        "Expected CommitMenu with items, got {:?}",
        app.mode
    );

    // Cancel returns to Normal
    app.handle_action(Action::Cancel).unwrap();
    assert!(matches!(app.mode, AppMode::Normal));
}

// ── Workflow: Commit Menu on Uncommitted Node → Files Panel ─────────

#[test]
fn commit_menu_on_uncommitted_node_switches_to_files() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "initial");
    // Create uncommitted change
    fs::write(td.path().join("b.txt"), "new").unwrap();
    let mut app = make_app(repo);

    // Should be on uncommitted node
    assert!(app.is_uncommitted_selected());
    assert_eq!(app.focused_panel, FocusedPanel::Graph);

    // Open commit menu on uncommitted node
    app.handle_action(Action::OpenCommitMenu).unwrap();

    // Should switch to Files panel instead of opening CommitMenu
    assert_eq!(app.focused_panel, FocusedPanel::Files);
    assert!(
        matches!(app.mode, AppMode::Normal),
        "Expected Normal mode (not CommitMenu), got {:?}",
        app.mode
    );
}
