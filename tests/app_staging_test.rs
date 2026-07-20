//! Integration tests for file staging selection behavior and diff cache
//! interaction through App. Uses real git repos.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use chrono::Local;
use git2::{Oid, Repository, Signature};
use tempfile::TempDir;

use keifu::action::Action;
use keifu::app::{App, AppMode, FocusedPanel, SearchState};
use keifu::config::Config;
use keifu::diff_cache::{DiffCache, DiffResult, DiffTarget, DIFF_LOAD_DEBOUNCE};
use keifu::files_pane_state::{FileSelection, FilesPaneItem, FilesPaneState, section_of};
use keifu::git::graph::{CellType, GraphLayout, GraphNode};
use keifu::git::operations::{stage_file, unstage_file};
use keifu::git::{
    CommitDiffInfo, CommitInfo, FileChangeKind, FileDiffInfo, GitRepository, StageStatus,
    WorkingTreeStatus,
};
use keifu::graph_nav::GraphNav;
use keifu::network::NetworkManager;
use keifu::text_editor::TextEditor;

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
    let oid = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )
        .unwrap();
    drop(tree);
    oid
}

fn make_commit(oid: Oid) -> CommitInfo {
    CommitInfo {
        oid,
        short_id: oid.to_string()[..7].to_string(),
        author_name: "Test User".to_string(),
        author_email: "test@example.com".to_string(),
        timestamp: Local::now(),
        message: "test".to_string(),
        full_message: "test".to_string(),
        parent_oids: Vec::new(),
    }
}

fn make_base_app(
    node: GraphNode,
    diff_target: DiffTarget,
    working_tree_status: Option<WorkingTreeStatus>,
) -> App {
    let (_tempdir, repo) = init_repo();
    let commits = node.commit.iter().cloned().collect();

    App {
        mode: AppMode::Normal,
        repo_path: repo.path.clone(),
        repo,
        head_name: None,
        head_detached: false,
        commits,
        commit_load_limit: 500,
        all_commits_loaded: true,
        branches: Vec::new(),
        remotes: Vec::new(),
        graph_layout: GraphLayout {
            nodes: vec![node],
            max_lane: 0,
        },
        graph_generation: 0,
        graph_nav: GraphNav::new(),
        focused_panel: FocusedPanel::Graph,
        files_pane: FilesPaneState::new(),
        hidden_branches: HashSet::new(),
        branch_authors: std::collections::HashMap::new(),
        branch_authors_key: Vec::new(),
        commit_editor: TextEditor::new(),
        editing_commit_message: false,
        amending_commit: false,
        commit_detail_scroll: 0,
        commit_detail_max_scroll: 0,
        commit_editor_line_offset: 0,
        commit_detail_visible_rows: 20,
        commit_filter: String::new(),
        commit_filter_active: false,
        visible_commit_indices: Vec::new(),
        search_state: SearchState::default(),
        working_tree_status,
        op_state: keifu::git::OperationState::Clean,
        conflict_count: 0,
        diff_cache: {
            let mut dc = DiffCache::new();
            dc.selected_diff_target = Some(diff_target);
            dc.selected_diff_target_changed_at = Instant::now() - DIFF_LOAD_DEBOUNCE;
            dc
        },
        compare_marked: None,
        compare_range: None,
        sig_status_cache: std::collections::HashMap::new(),
        should_quit: false,
        pending_refresh: false,
        diff_viewport_height: 40,
        diff_viewport_width: 80,
        diff_word_wrap: false,
        diff_source: None,
        message: None,
        message_time: None,
        message_sticky: false,
        wt_status_error_latched: false,
        auto_refresh_error_latched: false,
        watch_refresh_error_latched: false,
        toasts: keifu::toast::ToastQueue::new(),
        pr_toasts_armed: false,
        network: NetworkManager::new(),
        credentials: std::collections::HashMap::new(),
        in_flight_op: None,
        pending_auth: None,
        open_prs: std::collections::HashMap::new(),
        pr_fetch: keifu::pr::PrFetch::new(),
        merged_pr_branches: std::collections::HashSet::new(),
        merged_branch_fetch: keifu::merged_branches::MergedBranchFetch::new(),
        last_pull: None,
        pre_pull_head: None,
        undo_ledger: keifu::undo::UndoLedger::default(),
        check_fetch: keifu::checks::CheckFetch::new(),
        ci_checks: None,
        thread_fetch: keifu::pr_thread::PrThreadFetch::new(),
        pr_thread: None,
        pr_editor: keifu::text_editor::TextEditor::new(),
        pr_action_runner: keifu::pr_action::PrActionRunner::new(),
        issue_fetch: keifu::issue::IssueFetch::new(),
        issue_action_runner: keifu::issue_action::IssueActionRunner::new(),
        issue_list: None,
        issue_detail: None,
        issue_editor: keifu::text_editor::TextEditor::new(),
        issue_label_picker: None,
        issue_label_filter: None,
        pending_external_edit: None,
        avatar_fetch: keifu::avatar_fetch::AvatarFetch::new(),
        avatar_enqueued_generation: None,
        watcher: None,
        pending_watcher: None,
        watcher_disconnected: false,
        last_undoable_op: None,
        side_panel_layout: false,
        hide_remote_branches: false,
        merged_branches: std::collections::HashSet::new(),
        merged_classify: keifu::merged_branches::MergedClassifier::new(),
        hide_merged_branches: false,
        metadata_columns: keifu::config::MetadataColumns::default(),
        graph_width_cap: None,
        debug_keys: false,
        perf: keifu::perf::PerfStats::default(),
        mouse_layout: Default::default(),
        last_click: None,
        files_view_offset: 0,
        menu_anchor: None,
        popup_rect: None,
        graph_chip_hits: Vec::new(),
        status_hints: Vec::new(),
        graph_split_ratio: 65,
        dragging_divider: false,
        trace_enabled: true,
        config: Config::default(),
        terminal_bg: None,
        pixel_graph: None,
        pixel_specs_cache: None,
    }
}

