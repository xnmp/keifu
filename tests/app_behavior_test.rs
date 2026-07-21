//! Pre-refactor regression tests for App behavior.
//! Tests the behavioral contracts of subsystems that will be extracted:
//! - Graph navigation
//! - Files pane state (grouping, filtering, index mapping)
//! - Action dispatch (mode transitions, panel navigation)
//! - SearchState
//! - filter_remote_duplicates

use std::fs;
use std::path::Path;

use git2::{Oid, Repository, Signature, Status};
use tempfile::TempDir;

use keifu::action::Action;
use keifu::app::{
    App, AppMode, CommitMenuItem, ConfirmAction, FilesPaneItem, FocusedPanel, InputAction,
};
use keifu::git::GitRepository;

// ── Helpers ─────────────────────────────────────────────────────────

fn init_repo() -> (TempDir, GitRepository) {
    let tempdir = tempfile::tempdir().unwrap();
    let git2_repo = Repository::init(tempdir.path()).unwrap();
    // Configure a committer identity so shell-git operations (cherry-pick,
    // revert, reset) and git2's `repo.signature()` (merge commits) succeed,
    // and disable gpg signing so they don't block on a key.
    {
        let mut cfg = git2_repo.config().unwrap();
        cfg.set_str("user.name", "Test User").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        cfg.set_bool("commit.gpgsign", false).unwrap();
    }
    drop(git2_repo);
    let repo = GitRepository::open(tempdir.path()).unwrap();
    (tempdir, repo)
}

/// Create a commit on top of `refname` (e.g. "refs/heads/feature") without
/// touching HEAD or the working tree. Advances `refname` and returns the new oid.
fn commit_to_ref(
    repo: &Repository,
    refname: &str,
    path: &str,
    contents: &str,
    message: &str,
) -> Oid {
    let parent = repo
        .find_reference(refname)
        .unwrap()
        .peel_to_commit()
        .unwrap();
    let mut builder = repo.treebuilder(Some(&parent.tree().unwrap())).unwrap();
    let blob = repo.blob(contents.as_bytes()).unwrap();
    builder.insert(path, blob, 0o100644).unwrap();
    let tree_id = builder.write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = Signature::now("Test User", "test@example.com").unwrap();
    repo.commit(Some(refname), &sig, &sig, message, &tree, &[&parent])
        .unwrap()
}

/// The OID that HEAD currently points at, read from a fresh handle on disk.
fn head_oid(repo_dir: &Path) -> Oid {
    Repository::open(repo_dir)
        .unwrap()
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id()
}

/// Owned facts about the HEAD commit, read from a fresh handle on disk:
/// (oid, parent_count, parent_oids, message). Returns owned data so the
/// backing `Repository` can be dropped before the values are used.
fn head_commit_facts(repo_dir: &Path) -> (Oid, usize, Vec<Oid>, String) {
    let repo = Repository::open(repo_dir).unwrap();
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    let parents = (0..commit.parent_count())
        .map(|i| commit.parent_id(i).unwrap())
        .collect();
    (
        commit.id(),
        commit.parent_count(),
        parents,
        commit.message().unwrap_or("").to_string(),
    )
}

/// The git status flags for a single path (untracked included), read from disk.
fn path_status(repo_dir: &Path, file: &str) -> Status {
    let repo = Repository::open(repo_dir).unwrap();
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true);
    let statuses = repo.statuses(Some(&mut opts)).unwrap();
    statuses
        .iter()
        .find(|entry| entry.path() == Some(file))
        .map(|entry| entry.status())
        .unwrap_or(Status::CURRENT)
}

/// Number of stash entries, read from a fresh handle on disk.
fn stash_count(repo_dir: &Path) -> usize {
    let mut repo = Repository::open(repo_dir).unwrap();
    let mut count = 0;
    repo.stash_foreach(|_, _, _| {
        count += 1;
        true
    })
    .unwrap();
    count
}

/// Populate the uncommitted quick-diff + files-pane cache synchronously so
/// files-pane operations can resolve the selected file (the async diff loader
/// never runs in tests).
fn prime_uncommitted(app: &mut App) {
    app.diff_cache.set_quick_uncommitted(app.repo.repo());
    app.sync_file_list_cache();
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
    assert_eq!(app.graph_nav.graph_list_state.selected(), Some(0));

    app.handle_action(Action::MoveDown).unwrap();
    assert_eq!(app.graph_nav.graph_list_state.selected(), Some(1));

    app.handle_action(Action::MoveDown).unwrap();
    assert_eq!(app.graph_nav.graph_list_state.selected(), Some(2));

    app.handle_action(Action::MoveUp).unwrap();
    assert_eq!(app.graph_nav.graph_list_state.selected(), Some(1));
}

#[test]
fn move_selection_clamps_at_boundaries() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    commit_file(repo.repo(), "b.txt", "b", "second");
    let mut app = make_app(repo);

    // Move up past top
    app.handle_action(Action::MoveUp).unwrap();
    assert_eq!(app.graph_nav.graph_list_state.selected(), Some(0));

    // Go to bottom, then try to go past
    app.handle_action(Action::GoToBottom).unwrap();
    let bottom = app.graph_nav.graph_list_state.selected().unwrap();
    app.handle_action(Action::MoveDown).unwrap();
    assert_eq!(app.graph_nav.graph_list_state.selected(), Some(bottom));
}

