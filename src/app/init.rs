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
        // Startup phase timings, folded into the exit perf summary (and the
        // live slow-op log) so "startup feels slow" reports come with their
        // own breakdown — the #78 sync-classification regression sat invisible
        // here for lack of exactly this.
        let mut perf = crate::perf::PerfStats::default();
        let mut phase_started = Instant::now();
        macro_rules! phase {
            ($name:literal) => {{
                perf.record(concat!("startup.", $name), phase_started.elapsed());
                phase_started = Instant::now();
            }};
        }
        let repo_path = repo.path.clone();
        let head_name = repo.head_name();
        let head_detached = repo.is_head_detached();

        let stashes = repo.get_stashes();
        phase!("stashes");
        // The per-branch picker hides nothing at startup, but the show/hide-
        // remotes toggle is persisted, so honour it here to avoid a first-frame
        // flash of remote-only commits that the next refresh would remove.
        let branches = repo.get_branches()?;
        phase!("branches");
        // Persisted visibility toggles are honoured here to avoid a first-frame
        // flash of commits the next refresh would remove.
        let remote_only = if ui_state.hide_remote_branches {
            remote_only_branch_names(&branches)
        } else {
            std::collections::HashSet::new()
        };
        // Merged-branch classification (ancestry + patch-id squash detection) is
        // O(branches × tree diffs) — over a second of startup on branchy repos
        // (#78). Pay it synchronously ONLY when merged branches are *hidden*:
        // there an async fill-in would flash soon-to-vanish branches on screen.
        // In the default dim-only mode we start unclassified and kick the
        // background classifier below — merged branches fade to dim within a
        // moment instead of blocking the first frame. The GitHub merged-PR
        // signal fills in once its background fetch completes, either way.
        let (merged_branches, squash_targets) = if ui_state.hide_merged_branches {
            crate::git::merged::base_branch(&branches)
                .map(|base| {
                    crate::git::merged::classify_merged_branches_with_targets(
                        repo.repo(),
                        &branches,
                        base.tip_oid,
                        &base.name,
                        &std::collections::HashSet::new(),
                    )
                })
                .unwrap_or_default()
        } else {
            (
                std::collections::HashSet::new(),
                std::collections::HashMap::new(),
            )
        };
        phase!("classify_merged");
        let visible_branches: Vec<BranchInfo> = branches
            .iter()
            .filter(|b| !remote_only.contains(&b.name))
            .filter(|b| !(ui_state.hide_merged_branches && merged_branches.contains(&b.name)))
            .cloned()
            .collect();
        let remotes = repo.remotes();
        let tags = repo.get_tags();
        phase!("tags");
        let commits = repo.get_commits(
            INITIAL_COMMIT_LIMIT,
            &visible_branches,
            &stashes,
            ui_state.hide_merged_branches,
        )?;
        phase!("commits");
        // If the first walk yielded fewer than the limit, the whole history fits.
        let all_commits_loaded = commits.len() < INITIAL_COMMIT_LIMIT;
        let (working_tree_status, initial_message) = Self::working_tree_status_snapshot(&repo);
        phase!("working_tree_status");
        let initial_message_time = initial_message.as_ref().map(|_| now);
        let op_state = repo.operation_state();
        let conflict_count = repo.conflicted_count();
        let uncommitted_count = working_tree_status
            .as_ref()
            .map(|s| s.accurate_file_count());
        let head_commit_oid = repo.head_oid();
        // Squash-link edges (issue #81), only when the option is on. In hide mode
        // the merged tips are filtered out of `visible_branches`, so build_graph's
        // both-endpoints-loaded guard makes the links inert there; in the default
        // dim mode `squash_targets` fills in with the async classifier and a later
        // rebuild draws them.
        let squash_links: Vec<(git2::Oid, git2::Oid)> = if config.ui.squash_link_lines {
            squash_targets
                .iter()
                .filter_map(|(name, &target)| {
                    let tip = branches.iter().find(|b| &b.name == name)?.tip_oid;
                    Some((tip, target))
                })
                .collect()
        } else {
            Vec::new()
        };
        let graph_layout = build_graph(
            &commits,
            &visible_branches,
            &tags,
            &stashes,
            uncommitted_count,
            head_commit_oid,
            &squash_links,
        );

        phase!("build_graph");
        let mut graph_nav = GraphNav::new();
        graph_nav.rebuild_branch_positions(&graph_layout, &repo.remotes());
        let has_uncommitted_node = graph_layout
            .nodes
            .first()
            .is_some_and(|node| node.is_uncommitted);
        if !has_uncommitted_node && !graph_nav.branch_positions.is_empty() {
            graph_nav.selected_branch_position = Some(0);
        }

        // The last phase!() leaves a restart nobody reads; acknowledge it.
        let _ = phase_started;
        perf.record("startup.app_new_total", now.elapsed());
        let mut app = Self {
            mode: AppMode::Normal,
            repo,
            repo_path,
            head_name,
            head_detached,
            commits,
            commit_load_limit: INITIAL_COMMIT_LIMIT,
            all_commits_loaded,
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
            files_pane: {
                let mut files_pane = FilesPaneState::new();
                files_pane.files_group_by_folder = ui_state.files_group_by_folder;
                files_pane
            },
            hidden_branches: std::collections::HashSet::new(),
            pending_remote_deletions: std::collections::HashSet::new(),
            branch_authors: std::collections::HashMap::new(),
            branch_authors_key: Vec::new(),
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
            diff_word_wrap: ui_state.diff_word_wrap,
            diff_source: None,
            message: initial_message,
            message_time: initial_message_time,
            message_sticky: false,
            refresh_latches: RefreshLatches::default(),
            toasts: crate::toast::ToastQueue::new(),
            pr_toasts_armed: false,
            network: NetworkManager::new(),
            credentials: std::collections::HashMap::new(),
            in_flight_op: None,
            pending_auth: None,
            open_prs: std::collections::HashMap::new(),
            pr_fetch: crate::pr::open_pr_fetch(),
            last_pull: None,
            pre_pull_head: None,
            undo_ledger: crate::undo::UndoLedger::default(),
            check_fetch: crate::checks::CheckFetch::new(),
            ci_checks: None,
            thread_fetch: crate::pr_thread::PrThreadFetch::new(),
            pr_thread: None,
            pr_editor: crate::text_editor::TextEditor::new(),
            pr_action_runner: crate::pr_action::PrActionRunner::new(),
            issue_fetch: crate::issue::IssueFetch::new(),
            issue_action_runner: crate::issue_action::IssueActionRunner::new(),
            issue_list: None,
            issue_detail: None,
            issue_editor: crate::text_editor::TextEditor::new(),
            issue_label_picker: None,
            issue_label_filter: None,
            pending_external_edit: None,
            avatar_fetch: crate::avatar_fetch::AvatarFetch::new(),
            avatar_enqueued_generation: None,
            watcher,
            pending_watcher,
            watcher_disconnected: false,
            repo_dirty: false,
            last_undoable_op: None,
            side_panel_layout: ui_state.side_panel_layout,
            hide_remote_branches: ui_state.hide_remote_branches,
            merged: MergedState {
                branches: merged_branches,
                squash_targets,
                classify: crate::merged_branch_fetch::MergedClassifier::new(),
                hide: ui_state.hide_merged_branches,
                dim: ui_state.dim_merged_branches,
                pr_branches: std::collections::HashSet::new(),
                pr_branch_fetch: crate::merged_branch_fetch::merged_branch_fetch(),
                base_update: crate::signature_guarded::SignatureGuarded::default(),
            },
            metadata_columns: ui_state.metadata_columns,
            graph_width_cap: ui_state.graph_width_cap,
            debug_keys: false,
            perf,
            mouse_layout: MouseLayout::default(),
            last_click: None,
            files_view_offset: 0,
            menu_anchor: None,
            popup_rect: None,
            graph_chip_hits: Vec::new(),
            status_hints: Vec::new(),
            graph_split_ratio: crate::mouse::clamp_split_ratio(ui_state.graph_split_ratio as i32),
            dragging_divider: false,
            trace_enabled: ui_state.trace_enabled,
            config,
            terminal_bg,
            pixel_graph: None,
            pixel_specs_cache: None,
            trace_cache: None,
        };
        // Dim-only mode starts unclassified (see `merged_branches` above):
        // hand the branch set to the background classifier right away so
        // merged branches dim within a moment of the first frame.
        if !app.merged.hide {
            app.kick_merged_classification();
        }
        Ok(app)
    }

    /// Build an `App` on a throwaway, empty temp git repository, for tests.
    ///
    /// Starts from [`App::from_repo`] so every field takes its real default and
    /// new fields fall through automatically — no struct-literal churn. Tests
    /// then override only what they exercise (`graph_layout`, `commits`,
    /// `working_tree_status`, `diff_cache`, …).
    ///
    /// The backing temp directory is removed as soon as this returns; the open
    /// in-memory repository handle survives, which is all the diff-cache and
    /// files-pane unit tests need. Callers that must touch the working tree on
    /// disk should build their own repo and use [`App::from_repo`] instead.
    ///
    /// Gated behind the `test-support` feature (enabled for our own test
    /// targets) so it is never compiled into release builds.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn test_fixture() -> Self {
        let tempdir = tempfile::tempdir().expect("create temp repo dir");
        git2::Repository::init(tempdir.path()).expect("init temp repo");
        let repo = GitRepository::open(tempdir.path()).expect("open temp repo");
        Self::from_repo(repo).expect("build fixture App")
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