fn make_diff_app(selected_oid: Oid, in_flight_oid: Option<Oid>) -> App {
    let node = GraphNode {
        commit: Some(make_commit(selected_oid)),
        lane: 0,
        color_index: 0,
        branch_names: Vec::new(),
        tag_names: Vec::new(),
        is_head: false,
        is_uncommitted: false,
        is_stash: false,
        stash_label: None,
        uncommitted_count: None,
        cells: vec![CellType::Commit(0)],
        cell_oids: Vec::new(),
    };
    let mut app = make_base_app(node, DiffTarget::Commit(selected_oid), None);
    app.diff_cache.diff_loading_oid = in_flight_oid;
    app
}

fn make_uncommitted_app() -> App {
    let node = GraphNode {
        commit: None,
        lane: 0,
        color_index: 0,
        branch_names: Vec::new(),
        tag_names: Vec::new(),
        is_head: false,
        is_uncommitted: true,
        is_stash: false,
        stash_label: None,
        uncommitted_count: Some(1),
        cells: vec![CellType::Commit(0)],
        cell_oids: Vec::new(),
    };
    let wts = WorkingTreeStatus {
        file_paths: vec![PathBuf::from("tracked.txt")],
        mtime_hash: 1,
        has_collapsed_untracked_dirs: false,
    };
    make_base_app(node, DiffTarget::Uncommitted, Some(wts))
}

fn make_test_app(tempdir: &Path) -> App {
    let git_repo = GitRepository::open(tempdir).unwrap();
    let mut app = App::from_repo(git_repo).unwrap();
    app.repo_path = tempdir.to_string_lossy().to_string();
    app.graph_nav.graph_list_state.select(Some(0));
    app.diff_cache.set_quick_uncommitted(app.repo.repo());
    app.sync_file_list_cache();
    app
}

fn selected_file_path(app: &App) -> String {
    let idx = app.file_selected_index();
    let items = app.display_items();
    match &items[idx] {
        FilesPaneItem::File(f) => f.path.to_string_lossy().to_string(),
        FilesPaneItem::SectionHeader(t) | FilesPaneItem::FolderHeader(t) => {
            panic!("selected a header: {}", t)
        }
    }
}

fn selected_section(app: &App) -> Option<String> {
    let idx = app.file_selected_index();
    section_of(app.display_items(), idx).map(|s| s.to_string())
}

// ── Diff cache through App ──────────────────────────────────────────

#[test]
fn selected_diff_stays_loading_while_another_diff_is_in_flight() {
    let selected_oid = Oid::from_str("1111111111111111111111111111111111111111").unwrap();
    let in_flight_oid = Oid::from_str("2222222222222222222222222222222222222222").unwrap();
    let app = make_diff_app(selected_oid, Some(in_flight_oid));
    assert!(app.is_diff_loading());
}

#[test]
fn cached_selected_diff_is_not_marked_loading_by_other_requests() {
    let selected_oid = Oid::from_str("1111111111111111111111111111111111111111").unwrap();
    let in_flight_oid = Oid::from_str("2222222222222222222222222222222222222222").unwrap();
    let mut app = make_diff_app(selected_oid, Some(in_flight_oid));
    app.diff_cache.diff_cache = Some(CommitDiffInfo::default());
    app.diff_cache.diff_cache_oid = Some(selected_oid);
    assert!(!app.is_diff_loading());
}