#[test]
fn page_up_down_moves_by_10() {
    let (_td, repo) = init_repo();
    for i in 0..20 {
        commit_file(repo.repo(), "a.txt", &format!("{i}"), &format!("commit {i}"));
    }
    let mut app = make_app(repo);

    app.handle_action(Action::PageDown).unwrap();
    assert_eq!(app.graph_nav.graph_list_state.selected(), Some(10));

    app.handle_action(Action::PageUp).unwrap();
    assert_eq!(app.graph_nav.graph_list_state.selected(), Some(0));
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
    assert_eq!(app.graph_nav.graph_list_state.selected(), Some(max_idx));

    app.handle_action(Action::GoToTop).unwrap();
    assert_eq!(app.graph_nav.graph_list_state.selected(), Some(0));
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
    let c1 = commit_file(repo.repo(), "a.txt", "a", "first");
    // "old" stays at c1; main advances to a second commit, so the two branch
    // tips live on two distinct graph nodes.
    repo.repo()
        .branch("old", &repo.repo().find_commit(c1).unwrap(), false)
        .unwrap();
    commit_file(repo.repo(), "b.txt", "b", "second");
    let mut app = make_app(repo);

    // Two branch tips on two distinct nodes: main@node0, old@node1.
    assert_eq!(app.graph_nav.branch_positions.len(), 2);
    assert_eq!(app.graph_nav.selected_branch_position, Some(0));
    assert_eq!(app.graph_nav.graph_list_state.selected(), Some(0));

    // NextBranch moves the selection to the older branch tip on node 1.
    app.handle_action(Action::NextBranch).unwrap();
    assert_eq!(app.graph_nav.selected_branch_position, Some(1));
    assert_eq!(app.graph_nav.graph_list_state.selected(), Some(1));
    assert_eq!(app.graph_nav.selected_branch_name(), Some("old"));

    // PrevBranch moves it back to the first branch tip on node 0.
    app.handle_action(Action::PrevBranch).unwrap();
    assert_eq!(app.graph_nav.selected_branch_position, Some(0));
    assert_eq!(app.graph_nav.graph_list_state.selected(), Some(0));
}

#[test]
fn jump_to_head_selects_head_branch() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    commit_file(repo.repo(), "b.txt", "b", "second");
    let mut app = make_app(repo);
    let head_branch = app.head_name.clone().expect("repo has a head branch");
    // The HEAD branch tip is the newest commit, at graph node 0.
    let head_node = app
        .graph_layout
        .nodes
        .iter()
        .position(|n| n.is_head && !n.is_uncommitted)
        .expect("a HEAD commit node exists");

    // Move away from HEAD, then jump back.
    app.handle_action(Action::GoToBottom).unwrap();
    assert_ne!(app.graph_nav.graph_list_state.selected(), Some(head_node));

    app.handle_action(Action::JumpToHead).unwrap();

    // The selection must resolve to the HEAD branch position (not silently skipped).
    let pos = app
        .graph_nav
        .selected_branch_position
        .expect("JumpToHead selects the HEAD branch position");
    let (node_idx, name) = &app.graph_nav.branch_positions[pos];
    assert_eq!(name, &head_branch);
    assert_eq!(*node_idx, head_node);
    assert_eq!(app.graph_nav.graph_list_state.selected(), Some(head_node));
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
        filter: String::new(),
    };

    // Down from 0 → 1
    app.handle_action(Action::MoveDown).unwrap();
    if let AppMode::CommitMenu { selected, .. } = &app.mode {
        assert_eq!(*selected, 1);
    } else {
        panic!("expected CommitMenu mode, got {:?}", app.mode);
    }

    // Down from 1 → 2
    app.handle_action(Action::MoveDown).unwrap();
    if let AppMode::CommitMenu { selected, .. } = &app.mode {
        assert_eq!(*selected, 2);
    } else {
        panic!("expected CommitMenu mode, got {:?}", app.mode);
    }

    // Down from 2 → wraps to 0
    app.handle_action(Action::MoveDown).unwrap();
    if let AppMode::CommitMenu { selected, .. } = &app.mode {
        assert_eq!(*selected, 0);
    } else {
        panic!("expected CommitMenu mode, got {:?}", app.mode);
    }

    // Up from 0 → wraps to 2
    app.handle_action(Action::MoveUp).unwrap();
    if let AppMode::CommitMenu { selected, .. } = &app.mode {
        assert_eq!(*selected, 2);
    } else {
        panic!("expected CommitMenu mode, got {:?}", app.mode);
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
        filter: String::new(),
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
    let selected_before = app.graph_nav.graph_list_state.selected();

    app.refresh(false).unwrap();

    // Selection should be preserved (same commit still at same position)
    assert_eq!(app.graph_nav.graph_list_state.selected(), selected_before);
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

// ── Commit menu fuzzy filter ───────────────────────────────────────

#[test]
fn commit_menu_filter_down_wraps_at_match_count() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    use keifu::app::CommitMenuItem;
    app.mode = AppMode::CommitMenu {
        items: vec![
            CommitMenuItem::Checkout,
            CommitMenuItem::CherryPick,
            CommitMenuItem::Revert,
        ],
        selected: 0,
        filter: String::new(),
    };

    // Type "ch" — matches Checkout and Cherry-pick (2 items)
    app.handle_action(Action::InputChar('c')).unwrap();
    app.handle_action(Action::InputChar('h')).unwrap();

    // Move down twice — should wrap back to 0
    app.handle_action(Action::MoveDown).unwrap();
    app.handle_action(Action::MoveDown).unwrap();

    if let AppMode::CommitMenu { selected, .. } = &app.mode {
        assert_eq!(*selected, 0);
    } else {
        panic!("Expected CommitMenu mode");
    }
}

#[test]
fn commit_menu_filter_no_match_enter_does_nothing() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    use keifu::app::CommitMenuItem;
    app.mode = AppMode::CommitMenu {
        items: vec![CommitMenuItem::Checkout, CommitMenuItem::Revert],
        selected: 0,
        filter: String::new(),
    };

    // Type something that matches nothing
    app.handle_action(Action::InputChar('z')).unwrap();
    app.handle_action(Action::InputChar('z')).unwrap();
    app.handle_action(Action::InputChar('z')).unwrap();

    // Enter should not execute anything (stay in CommitMenu)
    app.handle_action(Action::MenuSelect).unwrap();
    assert!(matches!(app.mode, AppMode::CommitMenu { .. }));
}

