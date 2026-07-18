//! Construction: load repo data and assemble the `App`.

use super::*;

impl App {
    pub(crate) fn working_tree_status_snapshot(
        repo: &GitRepository,
    ) -> (Option<WorkingTreeStatus>, Option<String>) {
        match repo.get_working_tree_status() {
            Ok(status) => (status, None),
            Err(e) => (None, Some(format!("Working tree status failed: {e}"))),
        }
    }

    /// Create an App from a given repository (for testing and embedding)
    pub fn from_repo(repo: GitRepository) -> Result<Self> {
        let repo_path = repo.path.clone();
        let fs_watcher = crate::watcher::FsWatcher::new(std::path::Path::new(&repo_path));
        // No terminal to query in the embedded/test path.
        Self::build(repo, Config::default(), UiState::default(), fs_watcher, None, None)
    }

    /// Shared constructor: loads repo data and assembles the App.
    fn build(
        mut repo: GitRepository,
        config: Config,
        ui_state: UiState,
        watcher: Option<crate::watcher::FsWatcher>,
        pending_watcher: Option<crate::watcher::PendingFsWatcher>,
        terminal_bg: Option<(u8, u8, u8)>,
    ) -> Result<Self> {
        let now = Instant::now();
        let repo_path = repo.path.clone();
        let head_name = repo.head_name();
        let head_detached = repo.is_head_detached();

        let stashes = repo.get_stashes();
        // No branches are hidden at startup, so all branches are visible.
        let branches = repo.get_branches()?;
        let remotes = repo.remotes();
        let tags = repo.get_tags();
        let commits = repo.get_commits(500, &branches, &stashes)?;
        let (working_tree_status, initial_message) = Self::working_tree_status_snapshot(&repo);
        let initial_message_time = initial_message.as_ref().map(|_| now);
        let op_state = repo.operation_state();
        let conflict_count = repo.conflicted_count();
        let uncommitted_count = working_tree_status
            .as_ref()
            .map(|s| s.accurate_file_count());
        let head_commit_oid = repo.head_oid();
        let graph_layout =
            build_graph(&commits, &branches, &tags, &stashes, uncommitted_count, head_commit_oid);

        let mut graph_nav = GraphNav::new();
        graph_nav.rebuild_branch_positions(&graph_layout);
        let has_uncommitted_node = graph_layout
            .nodes
            .first()
            .is_some_and(|node| node.is_uncommitted);
        if !has_uncommitted_node && !graph_nav.branch_positions.is_empty() {
            graph_nav.selected_branch_position = Some(0);
        }

        Ok(Self {
            mode: AppMode::Normal,
            repo,
            repo_path,
            head_name,
            head_detached,
            commits,
            branches,
            remotes,
            graph_layout,
            graph_generation: 0,
            graph_nav,
            focused_panel: if ui_state.side_panel_layout {
                FocusedPanel::Files
            } else {
                FocusedPanel::Graph
            },
            files_pane: FilesPaneState::new(),
            hidden_branches: std::collections::HashSet::new(),
            commit_editor: crate::text_editor::TextEditor::new(),
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
            op_state,
            conflict_count,
            diff_cache: DiffCache::new(),
            compare_marked: None,
            compare_range: None,
            sig_status_cache: std::collections::HashMap::new(),
            should_quit: false,
            pending_refresh: false,
            diff_viewport_height: 40,
            diff_viewport_width: 80,
            message: initial_message,
            message_time: initial_message_time,
            toasts: crate::toast::ToastQueue::new(),
            pr_toasts_armed: false,
            network: NetworkManager::new(),
            open_prs: std::collections::HashMap::new(),
            pr_fetch: crate::pr::PrFetch::new(),
            last_pull: None,
            check_fetch: crate::checks::CheckFetch::new(),
            ci_checks: None,
            thread_fetch: crate::pr_thread::PrThreadFetch::new(),
            pr_thread: None,
            pr_editor: crate::text_editor::TextEditor::new(),
            pr_action_runner: crate::pr_action::PrActionRunner::new(),
            watcher,
            pending_watcher,
            last_undoable_op: None,
            side_panel_layout: ui_state.side_panel_layout,
            metadata_columns: ui_state.metadata_columns,
            graph_width_cap: ui_state.graph_width_cap,
            debug_keys: false,
            config,
            terminal_bg,
            pixel_graph: None,
            pixel_specs_cache: None,
        })
    }

    /// Detect the terminal's background color once, as (r, g, b).
    /// Returns `None` if the terminal doesn't support the query.
    fn detect_terminal_bg() -> Option<(u8, u8, u8)> {
        terminal_light::background_color()
            .ok()
            .map(|c| c.rgb())
            .map(|rgb| (rgb.r, rgb.g, rgb.b))
    }

    /// Create a new application
    pub fn new() -> Result<Self> {
        // The OSC-11 background-color query blocks on the terminal reply
        // (typically 5-15ms, worst case 100ms); overlap it with repository
        // loading. Joined below even on the error path, so the query's
        // temporary raw-mode toggle can't outlive main.
        let bg_query = std::thread::spawn(Self::detect_terminal_bg);

        let load = || -> Result<Self> {
            let config = Config::load();
            let ui_state = UiState::load();
            let repo = GitRepository::discover()?;
            // Registering recursive inotify watches walks the whole working
            // tree (hundreds of ms on large repos) — build off-thread and
            // install from the event loop once ready.
            let pending_watcher =
                crate::watcher::FsWatcher::spawn(std::path::Path::new(&repo.path));
            Self::build(repo, config, ui_state, None, Some(pending_watcher), None)
        };
        let result = load();
        let terminal_bg = bg_query.join().ok().flatten();

        let mut app = result?;
        app.terminal_bg = terminal_bg;
        Ok(app)
    }
}