#[test]
fn failed_commit_diff_load_is_cached_to_avoid_immediate_retry() {
    let selected_oid = Oid::from_str("1111111111111111111111111111111111111111").unwrap();
    let mut app = make_diff_app(selected_oid, Some(selected_oid));
    let (tx, rx) = mpsc::channel();
    tx.send(DiffResult {
        oid: selected_oid,
        diff: Err("boom".to_string()),
    })
    .unwrap();
    app.diff_cache.diff_receiver = Some(rx);

    app.update_diff_cache();
    app.update_diff_cache();

    assert!(app.diff_cache.diff_cache.as_ref().is_none());
    assert_eq!(app.diff_cache.diff_cache_oid, Some(selected_oid));
    assert!(app.cached_diff().is_none());
    assert!(!app.is_diff_loading());
    assert!(app.diff_cache.diff_loading_oid.is_none());
    assert!(app.diff_cache.diff_receiver.is_none());
    assert_eq!(app.message.as_deref(), Some("Failed to load diff: boom"));
}

#[test]
fn failed_uncommitted_diff_load_is_cached_to_avoid_immediate_retry() {
    let mut app = make_uncommitted_app();
    let (tx, rx) = mpsc::channel();
    let cache_key = app.working_tree_status.clone();
    tx.send((Err("boom".to_string()), cache_key)).unwrap();
    app.diff_cache.uncommitted_diff_loading = true;
    app.diff_cache.uncommitted_diff_receiver = Some(rx);

    app.update_diff_cache();
    app.update_diff_cache();

    assert!(app
        .diff_cache
        .cached_diff(Some(DiffTarget::Uncommitted))
        .is_none());
    assert!(app.diff_cache.uncommitted_diff_failed);
    assert!(app.cached_diff().is_none());
    assert!(!app.is_diff_loading());
    assert!(!app.diff_cache.uncommitted_diff_loading);
    assert!(app.diff_cache.uncommitted_diff_receiver.is_none());
    assert_eq!(app.message.as_deref(), Some("Failed to load diff: boom"));
}

// ── Refresh behavior ────────────────────────────────────────────────

#[test]
fn refresh_reuses_uncommitted_cache_for_nested_untracked_directories() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    let _oid = commit_file(&repo, "tracked.txt", "tracked\n", "initial");
    fs::create_dir_all(tempdir.path().join("dir/sub")).unwrap();
    fs::write(tempdir.path().join("dir/sub/file.txt"), "hello\n").unwrap();

    let git_repo = GitRepository::open(tempdir.path()).unwrap();
    let mut app = App::from_repo(git_repo).unwrap();
    app.diff_cache
        .uncommitted_diff_cache = Some(CommitDiffInfo::default());
    app.diff_cache
        .uncommitted_cache_key = app.working_tree_status.clone();

    app.refresh(false).unwrap();

    assert!(app
        .diff_cache
        .cached_diff(Some(DiffTarget::Uncommitted))
        .is_some());
    assert!(app.diff_cache.uncommitted_cache_key.as_ref().is_some());
}

#[test]
fn refresh_restores_non_branch_selection_by_commit_oid_when_uncommitted_row_is_added() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    let first_oid = commit_file(&repo, "tracked.txt", "first\n", "first");
    let _second_oid = commit_file(&repo, "tracked.txt", "second\n", "second");

    let git_repo = GitRepository::open(tempdir.path()).unwrap();
    let mut app = App::from_repo(git_repo).unwrap();

    let first_node_idx = app
        .graph_layout
        .nodes
        .iter()
        .position(|node| {
            node.commit
                .as_ref()
                .is_some_and(|commit| commit.oid == first_oid)
        })
        .unwrap();
    app.graph_nav.graph_list_state.select(Some(first_node_idx));
    app.graph_nav.sync_branch_selection_to_node(first_node_idx);

    fs::write(tempdir.path().join("untracked.txt"), "hello\n").unwrap();
    app.refresh(false).unwrap();

    let selected_oid = app
        .graph_nav
        .graph_list_state
        .selected()
        .and_then(|idx| app.graph_layout.nodes.get(idx))
        .and_then(|node| node.commit.as_ref())
        .map(|commit| commit.oid);

    assert_eq!(selected_oid, Some(first_oid));
    assert!(app
        .graph_layout
        .nodes
        .first()
        .is_some_and(|node| node.is_uncommitted));
}

// ── Integration tests: staging with real git repos ──────────────────