// ── Commit filter (graph panel) ────────────────────────────────────

#[test]
fn commit_filter_navigation_stays_within_matches() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "fix: bug in parser");
    commit_file(repo.repo(), "b.txt", "b", "feat: new feature");
    commit_file(repo.repo(), "c.txt", "c", "fix: another bug");
    let mut app = make_app(repo);

    app.handle_action(Action::StartCommitFilter).unwrap();
    app.handle_action(Action::CommitFilterChar('f')).unwrap();
    app.handle_action(Action::CommitFilterChar('i')).unwrap();
    app.handle_action(Action::CommitFilterChar('x')).unwrap();

    // Navigate to bottom then try to go further — should stay bounded
    app.handle_action(Action::GoToBottom).unwrap();
    let bottom = app.graph_nav.selected_index().unwrap();

    app.handle_action(Action::MoveDown).unwrap();
    let after_bottom = app.graph_nav.selected_index().unwrap();
    assert_eq!(bottom, after_bottom);

    // The selected commit should contain "fix"
    let node = app.graph_nav.selected_node(&app.graph_layout).unwrap();
    let msg = node.commit.as_ref().unwrap().message.to_lowercase();
    assert!(msg.contains("fix"));
}

#[test]
fn commit_filter_cancel_allows_full_navigation() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    commit_file(repo.repo(), "b.txt", "b", "second");
    commit_file(repo.repo(), "c.txt", "c", "third");
    let mut app = make_app(repo);

    let total_nodes = app.graph_layout.nodes.len();

    app.handle_action(Action::StartCommitFilter).unwrap();
    app.handle_action(Action::CommitFilterChar('x')).unwrap();

    // Cancel should restore full navigation
    app.handle_action(Action::Cancel).unwrap();

    app.handle_action(Action::GoToBottom).unwrap();
    let bottom_idx = app.graph_nav.selected_index().unwrap();
    // Should be able to reach the last node
    assert_eq!(bottom_idx, total_nodes - 1);
}

#[test]
fn commit_filter_selected_commit_still_valid_after_refresh() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "fix: important");
    commit_file(repo.repo(), "b.txt", "b", "feat: unrelated");
    let mut app = make_app(repo);

    app.handle_action(Action::StartCommitFilter).unwrap();
    app.handle_action(Action::CommitFilterChar('f')).unwrap();
    app.handle_action(Action::CommitFilterChar('i')).unwrap();
    app.handle_action(Action::CommitFilterChar('x')).unwrap();

    // Refresh should preserve filter — selected commit should still match
    app.refresh(true).unwrap();

    let node = app.graph_nav.selected_node(&app.graph_layout).unwrap();
    assert!(
        node.commit.is_some(),
        "filtered selection must land on a real commit node after refresh"
    );
    let commit = node.commit.as_ref().unwrap();
    assert!(commit.message.to_lowercase().contains("fix"));
}

// ── Word-level editing ─────────────────────────────────────────────

#[test]
fn backspace_word_in_commit_filter_removes_last_word() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "hello world");
    commit_file(repo.repo(), "b.txt", "b", "hello there");
    let mut app = make_app(repo);

    app.handle_action(Action::StartCommitFilter).unwrap();
    // Type "hello world"
    for c in "hello world".chars() {
        app.handle_action(Action::CommitFilterChar(c)).unwrap();
    }

    // Backspace word should remove "world", leaving "hello "
    app.handle_action(Action::InputBackspaceWord).unwrap();

    // Now both "hello world" and "hello there" should be visible
    // (both contain "hello "). Navigate down — should reach second match.
    app.handle_action(Action::GoToTop).unwrap();
    let first = app.graph_nav.selected_index().unwrap();
    app.handle_action(Action::MoveDown).unwrap();
    let second = app.graph_nav.selected_index().unwrap();
    assert_ne!(first, second);
}

#[test]
fn clear_line_in_commit_filter_shows_all_commits() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    commit_file(repo.repo(), "b.txt", "b", "second");
    commit_file(repo.repo(), "c.txt", "c", "third");
    let mut app = make_app(repo);

    let total_nodes = app.graph_layout.nodes.len();

    app.handle_action(Action::StartCommitFilter).unwrap();
    app.handle_action(Action::CommitFilterChar('z')).unwrap();

    // Clear line should restore all commits
    app.handle_action(Action::InputClearLine).unwrap();

    // Should be able to reach the last node (all visible again)
    app.handle_action(Action::GoToBottom).unwrap();
    let bottom_idx = app.graph_nav.selected_index().unwrap();
    assert_eq!(bottom_idx, total_nodes - 1);
}

// ── handle_confirm_action dispatch → repository mutation ────────────
//
// Each test sets AppMode::Confirm { action } directly, dispatches
// Action::Confirm, and asserts the repository changed as that operation
// requires. These catch a mis-wired confirm→operation dispatch.

