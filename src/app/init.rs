//! Construction: load repo data and assemble the `App`.

use super::*;

/// The seed for the startup merged-branch classification (#104): the merged set
/// and squash targets to paint the first frame, plus `Some((signature,
/// gh_merged))` on an exact cache hit (the caller primes rather than kicks the
/// classifier) or `None` when the seed is stale/absent (kick to reconcile).
type MergedSeed = (
    std::collections::HashSet<String>,
    std::collections::HashMap<String, git2::Oid>,
    Option<(u64, std::collections::HashSet<String>)>,
);

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

    /// Like [`App::from_repo`] but with an explicit [`UiState`], so tests can
    /// build an App with e.g. `hide_merged_branches` on and exercise the
    /// startup merged-classification cache path (#104). Gated to test builds.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn from_repo_with_ui_state(repo: GitRepository, ui_state: UiState) -> Result<Self> {
        let repo_path = repo.path.clone();
        let fs_watcher = crate::watcher::FsWatcher::new(std::path::Path::new(&repo_path));
        Self::build(repo, Config::default(), ui_state, fs_watcher, None, None)
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
        // (#78), and worse since remote branches became eligible (#100) and a
        // second trunk tip was added (#103). It must NEVER be paid synchronously
        // before the first frame. In dim-only mode we start unclassified and kick
        // the background classifier below. In hide mode we can't start empty
        // without flashing soon-to-vanish branches, so we seed from the
        // persistent cache (#104): an exact-signature hit is correct and used
        // synchronously (instant, no flash); a stale/absent entry paints its
        // last-known (or empty) result immediately and reconciles in the
        // background. Either way the GitHub merged-PR signal fills in once its
        // fetch completes. `merged_cache_hit` is `Some` only on an exact hit —
        // the signal to prime rather than kick the classifier (see below).
        let (merged_branches, squash_targets, merged_cache_hit) =
            if ui_state.hide_merged_branches {
                Self::init_merged_from_cache(&repo_path, &branches)
            } else {
                (
                    std::collections::HashSet::new(),
                    std::collections::HashMap::new(),
                    None,
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
        // Reconcile the seeded merged classification against the live repo in the
        // background — never inline (#104). Dim-only mode always starts empty and
        // kicks. Hide mode primed from an exact cache hit is already correct, so
        // it only records the signature (so a later unchanged refresh skips the
        // redundant background run); a stale/absent seed kicks to reconcile.
        if app.merged.hide {
            match merged_cache_hit {
                Some((signature, gh_merged)) => {
                    app.merged.classify.note_cached(signature, gh_merged);
                }
                None => app.kick_merged_classification(),
            }
        } else {
            app.kick_merged_classification();
        }
        Ok(app)
    }

    /// Seed the startup merged-branch classification from the persistent cache
    /// (#104), never blocking on a fresh classification.
    ///
    /// Returns the merged set + squash targets to paint the first frame, plus
    /// `Some((signature, gh_merged))` when the cached result *exactly* matches the
    /// live inputs — the caller then treats the seed as authoritative and primes
    /// the background classifier instead of kicking it. `None` means the seed is
    /// stale (or there was no cache) and the caller must kick the async
    /// classifier to reconcile; the brief flash of a few soon-to-hide branches is
    /// the accepted tradeoff over a startup that freezes on the O(branches ×
    /// git-scan) classification.
    fn init_merged_from_cache(repo_path: &str, branches: &[BranchInfo]) -> MergedSeed {
        use std::collections::{HashMap, HashSet};
        let Some(cache) = crate::merged_cache::MergedCache::load(repo_path) else {
            // Cold cache: empty first frame; the caller kicks the classifier.
            return (HashSet::new(), HashMap::new(), None);
        };
        let Some(base) = crate::git::merged::base_branch(branches) else {
            return (HashSet::new(), HashMap::new(), None);
        };
        // Reconstruct the input identity from the branches/base we have now and
        // the gh set the cache recorded (the live gh set isn't observable until
        // the first background fetch). Equal signatures ⇒ nothing the
        // classification depends on has changed, so the cached result is still
        // correct and safe to trust synchronously.
        let input = crate::merged_branch_fetch::ClassifyInput {
            repo_path: repo_path.to_string(),
            branches: branches.to_vec(),
            base_name: base.name.clone(),
            base_tip: base.tip_oid,
            gh_merged: cache.gh_merged.clone(),
        };
        if input.signature() == cache.signature {
            (
                cache.merged,
                cache.squash_targets,
                Some((cache.signature, cache.gh_merged)),
            )
        } else {
            // Stale entry: serve the last-known result for an instant,
            // non-blocking first frame; the caller reconciles asynchronously.
            (cache.merged, cache.squash_targets, None)
        }
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