#[test]
fn integration_stage_modified_with_untracked_selects_untracked() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    commit_file(&repo, "tracked.txt", "v1\n", "initial");

    fs::write(tempdir.path().join("tracked.txt"), "v2\n").unwrap();
    fs::write(tempdir.path().join("untracked.txt"), "new\n").unwrap();

    let mut app = make_test_app(tempdir.path());
    app.files_pane.file_selection = FileSelection {
        section: Some("Unstaged Changes".to_string()),
        path: Some("tracked.txt".into()),
    };
    app.sync_file_list_cache();
    assert_eq!(selected_file_path(&app), "tracked.txt");

    stage_file(&app.repo_path, "tracked.txt").unwrap();
    app.refresh_after_file_op().unwrap();

    assert_eq!(
        selected_section(&app).as_deref(),
        Some("Unstaged Changes"),
    );
    assert_eq!(selected_file_path(&app), "untracked.txt");
}

#[test]
fn selected_file_repo_path_returns_repo_relative_path() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    commit_file(&repo, "tracked.txt", "v1\n", "initial");

    // An untracked file nested in a subdirectory, so "repo-relative" is
    // meaningful (not just a bare filename).
    fs::create_dir_all(tempdir.path().join("src")).unwrap();
    fs::write(tempdir.path().join("src/new.txt"), "hi\n").unwrap();

    let mut app = make_test_app(tempdir.path());
    app.focused_panel = FocusedPanel::Files;
    app.files_pane.file_selection = FileSelection {
        section: Some("Unstaged Changes".to_string()),
        path: Some("src/new.txt".into()),
    };
    app.sync_file_list_cache();

    // The path resolver returns the selected file's repo-relative path ...
    assert_eq!(
        app.selected_file_repo_path().as_deref(),
        Some("src/new.txt")
    );

    // ... and the CopyPath action runs the handler and reports the outcome as
    // a toast (clipboard shell-out may fail headless, so we assert a toast was
    // produced — success or clipboard error — not the clipboard contents).
    app.handle_action(Action::CopyPath).unwrap();
    assert!(
        !app.toasts.visible().is_empty(),
        "CopyPath should produce a toast"
    );
    assert!(matches!(app.mode, AppMode::Normal));
}

#[test]
fn integration_stage_only_unstaged_selects_staged() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    commit_file(&repo, "a.txt", "v1\n", "initial");

    fs::write(tempdir.path().join("a.txt"), "v2\n").unwrap();

    let mut app = make_test_app(tempdir.path());
    app.files_pane.file_selection = FileSelection {
        section: Some("Unstaged Changes".to_string()),
        path: Some("a.txt".into()),
    };
    app.sync_file_list_cache();

    stage_file(&app.repo_path, "a.txt").unwrap();
    app.refresh_after_file_op().unwrap();

    assert_eq!(selected_file_path(&app), "a.txt");
    assert_eq!(selected_section(&app).as_deref(), Some("Staged Changes"));
}

#[test]
fn integration_unstage_selects_next_staged() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    commit_file(&repo, "a.txt", "v1\n", "initial");

    fs::write(tempdir.path().join("a.txt"), "v2\n").unwrap();
    fs::write(tempdir.path().join("b.txt"), "new\n").unwrap();
    stage_file(tempdir.path().to_str().unwrap(), "a.txt").unwrap();
    stage_file(tempdir.path().to_str().unwrap(), "b.txt").unwrap();

    let mut app = make_test_app(tempdir.path());
    app.files_pane.file_selection = FileSelection {
        section: Some("Staged Changes".to_string()),
        path: Some("a.txt".into()),
    };
    app.sync_file_list_cache();

    unstage_file(&app.repo_path, "a.txt").unwrap();
    app.refresh_after_file_op().unwrap();

    assert_eq!(selected_file_path(&app), "b.txt");
    assert_eq!(selected_section(&app).as_deref(), Some("Staged Changes"));
}

#[test]
fn integration_stage_with_existing_staged_selects_next_unstaged() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    commit_file(&repo, "a.txt", "v1\n", "initial");
    commit_file(&repo, "b.txt", "v1\n", "second");

    fs::write(tempdir.path().join("a.txt"), "v2\n").unwrap();
    fs::write(tempdir.path().join("b.txt"), "v2\n").unwrap();
    fs::write(tempdir.path().join("untracked.txt"), "new\n").unwrap();
    stage_file(tempdir.path().to_str().unwrap(), "a.txt").unwrap();

    let mut app = make_test_app(tempdir.path());
    app.files_pane.file_selection = FileSelection {
        section: Some("Unstaged Changes".to_string()),
        path: Some("b.txt".into()),
    };
    app.sync_file_list_cache();

    stage_file(&app.repo_path, "b.txt").unwrap();
    app.refresh_after_file_op().unwrap();

    assert_eq!(
        selected_section(&app).as_deref(),
        Some("Unstaged Changes"),
    );
    assert_eq!(selected_file_path(&app), "untracked.txt");
}