#[test]
fn confirm_merge_creates_merge_commit() {
    let (td, repo) = init_repo();
    let c1 = commit_file(repo.repo(), "a.txt", "a", "root");
    repo.repo()
        .branch("feature", &repo.repo().find_commit(c1).unwrap(), false)
        .unwrap();
    let c_feat = commit_to_ref(repo.repo(), "refs/heads/feature", "feature.txt", "f", "feature work");
    let c_main = commit_file(repo.repo(), "main.txt", "m", "main work");
    let mut app = make_app(repo);
    assert_eq!(head_oid(td.path()), c_main);

    app.mode = AppMode::Confirm {
        message: "Merge 'feature'?".to_string(),
        action: ConfirmAction::Merge { name: "feature".to_string(), is_remote: false },
    };
    app.handle_action(Action::Confirm).unwrap();

    // main and feature diverged, so the merge produces a two-parent commit.
    let (_, parent_count, parents, _) = head_commit_facts(td.path());
    assert_eq!(parent_count, 2, "merge should create a two-parent merge commit");
    assert!(parents.contains(&c_main), "one parent is the previous HEAD");
    assert!(parents.contains(&c_feat), "other parent is the merged branch tip");
    assert!(matches!(app.mode, AppMode::Normal));
}

#[test]
fn confirm_rebase_replays_head_onto_branch() {
    let (td, repo) = init_repo();
    let c1 = commit_file(repo.repo(), "a.txt", "a", "root");
    repo.repo()
        .branch("feature", &repo.repo().find_commit(c1).unwrap(), false)
        .unwrap();
    let c_feat = commit_to_ref(repo.repo(), "refs/heads/feature", "feature.txt", "f", "feature work");
    let c_main = commit_file(repo.repo(), "main.txt", "m", "main work");
    let mut app = make_app(repo);

    app.mode = AppMode::Confirm {
        message: "Rebase onto 'feature'?".to_string(),
        action: ConfirmAction::Rebase { name: "feature".to_string(), is_remote: false },
    };
    app.handle_action(Action::Confirm).unwrap();

    // HEAD's "main work" commit is replayed on top of the feature tip.
    let (id, parent_count, parents, message) = head_commit_facts(td.path());
    assert_ne!(id, c_main, "rebase creates a new commit");
    assert_eq!(parent_count, 1);
    assert_eq!(parents[0], c_feat, "rebased commit sits on the feature tip");
    assert!(message.contains("main work"));
    assert!(td.path().join("feature.txt").exists());
    assert!(td.path().join("main.txt").exists());
}

#[test]
fn confirm_cherry_pick_applies_commit_to_head() {
    let (td, repo) = init_repo();
    let c1 = commit_file(repo.repo(), "a.txt", "a", "root");
    repo.repo()
        .branch("feature", &repo.repo().find_commit(c1).unwrap(), false)
        .unwrap();
    let c_feat = commit_to_ref(
        repo.repo(),
        "refs/heads/feature",
        "feature.txt",
        "picked",
        "add feature file",
    );
    let mut app = make_app(repo);
    assert_eq!(head_oid(td.path()), c1);

    app.mode = AppMode::Confirm {
        message: "Cherry-pick?".to_string(),
        action: ConfirmAction::CherryPick(c_feat),
    };
    app.handle_action(Action::Confirm).unwrap();

    let (id, _, parents, _) = head_commit_facts(td.path());
    assert_ne!(id, c1, "HEAD advanced with the cherry-picked change");
    assert_eq!(parents[0], c1, "new commit sits on the previous HEAD");
    assert!(
        td.path().join("feature.txt").exists(),
        "cherry-picked file is present in the working tree"
    );
}

#[test]
fn confirm_revert_undoes_commit_changes() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "root");
    let c2 = commit_file(repo.repo(), "b.txt", "b", "add b");
    let mut app = make_app(repo);
    assert!(td.path().join("b.txt").exists());

    app.mode = AppMode::Confirm {
        message: "Revert?".to_string(),
        action: ConfirmAction::Revert(c2),
    };
    app.handle_action(Action::Confirm).unwrap();

    let (_, _, parents, _) = head_commit_facts(td.path());
    assert_eq!(parents[0], c2, "revert commit sits on top of the reverted commit");
    assert!(!td.path().join("b.txt").exists(), "revert removed the file the commit added");
    assert!(td.path().join("a.txt").exists());
}

#[test]
fn confirm_reset_soft_moves_head_keeps_changes_staged() {
    let (td, repo) = init_repo();
    let c1 = commit_file(repo.repo(), "a.txt", "a", "root");
    commit_file(repo.repo(), "b.txt", "b", "add b");
    let mut app = make_app(repo);

    app.mode = AppMode::Confirm {
        message: "Reset soft?".to_string(),
        action: ConfirmAction::ResetSoft(c1),
    };
    app.handle_action(Action::Confirm).unwrap();

    assert_eq!(head_oid(td.path()), c1, "HEAD moved back to c1");
    assert!(td.path().join("b.txt").exists(), "working-tree file is preserved");
    let st = path_status(td.path(), "b.txt");
    assert!(st.contains(Status::INDEX_NEW), "b.txt stays staged after a soft reset");
    assert!(!st.contains(Status::WT_NEW), "b.txt is not an unstaged/untracked change");
}

#[test]
fn confirm_reset_mixed_moves_head_unstages_changes() {
    let (td, repo) = init_repo();
    let c1 = commit_file(repo.repo(), "a.txt", "a", "root");
    commit_file(repo.repo(), "b.txt", "b", "add b");
    let mut app = make_app(repo);

    app.mode = AppMode::Confirm {
        message: "Reset mixed?".to_string(),
        action: ConfirmAction::ResetMixed(c1),
    };
    app.handle_action(Action::Confirm).unwrap();

    assert_eq!(head_oid(td.path()), c1, "HEAD moved back to c1");
    assert!(td.path().join("b.txt").exists(), "working-tree file is preserved");
    let st = path_status(td.path(), "b.txt");
    assert!(st.contains(Status::WT_NEW), "b.txt is unstaged (untracked) after a mixed reset");
    assert!(!st.contains(Status::INDEX_NEW), "b.txt is not staged after a mixed reset");
}

#[test]
fn confirm_reset_hard_moves_head_and_reverts_worktree() {
    let (td, repo) = init_repo();
    let c1 = commit_file(repo.repo(), "a.txt", "original", "root");
    commit_file(repo.repo(), "a.txt", "modified", "change a");
    let mut app = make_app(repo);
    assert_eq!(fs::read_to_string(td.path().join("a.txt")).unwrap(), "modified");

    app.mode = AppMode::Confirm {
        message: "Reset hard?".to_string(),
        action: ConfirmAction::ResetHard(c1),
    };
    app.handle_action(Action::Confirm).unwrap();

    assert_eq!(head_oid(td.path()), c1, "HEAD moved back to c1");
    assert_eq!(
        fs::read_to_string(td.path().join("a.txt")).unwrap(),
        "original",
        "hard reset reverted the working-tree file content"
    );
}

#[test]
fn confirm_stash_drop_removes_stash() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "original", "root");
    fs::write(td.path().join("a.txt"), "changed").unwrap();
    {
        let mut r = Repository::open(td.path()).unwrap();
        let sig = Signature::now("Test User", "test@example.com").unwrap();
        r.stash_save(&sig, "wip", None).unwrap();
    }
    assert_eq!(stash_count(td.path()), 1);
    let mut app = make_app(repo);

    app.mode = AppMode::Confirm {
        message: "Drop stash?".to_string(),
        action: ConfirmAction::StashDrop(0),
    };
    app.handle_action(Action::Confirm).unwrap();

    assert_eq!(stash_count(td.path()), 0, "the stash was dropped");
    assert!(matches!(app.mode, AppMode::Normal));
}

// ── open_commit_menu item-list construction ────────────────────────

#[test]
fn commit_menu_stash_node_shows_stash_items() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "original", "root");
    fs::write(td.path().join("a.txt"), "changed").unwrap();
    {
        let mut r = Repository::open(td.path()).unwrap();
        let sig = Signature::now("Test User", "test@example.com").unwrap();
        r.stash_save(&sig, "wip", None).unwrap();
    }
    let mut app = make_app(repo);

    let stash_idx = app
        .graph_layout
        .nodes
        .iter()
        .position(|n| n.is_stash)
        .expect("a stash node is present");
    app.graph_nav.graph_list_state.select(Some(stash_idx));

    app.handle_action(Action::OpenCommitMenu).unwrap();
    match &app.mode {
        AppMode::CommitMenu { items, .. } => assert_eq!(
            items,
            &vec![
                CommitMenuItem::StashApply,
                CommitMenuItem::StashPop,
                CommitMenuItem::BranchFromStash,
                CommitMenuItem::StashDrop,
            ],
        ),
        other => panic!("expected a stash CommitMenu, got {:?}", other),
    }
}

#[test]
fn stash_pop_conflict_shows_stash_specific_guidance_not_merge_flow() {
    // A stash whose contents conflict with a divergent HEAD commit.
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "f.txt", "base\n", "root");
    fs::write(td.path().join("f.txt"), "stashed\n").unwrap();
    {
        let mut r = Repository::open(td.path()).unwrap();
        let sig = Signature::now("Test User", "test@example.com").unwrap();
        r.stash_save(&sig, "wip", None).unwrap();
    }
    commit_file(repo.repo(), "f.txt", "committed\n", "diverge");
    let mut app = make_app(repo);

    // Select the stash node and open its menu, then pop it (item index 1).
    let stash_idx = app
        .graph_layout
        .nodes
        .iter()
        .position(|n| n.is_stash)
        .expect("a stash node is present");
    app.graph_nav.graph_list_state.select(Some(stash_idx));
    app.handle_action(Action::OpenCommitMenu).unwrap();
    app.handle_action(Action::MoveDown).unwrap(); // StashApply -> StashPop
    app.handle_action(Action::MenuSelect).unwrap();

    let msg = app.get_message().expect("a status message was set");
    assert!(
        msg.contains("stash kept") && msg.to_lowercase().contains("resolve"),
        "guidance should be stash-specific: {msg:?}"
    );
    // The merge-style Continue/Abort guidance must NOT appear — a stash conflict
    // leaves no operation in progress.
    assert!(
        !msg.contains("Continue (c)"),
        "stash guidance must not offer the merge Continue step: {msg:?}"
    );
    assert_eq!(
        app.op_state,
        keifu::git::OperationState::Clean,
        "a stash conflict must not put the app into an in-progress operation"
    );
}

#[test]
fn commit_menu_tagged_commit_includes_tag_ops() {
    let (_td, repo) = init_repo();
    let c1 = commit_file(repo.repo(), "a.txt", "a", "first");
    // Lightweight tag on the single commit.
    repo.repo()
        .tag_lightweight("v1.0", &repo.repo().find_object(c1, None).unwrap(), false)
        .unwrap();
    let mut app = make_app(repo);

    // Selection starts on the tagged commit node.
    app.handle_action(Action::OpenCommitMenu).unwrap();
    match &app.mode {
        AppMode::CommitMenu { items, .. } => {
            assert!(
                items.contains(&CommitMenuItem::DeleteTag),
                "menu should offer Delete tag, got {:?}",
                items
            );
            assert!(
                items.contains(&CommitMenuItem::PushTag),
                "menu should offer Push tag, got {:?}",
                items
            );
        }
        other => panic!("expected a CommitMenu, got {:?}", other),
    }
}

#[test]
fn commit_menu_untagged_commit_omits_tag_ops() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    app.handle_action(Action::OpenCommitMenu).unwrap();
    match &app.mode {
        AppMode::CommitMenu { items, .. } => {
            assert!(!items.contains(&CommitMenuItem::DeleteTag));
            assert!(!items.contains(&CommitMenuItem::PushTag));
        }
        other => panic!("expected a CommitMenu, got {:?}", other),
    }
}