#[test]
fn integration_stage_middle_unstaged_selects_next_unstaged() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    commit_file(&repo, "a.txt", "v1\n", "initial");
    commit_file(&repo, "b.txt", "v1\n", "second");
    commit_file(&repo, "c.txt", "v1\n", "third");

    fs::write(tempdir.path().join("a.txt"), "v2\n").unwrap();
    fs::write(tempdir.path().join("b.txt"), "v2\n").unwrap();
    fs::write(tempdir.path().join("c.txt"), "v2\n").unwrap();

    let mut app = make_test_app(tempdir.path());
    app.files_pane.file_selection = FileSelection {
        section: Some("Unstaged Changes".to_string()),
        path: Some("b.txt".into()),
    };
    app.sync_file_list_cache();

    stage_file(&app.repo_path, "b.txt").unwrap();
    app.refresh_after_file_op().unwrap();

    assert_eq!(selected_section(&app).as_deref(), Some("Unstaged Changes"));
    assert_eq!(selected_file_path(&app), "c.txt");
}

#[test]
fn integration_stage_last_unstaged_selects_prev_unstaged() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    commit_file(&repo, "a.txt", "v1\n", "initial");
    commit_file(&repo, "b.txt", "v1\n", "second");

    fs::write(tempdir.path().join("a.txt"), "v2\n").unwrap();
    fs::write(tempdir.path().join("b.txt"), "v2\n").unwrap();

    let mut app = make_test_app(tempdir.path());
    app.files_pane.file_selection = FileSelection {
        section: Some("Unstaged Changes".to_string()),
        path: Some("b.txt".into()),
    };
    app.sync_file_list_cache();

    stage_file(&app.repo_path, "b.txt").unwrap();
    app.refresh_after_file_op().unwrap();

    assert_eq!(selected_section(&app).as_deref(), Some("Unstaged Changes"));
    assert_eq!(selected_file_path(&app), "a.txt");
}

#[test]
fn integration_unstage_only_staged_falls_back_to_unstaged() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    commit_file(&repo, "a.txt", "v1\n", "initial");

    fs::write(tempdir.path().join("a.txt"), "v2\n").unwrap();
    fs::write(tempdir.path().join("b.txt"), "new\n").unwrap();
    stage_file(tempdir.path().to_str().unwrap(), "a.txt").unwrap();

    let mut app = make_test_app(tempdir.path());
    app.files_pane.file_selection = FileSelection {
        section: Some("Staged Changes".to_string()),
        path: Some("a.txt".into()),
    };
    app.sync_file_list_cache();

    unstage_file(&app.repo_path, "a.txt").unwrap();
    app.refresh_after_file_op().unwrap();

    assert_eq!(selected_file_path(&app), "a.txt");
}

#[test]
fn integration_unstage_last_staged_selects_prev_staged() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    commit_file(&repo, "a.txt", "v1\n", "initial");
    commit_file(&repo, "b.txt", "v1\n", "second");

    fs::write(tempdir.path().join("a.txt"), "v2\n").unwrap();
    fs::write(tempdir.path().join("b.txt"), "v2\n").unwrap();
    stage_file(tempdir.path().to_str().unwrap(), "a.txt").unwrap();
    stage_file(tempdir.path().to_str().unwrap(), "b.txt").unwrap();

    let mut app = make_test_app(tempdir.path());
    app.files_pane.file_selection = FileSelection {
        section: Some("Staged Changes".to_string()),
        path: Some("b.txt".into()),
    };
    app.sync_file_list_cache();

    unstage_file(&app.repo_path, "b.txt").unwrap();
    app.refresh_after_file_op().unwrap();

    assert_eq!(selected_section(&app).as_deref(), Some("Staged Changes"));
    assert_eq!(selected_file_path(&app), "a.txt");
}

#[test]
fn integration_file_deleted_selects_next_in_section() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    commit_file(&repo, "a.txt", "v1\n", "initial");

    fs::write(tempdir.path().join("a.txt"), "v2\n").unwrap();
    fs::write(tempdir.path().join("b.txt"), "new\n").unwrap();
    fs::write(tempdir.path().join("c.txt"), "new\n").unwrap();

    let mut app = make_test_app(tempdir.path());
    app.files_pane.file_selection = FileSelection {
        section: Some("Unstaged Changes".to_string()),
        path: Some("b.txt".into()),
    };
    app.sync_file_list_cache();

    fs::remove_file(tempdir.path().join("b.txt")).unwrap();
    app.refresh_after_file_op().unwrap();

    assert_eq!(selected_section(&app).as_deref(), Some("Unstaged Changes"));
    assert_eq!(selected_file_path(&app), "c.txt");
}

#[test]
fn integration_last_file_deleted_selects_prev_in_section() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    commit_file(&repo, "a.txt", "v1\n", "initial");

    fs::write(tempdir.path().join("a.txt"), "v2\n").unwrap();
    fs::write(tempdir.path().join("b.txt"), "new\n").unwrap();

    let mut app = make_test_app(tempdir.path());
    app.files_pane.file_selection = FileSelection {
        section: Some("Unstaged Changes".to_string()),
        path: Some("b.txt".into()),
    };
    app.sync_file_list_cache();

    fs::remove_file(tempdir.path().join("b.txt")).unwrap();
    app.refresh_after_file_op().unwrap();

    assert_eq!(selected_section(&app).as_deref(), Some("Unstaged Changes"));
    assert_eq!(selected_file_path(&app), "a.txt");
}

#[test]
fn integration_selection_never_lands_on_header() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    commit_file(&repo, "a.txt", "v1\n", "initial");

    fs::write(tempdir.path().join("a.txt"), "v2\n").unwrap();
    fs::write(tempdir.path().join("b.txt"), "new\n").unwrap();

    let mut app = make_test_app(tempdir.path());
    app.files_pane.file_selection = FileSelection::default();
    app.sync_file_list_cache();

    let idx = app.file_selected_index();
    let items = app.display_items();
    assert!(
        matches!(items.get(idx), Some(FilesPaneItem::File(_))),
        "selection should never land on a header, got index {}",
        idx,
    );
}

#[test]
fn integration_quick_diff_includes_untracked_files() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    commit_file(&repo, "tracked.txt", "v1\n", "initial");
    fs::write(tempdir.path().join("new_file.txt"), "hello\n").unwrap();

    let quick = CommitDiffInfo::quick_file_list_for_working_tree(&repo).unwrap();
    let paths: Vec<_> = quick
        .unstaged_files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();
    assert!(
        paths.contains(&"new_file.txt".to_string()),
        "quick diff should include untracked files, got: {:?}",
        paths
    );
}

// ── Regressions: diff viewer navigation & empty-pane refresh ────────

fn wt_file(path: &str, status: StageStatus) -> FileDiffInfo {
    FileDiffInfo {
        path: PathBuf::from(path),
        kind: FileChangeKind::Modified,
        is_binary: false,
        insertions: 1,
        deletions: 1,
        stage_status: Some(status),
    }
}

/// A partially-staged file appears in both the staged and unstaged
/// sections, so the pane shows more file entries than the deduplicated
/// diff. PrevFile used to index past the end of the deduped list.
#[test]
fn prev_file_in_diff_viewer_survives_partially_staged_files() {
    let mut app = make_uncommitted_app();
    app.diff_cache.uncommitted_diff_cache = Some(CommitDiffInfo {
        files: vec![
            wt_file("a.rs", StageStatus::Staged),
            wt_file("b.rs", StageStatus::Staged),
        ],
        staged_files: vec![
            wt_file("a.rs", StageStatus::Staged),
            wt_file("b.rs", StageStatus::Staged),
        ],
        unstaged_files: vec![
            wt_file("a.rs", StageStatus::Unstaged),
            wt_file("b.rs", StageStatus::Unstaged),
        ],
        total_files: 2,
        ..Default::default()
    });
    app.sync_file_list_cache();

    // Select the last displayed file (unstaged b.rs).
    let last_file_idx = app
        .display_items()
        .iter()
        .rposition(|item| matches!(item, FilesPaneItem::File(_)))
        .expect("pane should list files");
    app.files_pane.select_file_at(last_file_idx);
    app.focused_panel = FocusedPanel::Files;

    app.handle_action(Action::OpenFileDiff).unwrap();
    let AppMode::FileDiff { file_index, ref file_list, .. } = app.mode else {
        panic!("expected FileDiff mode, got {:?}", app.mode);
    };
    assert_eq!(file_list.len(), 4, "viewer must cycle the displayed entries");
    assert_eq!(file_index, 3);

    app.handle_action(Action::PrevFile).unwrap();
    let AppMode::FileDiff { file_index, .. } = app.mode else {
        panic!("expected FileDiff mode after PrevFile, got {:?}", app.mode);
    };
    assert_eq!(file_index, 2);
}