#[test]
fn commit_menu_branch_tip_includes_rename() {
    let (_td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    app.handle_action(Action::OpenCommitMenu).unwrap();
    match &app.mode {
        AppMode::CommitMenu { items, .. } => assert!(
            items.contains(&CommitMenuItem::RenameBranch),
            "branch tip menu should offer Rename branch, got {:?}",
            items
        ),
        other => panic!("expected a CommitMenu, got {:?}", other),
    }
}

// ── Rename / stash / branch-from-stash input wiring ─────────────────

#[test]
fn rename_branch_via_input_renames_current_branch() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);
    let old = app.head_name.clone().expect("HEAD is on a branch");

    // The rename input is launched prefilled with the current name; the user
    // replaces it and confirms.
    app.mode = AppMode::Input {
        title: format!("Rename '{}' to", old),
        input: "renamed-branch".to_string(),
        action: InputAction::RenameBranch {
            old_name: old.clone(),
        },
    };
    app.handle_action(Action::Confirm).unwrap();

    assert!(matches!(app.mode, AppMode::Normal));
    let r = Repository::open(td.path()).unwrap();
    assert_eq!(r.head().unwrap().shorthand().unwrap(), "renamed-branch");
    assert!(r.find_branch(&old, git2::BranchType::Local).is_err());
}

#[test]
fn ctrl_s_opens_stash_options_menu_on_uncommitted_node() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "initial");
    fs::write(td.path().join("a.txt"), "changed").unwrap();
    let mut app = make_app(repo);
    assert!(app.is_uncommitted_selected());
    app.focused_panel = FocusedPanel::CommitDetail;

    app.handle_action(Action::StashStaged).unwrap();
    match &app.mode {
        AppMode::CommitMenu { items, .. } => assert_eq!(
            items,
            &vec![
                CommitMenuItem::StashPushStaged,
                CommitMenuItem::StashPushAll,
                CommitMenuItem::StashPushUntracked,
            ],
        ),
        other => panic!("expected a stash options CommitMenu, got {:?}", other),
    }
}

#[test]
fn branch_from_stash_via_input_creates_branch_and_drops_stash() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "original", "root");
    fs::write(td.path().join("a.txt"), "changed").unwrap();
    {
        let mut r = Repository::open(td.path()).unwrap();
        let sig = Signature::now("Test User", "test@example.com").unwrap();
        r.stash_save(&sig, "wip", None).unwrap();
    }
    let mut app = make_app(repo);
    let stash_idx = app
        .graph_layout
        .nodes
        .iter()
        .position(|n| n.is_stash)
        .expect("a stash node is present");
    app.graph_nav.graph_list_state.select(Some(stash_idx));

    app.mode = AppMode::Input {
        title: "Branch from stash".to_string(),
        input: "stash-work".to_string(),
        action: InputAction::BranchFromStash { index: 0 },
    };
    app.handle_action(Action::Confirm).unwrap();

    assert!(matches!(app.mode, AppMode::Normal));
    let r = Repository::open(td.path()).unwrap();
    assert!(
        r.find_branch("stash-work", git2::BranchType::Local).is_ok(),
        "a branch was created from the stash"
    );
    assert_eq!(stash_count(td.path()), 0, "the stash was consumed");
}

#[test]
fn commit_menu_branch_tip_includes_branch_ops() {
    let (_td, repo) = init_repo();
    let c1 = commit_file(repo.repo(), "a.txt", "a", "first");
    repo.repo()
        .branch("feature", &repo.repo().find_commit(c1).unwrap(), false)
        .unwrap();
    commit_file(repo.repo(), "b.txt", "b", "second");
    let mut app = make_app(repo);

    // Select the "feature" branch tip — a non-HEAD local branch on its own node.
    let pos = app
        .graph_nav
        .branch_positions
        .iter()
        .position(|(_, n)| n == "feature")
        .unwrap();
    app.graph_nav.selected_branch_position = Some(pos);
    let (node_idx, _) = app.graph_nav.branch_positions[pos];
    app.graph_nav.graph_list_state.select(Some(node_idx));

    app.handle_action(Action::OpenCommitMenu).unwrap();
    match &app.mode {
        AppMode::CommitMenu { items, .. } => {
            for expected in [
                CommitMenuItem::Push,
                CommitMenuItem::MergeIntoCurrent,
                CommitMenuItem::Rebase,
                CommitMenuItem::DeleteBranch,
            ] {
                assert!(
                    items.contains(&expected),
                    "branch-tip menu should include {:?}, got {:?}",
                    expected,
                    items
                );
            }
        }
        other => panic!("expected a CommitMenu, got {:?}", other),
    }
}

#[test]
fn commit_menu_non_tip_excludes_branch_ops() {
    let (_td, repo) = init_repo();
    let c1 = commit_file(repo.repo(), "a.txt", "a", "first");
    commit_file(repo.repo(), "b.txt", "b", "second");
    let mut app = make_app(repo);

    // Select the first commit, which has no branch pointing at it.
    let c1_idx = app
        .graph_layout
        .nodes
        .iter()
        .position(|n| n.commit.as_ref().map(|c| c.oid) == Some(c1))
        .unwrap();
    app.graph_nav.graph_list_state.select(Some(c1_idx));
    app.graph_nav.selected_branch_position = None;

    app.handle_action(Action::OpenCommitMenu).unwrap();
    match &app.mode {
        AppMode::CommitMenu { items, .. } => {
            for absent in [
                CommitMenuItem::Push,
                CommitMenuItem::MergeIntoCurrent,
                CommitMenuItem::Rebase,
                CommitMenuItem::DeleteBranch,
            ] {
                assert!(
                    !items.contains(&absent),
                    "non-tip menu should exclude {:?}, got {:?}",
                    absent,
                    items
                );
            }
            // The commit-level actions are still present.
            assert!(items.contains(&CommitMenuItem::Checkout));
            assert!(items.contains(&CommitMenuItem::CherryPick));
        }
        other => panic!("expected a CommitMenu, got {:?}", other),
    }
}

// ── File-op orchestration (stage / gitignore / archive + undo) ─────

#[test]
fn undo_stage_unstages_file() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "root");
    fs::write(td.path().join("b.txt"), "new file").unwrap();
    let mut app = make_app(repo);
    app.focused_panel = FocusedPanel::Files;
    prime_uncommitted(&mut app);

    // b.txt starts untracked (unstaged).
    assert!(path_status(td.path(), "b.txt").contains(Status::WT_NEW));

    app.handle_action(Action::ToggleStage).unwrap();
    assert!(
        path_status(td.path(), "b.txt").contains(Status::INDEX_NEW),
        "b.txt is staged after ToggleStage"
    );
    assert!(app.last_undoable_op.is_some());

    app.handle_action(Action::UndoLastFileOp).unwrap();
    let st = path_status(td.path(), "b.txt");
    assert!(st.contains(Status::WT_NEW), "undo returned b.txt to unstaged");
    assert!(!st.contains(Status::INDEX_NEW), "undo removed the staged entry");
    assert!(app.last_undoable_op.is_none(), "undo cleared the undo slot");
}

#[test]
fn undo_gitignore_removes_pattern() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "root");
    fs::write(td.path().join("b.txt"), "new file").unwrap();
    let mut app = make_app(repo);
    app.focused_panel = FocusedPanel::Files;
    prime_uncommitted(&mut app);

    app.handle_action(Action::AddToGitignore).unwrap();
    let gitignore = || fs::read_to_string(td.path().join(".gitignore")).unwrap_or_default();
    assert!(
        gitignore().lines().any(|l| l.trim() == "b.txt"),
        "pattern added to .gitignore"
    );
    assert!(app.last_undoable_op.is_some());

    app.handle_action(Action::UndoLastFileOp).unwrap();
    assert!(
        !gitignore().lines().any(|l| l.trim() == "b.txt"),
        "pattern removed from .gitignore on undo"
    );
    assert!(app.last_undoable_op.is_none());
}

#[test]
fn undo_archive_restores_file() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "root");
    fs::write(td.path().join("b.txt"), "keep me").unwrap();
    let mut app = make_app(repo);
    app.focused_panel = FocusedPanel::Files;
    prime_uncommitted(&mut app);

    app.handle_action(Action::ArchiveFile).unwrap();
    assert!(!td.path().join("b.txt").exists(), "file moved out of the working tree");
    assert!(td.path().join(".archive/b.txt").exists(), "file moved into .archive/");
    assert!(app.last_undoable_op.is_some());

    app.handle_action(Action::UndoLastFileOp).unwrap();
    assert!(
        td.path().join("b.txt").exists(),
        "undo restored the file to its original path"
    );
    assert!(
        !td.path().join(".archive/b.txt").exists(),
        "file removed from .archive/ on undo"
    );
    assert_eq!(fs::read_to_string(td.path().join("b.txt")).unwrap(), "keep me");
    assert!(app.last_undoable_op.is_none());
}

#[test]
fn gitignore_file_selection_uses_file_path() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "root");
    fs::write(td.path().join("junk.log"), "noise").unwrap();
    let mut app = make_app(repo);
    app.focused_panel = FocusedPanel::Files;
    prime_uncommitted(&mut app);

    app.handle_action(Action::AddToGitignore).unwrap();
    let contents = fs::read_to_string(td.path().join(".gitignore")).unwrap();
    assert!(
        contents.lines().any(|l| l.trim() == "junk.log"),
        "gitignore got the file path, was: {contents:?}"
    );
}

#[test]
fn gitignore_folder_header_selection_uses_folder_pattern() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "root");
    fs::create_dir_all(td.path().join("build")).unwrap();
    fs::write(td.path().join("build/out1.o"), "x").unwrap();
    fs::write(td.path().join("build/out2.o"), "y").unwrap();
    let mut app = make_app(repo);
    app.focused_panel = FocusedPanel::Files;
    app.files_pane.files_group_by_folder = true;
    prime_uncommitted(&mut app);

    // Select the "build/" folder header (a header, not a file).
    let header_idx = app
        .files_pane
        .display_items()
        .iter()
        .position(|i| matches!(i, FilesPaneItem::FolderHeader(t) if t == "build/"))
        .expect("build/ folder header is present");
    app.files_pane.select_file_at(header_idx);

    app.handle_action(Action::AddToGitignore).unwrap();
    let contents = fs::read_to_string(td.path().join(".gitignore")).unwrap();
    assert!(
        contents.lines().any(|l| l.trim() == "build/"),
        "gitignore got the folder pattern, was: {contents:?}"
    );
}

#[test]
fn archive_moves_file_and_gitignores_archive_dir() {
    let (td, repo) = init_repo();
    commit_file(repo.repo(), "a.txt", "a", "root");
    fs::write(td.path().join("scratch.txt"), "temp").unwrap();
    let mut app = make_app(repo);
    app.focused_panel = FocusedPanel::Files;
    prime_uncommitted(&mut app);

    app.handle_action(Action::ArchiveFile).unwrap();

    assert!(!td.path().join("scratch.txt").exists(), "file moved out of working tree");
    assert_eq!(
        fs::read_to_string(td.path().join(".archive/scratch.txt")).unwrap(),
        "temp"
    );
    let contents = fs::read_to_string(td.path().join(".gitignore")).unwrap();
    assert!(
        contents.lines().any(|l| l.trim() == ".archive"),
        "the archive dir is gitignored, was: {contents:?}"
    );
}