/// Undo can trigger a refresh while a node with no file changes is
/// selected; the selection-restore logic used to slice past the end of
/// the empty items list.
#[test]
fn refresh_after_file_op_with_empty_files_pane_does_not_panic() {
    let tempdir = tempfile::tempdir().unwrap();
    Repository::init(tempdir.path()).unwrap();
    let mut app = make_test_app(tempdir.path());
    app.focused_panel = FocusedPanel::Files;
    assert!(app.display_items().is_empty());

    app.refresh_after_file_op().unwrap();
}

/// A detached HEAD on a commit not reachable from any branch must still
/// appear in the graph (it also anchors the uncommitted-changes node).
#[test]
fn detached_orphan_head_commit_appears_in_history() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    let first = commit_file(&repo, "a.txt", "v1\n", "initial");

    // Detach at the branch tip, then commit: the new commit is reachable
    // only through HEAD.
    repo.set_head_detached(first).unwrap();
    fs::write(tempdir.path().join("a.txt"), "v2\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = Signature::now("Test User", "test@example.com").unwrap();
    let parent = repo.find_commit(first).unwrap();
    let orphan = repo
        .commit(Some("HEAD"), &sig, &sig, "detached work", &tree, &[&parent])
        .unwrap();

    let mut git_repo = GitRepository::open(tempdir.path()).unwrap();
    // The branch still points at `first`; `orphan` is reachable only via HEAD.
    let branches = git_repo.get_branches().unwrap();
    let stashes = git_repo.get_stashes();
    let commits = git_repo.get_commits(500, &branches, &stashes).unwrap();
    assert!(
        commits.iter().any(|c| c.oid == orphan),
        "detached HEAD commit missing from history"
    );
}

// ── Hunk-level staging through App.handle_action ────────────────────

/// A 20-line file with well-separated edits (line 2 and line 19) so the
/// working-tree diff yields two distinct hunks.
fn base_20() -> String {
    (1..=20).map(|i| format!("l{i}\n")).collect()
}
fn two_edits_20() -> String {
    (1..=20)
        .map(|i| match i {
            2 => "TOP\n".to_string(),
            19 => "BOTTOM\n".to_string(),
            _ => format!("l{i}\n"),
        })
        .collect()
}

fn git_out(repo_path: &str, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(repo_path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn open_two_hunk_diff(tempdir: &Path) -> App {
    let repo = Repository::init(tempdir).unwrap();
    commit_file(&repo, "f.txt", &base_20(), "base");
    fs::write(tempdir.join("f.txt"), two_edits_20()).unwrap();

    let mut app = make_test_app(tempdir);
    app.focused_panel = FocusedPanel::Files;
    app.diff_viewport_height = 40;
    app.files_pane.file_selection = FileSelection {
        section: Some("Unstaged Changes".to_string()),
        path: Some("f.txt".into()),
    };
    app.sync_file_list_cache();
    app.handle_action(Action::OpenFileDiff).unwrap();
    assert!(matches!(app.mode, AppMode::FileDiff { .. }), "diff should open");
    app
}

/// Point the viewer's cursor at hunk `idx` by scrolling to its header row.
fn target_hunk(app: &mut App, idx: usize) {
    let pos = if let AppMode::FileDiff { hunk_positions, .. } = &app.mode {
        assert_eq!(hunk_positions.len(), 2, "expected two hunks");
        hunk_positions[idx]
    } else {
        panic!("not in FileDiff mode");
    };
    if let AppMode::FileDiff { scroll_offset, .. } = &mut app.mode {
        *scroll_offset = pos;
    }
}

#[test]
fn stage_hunk_action_stages_only_the_hunk_under_cursor() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut app = open_two_hunk_diff(tempdir.path());
    let rp = app.repo_path.clone();

    // Cursor on the second hunk (BOTTOM); stage it.
    target_hunk(&mut app, 1);
    app.handle_action(Action::StageHunk).unwrap();

    let staged = git_out(&rp, &["diff", "--cached"]);
    assert!(staged.contains("+BOTTOM"), "BOTTOM should be staged:\n{staged}");
    assert!(!staged.contains("+TOP"), "TOP must stay unstaged:\n{staged}");
    // Viewer stays open on the same (still-changed) file.
    assert!(matches!(app.mode, AppMode::FileDiff { .. }));
}

#[test]
fn unstage_hunk_action_removes_that_hunk_from_the_index() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut app = open_two_hunk_diff(tempdir.path());
    let rp = app.repo_path.clone();

    // Stage the whole file first, then unstage just the TOP hunk.
    stage_file(&rp, "f.txt").unwrap();
    target_hunk(&mut app, 0);
    app.handle_action(Action::UnstageHunk).unwrap();

    let staged = git_out(&rp, &["diff", "--cached"]);
    assert!(!staged.contains("+TOP"), "TOP should be unstaged:\n{staged}");
    assert!(staged.contains("+BOTTOM"), "BOTTOM stays staged:\n{staged}");
}

#[test]
fn discard_hunk_action_routes_through_confirm_then_reverts_worktree() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut app = open_two_hunk_diff(tempdir.path());

    // Cursor on the first hunk (TOP); request discard.
    target_hunk(&mut app, 0);
    app.handle_action(Action::DiscardHunk).unwrap();
    assert!(
        matches!(app.mode, AppMode::Confirm { .. }),
        "discard must prompt for confirmation"
    );

    app.handle_action(Action::Confirm).unwrap();

    let contents = fs::read_to_string(tempdir.path().join("f.txt")).unwrap();
    assert!(!contents.contains("TOP"), "TOP reverted:\n{contents}");
    assert!(contents.contains("BOTTOM"), "BOTTOM survives:\n{contents}");
    // One hunk remains, so the viewer reopens rather than closing.
    assert!(matches!(app.mode, AppMode::FileDiff { .. }));
}

#[test]
fn cancelling_discard_hunk_returns_to_the_diff_viewer() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut app = open_two_hunk_diff(tempdir.path());

    target_hunk(&mut app, 0);
    app.handle_action(Action::DiscardHunk).unwrap();
    assert!(matches!(app.mode, AppMode::Confirm { .. }));

    // Cancelling must reopen the viewer, not drop to Normal, and leave the
    // working tree untouched.
    app.handle_action(Action::Cancel).unwrap();
    assert!(matches!(app.mode, AppMode::FileDiff { .. }), "should reopen diff");
    let contents = fs::read_to_string(tempdir.path().join("f.txt")).unwrap();
    assert!(contents.contains("TOP") && contents.contains("BOTTOM"));
}

#[test]
fn stage_all_action_stages_tracked_and_untracked_from_files_pane() {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    commit_file(&repo, "tracked.txt", "v1\n", "initial");
    fs::write(tempdir.path().join("tracked.txt"), "v2\n").unwrap();
    fs::write(tempdir.path().join("untracked.txt"), "new\n").unwrap();

    let mut app = make_test_app(tempdir.path());
    app.focused_panel = FocusedPanel::Files;
    let rp = app.repo_path.clone();

    app.handle_action(Action::StageAll).unwrap();

    let staged = git_out(&rp, &["diff", "--cached", "--name-only"]);
    assert!(staged.contains("tracked.txt") && staged.contains("untracked.txt"), "{staged}");

    app.handle_action(Action::UnstageAll).unwrap();
    assert!(git_out(&rp, &["diff", "--cached", "--name-only"]).trim().is_empty());
}

// ── Clickable status-bar hints ──────────────────────────────────────

/// End-to-end: the status bar records the "? help" hint's cell rect, and a mouse
/// click landing in that rect dispatches its action (opening help) — the same as
/// pressing the key.
#[test]
fn clicking_a_status_bar_hint_opens_help() {
    use keifu::ui::status_bar::StatusBar;
    use keifu::ui::theme::Theme;
    use ratatui::layout::Rect;

    let oid = Oid::from_str("1111111111111111111111111111111111111111").unwrap();
    let mut app = make_diff_app(oid, None);
    app.focused_panel = FocusedPanel::Graph;
    app.graph_nav.graph_list_state.select(Some(0));

    let theme = Theme::dark();
    let status_area = Rect::new(0, 29, 120, 1);
    let status_bar = StatusBar::new(&app, &theme);
    let regions = status_bar.hint_regions(status_area);

    // The graph pane's hints include "? help" → ToggleHelp.
    let (rect, _) = regions
        .iter()
        .find(|(_, action)| *action == Action::ToggleHelp)
        .expect("graph pane advertises a clickable help hint");
    let (col, row) = (rect.x + rect.width / 2, rect.y);

    // The mouse layer reads whatever the last render recorded.
    app.status_hints = regions.clone();
    assert!(matches!(app.mode, AppMode::Normal));
    app.handle_action(Action::MouseClick { col, row }).unwrap();
    assert!(
        matches!(app.mode, AppMode::Help),
        "clicking the help hint toggled help open"
    );

    // A click on the status row that misses every hint is a no-op for the hints
    // layer (help stays as it was — no accidental dispatch).
    let miss_col = status_area.width - 1;
    app.handle_action(Action::MouseClick {
        col: miss_col,
        row: status_area.y,
    })
    .unwrap();
    assert!(matches!(app.mode, AppMode::Help), "an empty-cell click dispatched nothing");
}