// ── Branch filtering: hiding a branch removes its commits ────────────

/// A repo where `feature` branches off a shared root (`c1`) and adds one
/// exclusive commit (`f1`), while HEAD stays on `main` at `c2`.
/// Returns (tempdir, app, shared_c1, feature_f1, main_c2).
fn repo_with_feature_branch() -> (TempDir, App, Oid, Oid, Oid) {
    let (td, repo) = init_repo();
    let c1 = commit_file(repo.repo(), "a.txt", "a", "shared root");
    let c2 = commit_file(repo.repo(), "b.txt", "b", "main work"); // HEAD = main @ c2
    // Branch `feature` off the shared root, then add a commit reachable only
    // through `feature` (HEAD and the working tree are left untouched).
    repo.repo()
        .branch("feature", &repo.repo().find_commit(c1).unwrap(), false)
        .unwrap();
    let f1 = commit_to_ref(repo.repo(), "refs/heads/feature", "f.txt", "f", "feature work");
    let app = make_app(repo);
    (td, app, c1, f1, c2)
}

fn commit_oids(app: &App) -> Vec<Oid> {
    app.commits.iter().map(|c| c.oid).collect()
}

#[test]
fn hiding_a_branch_removes_exclusive_commits_but_keeps_shared_ancestors() {
    let (_td, mut app, c1, f1, c2) = repo_with_feature_branch();

    // Baseline: the exclusive commit is present before hiding.
    assert!(commit_oids(&app).contains(&f1));

    app.hidden_branches.insert("feature".to_string());
    app.refresh(true).unwrap();

    let oids = commit_oids(&app);
    assert!(!oids.contains(&f1), "feature's exclusive commit must vanish");
    assert!(oids.contains(&c1), "shared ancestor must remain");
    assert!(oids.contains(&c2), "main's own commit must remain");
    // The hidden commit is gone from the rendered graph nodes too.
    assert!(
        app.graph_layout
            .nodes
            .iter()
            .all(|n| n.commit.as_ref().map(|c| c.oid) != Some(f1)),
        "hidden commit must not appear as a graph node"
    );
}

#[test]
fn unhiding_a_branch_brings_its_commits_back() {
    let (_td, mut app, _c1, f1, _c2) = repo_with_feature_branch();

    app.hidden_branches.insert("feature".to_string());
    app.refresh(true).unwrap();
    assert!(!commit_oids(&app).contains(&f1));

    app.hidden_branches.remove("feature");
    app.refresh(true).unwrap();
    assert!(
        commit_oids(&app).contains(&f1),
        "unhiding the branch restores its commits"
    );
}

#[test]
fn hiding_all_branches_still_shows_head_history() {
    let (_td, mut app, c1, f1, c2) = repo_with_feature_branch();

    // Hide literally every branch.
    let all: Vec<String> = app.branches.iter().map(|b| b.name.clone()).collect();
    for name in all {
        app.hidden_branches.insert(name);
    }
    app.refresh(true).unwrap();

    let oids = commit_oids(&app);
    // HEAD (main @ c2) and its ancestor are still walked via the HEAD push.
    assert!(oids.contains(&c2), "HEAD commit must remain");
    assert!(oids.contains(&c1), "HEAD's ancestor must remain");
    // feature is hidden and its exclusive commit is unreachable from HEAD.
    assert!(!oids.contains(&f1));
    // No panic / empty-crash: the graph still has nodes.
    assert!(!app.graph_layout.nodes.is_empty());
}

#[test]
fn selection_survives_branch_hide_refresh() {
    let (_td, mut app, _c1, _f1, c2) = repo_with_feature_branch();

    // Select the HEAD/main commit node (c2), which stays visible.
    let c2_idx = app
        .graph_layout
        .nodes
        .iter()
        .position(|n| n.commit.as_ref().map(|c| c.oid) == Some(c2))
        .unwrap();
    app.graph_nav.graph_list_state.select(Some(c2_idx));

    app.hidden_branches.insert("feature".to_string());
    app.refresh(true).unwrap();

    // Selection stays in bounds and still points at the same commit.
    let sel = app.graph_nav.graph_list_state.selected().unwrap();
    assert!(sel < app.graph_layout.nodes.len(), "selection stays in bounds");
    assert_eq!(
        app.graph_layout.nodes[sel].commit.as_ref().map(|c| c.oid),
        Some(c2),
        "the selected commit is preserved across the shrinking refresh"
    );
}

// ── Startup merged-branch classification (#78) ──────────────────────

/// Startup must NOT classify merged branches synchronously in dim-only mode
/// (the default): classification is O(branches × tree diffs) and was costing
/// over a second of time-to-first-frame on branchy repos. The contract:
/// `App::from_repo` returns with an empty merged set, having already kicked
/// the background classifier, and polling `update_merged_classification`
/// delivers the real set (rebuilding the graph) shortly after.
#[test]
fn startup_defers_merged_classification_to_the_background() {
    let (td, repo) = init_repo();
    let a = commit_file(repo.repo(), "a.txt", "a", "initial");
    // `topic` at an ancestor of the base tip → unambiguously merged.
    repo.repo().reference("refs/heads/topic", a, true, "topic").unwrap();
    commit_file(repo.repo(), "b.txt", "b", "advance base");
    // The default branch name may be master or main; either is a valid base.
    let mut app = App::from_repo(GitRepository::open(td.path()).unwrap()).unwrap();

    assert!(
        app.merged.branches.is_empty(),
        "init must not classify synchronously (dim-only mode): {:?}",
        app.merged.branches
    );

    // The classifier was kicked at init: poll until it delivers.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if app.update_merged_classification() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "background classification never delivered"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        app.merged.branches.contains("topic"),
        "async classification finds the merged branch: {:?}",
        app.merged.branches
    );
}
