//! Application state management

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::widgets::ListState;

use git2::Oid;

use crate::{
    action::Action,
    config::Config,
    git::{
        build_graph,
        graph::GraphLayout,
        operations::{
            add_tag, cherry_pick, checkout_branch, checkout_commit, checkout_remote_branch,
            commit_with_message, create_branch, delete_branch, fetch_origin, merge_branch,
            push_to_origin, rebase_branch, reset_to_commit, revert_commit, stage_file,
            unstage_file, ResetMode,
        },
        BranchInfo, CommitDiffInfo, CommitInfo, FileDiffContent, FileDiffInfo, GitRepository,
        StageStatus, WorkingTreeStatus,
    },
    search::{fuzzy_search_branches, FuzzySearchResult},
};

/// Copy text to system clipboard using platform-specific commands
fn copy_to_clipboard(text: &str) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let commands: &[(&str, &[&str])] = &[
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
        ("wl-copy", &[]),
        ("pbcopy", &[]),
    ];

    for (cmd, args) in commands {
        if let Ok(mut child) = Command::new(cmd)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().is_ok() {
                return Ok(());
            }
        }
    }

    anyhow::bail!("No clipboard tool found (install xclip, xsel, or wl-copy)")
}

/// Filter branch names to exclude remote branches that have matching local branches
/// Returns branches in order: local branches first, then remote-only branches
fn filter_remote_duplicates(branch_names: &[String]) -> Vec<&str> {
    use std::collections::HashSet;

    let local_branches: HashSet<&str> = branch_names
        .iter()
        .filter(|n| !n.starts_with("origin/"))
        .map(|s| s.as_str())
        .collect();

    branch_names
        .iter()
        .filter(|name| {
            if let Some(local_name) = name.strip_prefix("origin/") {
                !local_branches.contains(local_name)
            } else {
                true
            }
        })
        .map(|s| s.as_str())
        .collect()
}

/// Which panel is focused
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusedPanel {
    Graph,
    Files,
    CommitDetail,
}

/// Items in the commit context menu
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitMenuItem {
    Push,
    Checkout,
    CreateBranch,
    MergeIntoCurrent,
    CherryPick,
    Rebase,
    Reset,
    ResetSoft,
    ResetMixed,
    ResetHard,
    AddTag,
    Revert,
    CopyHash,
}

impl CommitMenuItem {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Push => "Push to origin",
            Self::Checkout => "Checkout",
            Self::CreateBranch => "Create branch here",
            Self::MergeIntoCurrent => "Merge into current branch",
            Self::CherryPick => "Cherry-pick",
            Self::Rebase => "Rebase current branch onto this",
            Self::Reset => "Reset to this commit...",
            Self::ResetSoft => "Soft (keep changes staged)",
            Self::ResetMixed => "Mixed (keep changes unstaged)",
            Self::ResetHard => "Hard (discard all changes)",
            Self::AddTag => "Add tag",
            Self::Revert => "Revert this commit",
            Self::CopyHash => "Copy commit hash",
        }
    }
}

/// Item in the files pane (header or file entry)
#[derive(Debug, Clone)]
pub enum FilesPaneItem {
    Header(String),
    File(FileDiffInfo),
}

/// Application modes
#[derive(Debug, Clone)]
pub enum AppMode {
    Normal,
    Help,
    Input {
        title: String,
        input: String,
        action: InputAction,
    },
    Confirm {
        message: String,
        action: ConfirmAction,
    },
    Error {
        message: String,
    },
    CommitMenu {
        items: Vec<CommitMenuItem>,
        selected: usize,
    },
    BranchFilter {
        filter: String,
        selected: usize,
        all_branches: Vec<String>,
    },
    FileSelect {
        selected_index: usize,
        file_list: Vec<FileDiffInfo>,
    },
    FileDiff {
        file_index: usize,
        file_list: Vec<FileDiffInfo>,
        content: FileDiffContent,
        rendered_lines: Vec<ratatui::text::Line<'static>>,
        hunk_positions: Vec<usize>,
        scroll_offset: usize,
        horizontal_offset: usize,
        max_line_width: usize,
        total_lines: usize,
    },
}

/// Input action kinds
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    CreateBranch,
    AddTag,
    Search,
}

/// Confirmation action kinds
#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteBranch(String),
    Merge(String),
    Rebase(String),
    CherryPick(Oid),
    Revert(Oid),
    ResetSoft(Oid),
    ResetMixed(Oid),
    ResetHard(Oid),
    Push,
}

/// Result of async diff computation
struct DiffResult {
    oid: Oid,
    diff: Result<CommitDiffInfo, String>,
}

/// Identifies the currently selected node for diff loading and caching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffTarget {
    Commit(Oid),
    Uncommitted,
}

type UncommittedDiffResult = (Result<CommitDiffInfo, String>, Option<WorkingTreeStatus>);

/// Delay before starting a diff load after selection changes.
/// Prevents unnecessary computation during fast scrolling.
const DIFF_LOAD_DEBOUNCE: Duration = Duration::from_millis(120);

/// Search state for branch search feature
#[derive(Debug, Clone, Default)]
struct SearchState {
    /// Fuzzy search results (sorted by score)
    fuzzy_matches: Vec<FuzzySearchResult>,
    /// Selected index in the dropdown (None if no results)
    dropdown_selection: Option<usize>,
    /// Position before search started (for cancel restoration)
    original_position: Option<usize>,
    /// Original node selection before search started
    original_node: Option<usize>,
}

impl SearchState {
    /// Move selection up in the dropdown (with wrap-around)
    fn select_up(&mut self) {
        if self.fuzzy_matches.is_empty() {
            return;
        }
        self.dropdown_selection = Some(match self.dropdown_selection {
            Some(0) | None => self.fuzzy_matches.len() - 1,
            Some(idx) => idx - 1,
        });
    }

    /// Move selection down in the dropdown (with wrap-around)
    fn select_down(&mut self) {
        if self.fuzzy_matches.is_empty() {
            return;
        }
        let last_idx = self.fuzzy_matches.len() - 1;
        self.dropdown_selection = Some(match self.dropdown_selection {
            Some(idx) if idx < last_idx => idx + 1,
            _ => 0,
        });
    }

    /// Get the currently selected result
    fn selected_result(&self) -> Option<&FuzzySearchResult> {
        self.dropdown_selection
            .and_then(|idx| self.fuzzy_matches.get(idx))
    }

    /// Clamp dropdown selection to valid range after results update
    fn clamp_selection(&mut self) {
        if self.fuzzy_matches.is_empty() {
            self.dropdown_selection = None;
        } else if let Some(idx) = self.dropdown_selection {
            if idx >= self.fuzzy_matches.len() {
                self.dropdown_selection = Some(self.fuzzy_matches.len() - 1);
            }
        } else {
            // Auto-select first result if we have results
            self.dropdown_selection = Some(0);
        }
    }
}

/// Application state
pub struct App {
    pub mode: AppMode,
    pub repo: GitRepository,
    pub repo_path: String,
    pub head_name: Option<String>,

    // Data
    pub commits: Vec<CommitInfo>,
    pub branches: Vec<BranchInfo>,
    pub graph_layout: GraphLayout,

    // UI state
    pub graph_list_state: ListState,
    pub focused_panel: FocusedPanel,
    pub files_group_by_folder: bool,
    pub files_filter: String,
    pub hidden_branches: std::collections::HashSet<String>,
    pub commit_editor: crate::text_editor::TextEditor,
    pub editing_commit_message: bool,

    // Branch selection state
    /// List of (node_index, branch_name) for all branches
    pub branch_positions: Vec<(usize, String)>,
    /// Currently selected branch position index
    pub selected_branch_position: Option<usize>,

    // Search state
    search_state: SearchState,

    // Latest working tree status snapshot
    working_tree_status: Option<WorkingTreeStatus>,

    // Quick diff cache (synchronous, file names only, no line stats)
    quick_diff_cache: Option<CommitDiffInfo>,
    quick_diff_target: Option<DiffTarget>,

    // Diff cache (async load)
    diff_cache: Option<CommitDiffInfo>,
    diff_cache_oid: Option<Oid>,
    diff_loading_oid: Option<Oid>,
    diff_receiver: Option<Receiver<DiffResult>>,

    // Uncommitted diff cache
    uncommitted_diff_cache: Option<CommitDiffInfo>,
    uncommitted_diff_failed: bool,
    uncommitted_diff_loading: bool,
    uncommitted_diff_receiver: Option<Receiver<UncommittedDiffResult>>,
    /// Cache key: working tree status at the time of caching (for invalidation)
    uncommitted_cache_key: Option<WorkingTreeStatus>,
    selected_diff_target: Option<DiffTarget>,
    selected_diff_target_changed_at: Instant,

    // Flags
    pub should_quit: bool,
    pending_refresh: bool,
    /// Viewport height for diff scroll calculations (updated during render)
    pub diff_viewport_height: u16,
    /// Viewport width for diff horizontal scroll calculations (updated during render)
    pub diff_viewport_width: u16,

    // Status message with auto-clear
    message: Option<String>,
    message_time: Option<std::time::Instant>,

    // Async fetch
    fetch_receiver: Option<Receiver<Result<(), String>>>,
    /// Whether to suppress error dialogs for fetch failures (for auto-fetch)
    fetch_silent: bool,

    // Auto-refresh state
    config: Config,
    last_refresh_time: Instant,
    last_fetch_time: Instant,
}

impl App {
    fn working_tree_status_snapshot(
        repo: &GitRepository,
    ) -> (Option<WorkingTreeStatus>, Option<String>) {
        match repo.get_working_tree_status() {
            Ok(status) => (status, None),
            Err(e) => (None, Some(format!("Working tree status failed: {e}"))),
        }
    }

    /// Create a new application
    pub fn new() -> Result<Self> {
        let config = Config::load();
        let now = Instant::now();

        let repo = GitRepository::discover()?;
        let repo_path = repo.path.clone();
        let head_name = repo.head_name();

        let commits = repo.get_commits(500)?;
        let branches = repo.get_branches()?;
        let (working_tree_status, initial_message) = Self::working_tree_status_snapshot(&repo);
        let initial_message_time = initial_message.as_ref().map(|_| now);
        let uncommitted_count = working_tree_status
            .as_ref()
            .map(|s| s.accurate_file_count());
        let head_commit_oid = repo.head_oid();
        let graph_layout = build_graph(&commits, &branches, uncommitted_count, head_commit_oid);

        let mut graph_list_state = ListState::default();
        graph_list_state.select(Some(0));

        // Build branch positions
        let branch_positions = Self::build_branch_positions(&graph_layout);

        // Determine initial branch selection
        // If uncommitted node exists (at index 0), don't select any branch
        // Otherwise, select the first branch if exists
        let has_uncommitted_node = graph_layout
            .nodes
            .first()
            .is_some_and(|node| node.is_uncommitted);
        let selected_branch_position = if has_uncommitted_node || branch_positions.is_empty() {
            None
        } else {
            Some(0)
        };

        Ok(Self {
            mode: AppMode::Normal,
            repo,
            repo_path,
            head_name,
            commits,
            branches,
            graph_layout,
            graph_list_state,
            focused_panel: FocusedPanel::Graph,
            files_group_by_folder: false,
            files_filter: String::new(),
            hidden_branches: std::collections::HashSet::new(),
            commit_editor: crate::text_editor::TextEditor::new(),
            editing_commit_message: false,
            branch_positions,
            selected_branch_position,
            search_state: SearchState::default(),
            working_tree_status,
            quick_diff_cache: None,
            quick_diff_target: None,
            diff_cache: None,
            diff_cache_oid: None,
            diff_loading_oid: None,
            diff_receiver: None,
            uncommitted_diff_cache: None,
            uncommitted_diff_failed: false,
            uncommitted_diff_loading: false,
            uncommitted_diff_receiver: None,
            uncommitted_cache_key: None,
            selected_diff_target: None,
            selected_diff_target_changed_at: now,
            should_quit: false,
            pending_refresh: false,
            diff_viewport_height: 40,
            diff_viewport_width: 80,
            message: initial_message,
            message_time: initial_message_time,
            fetch_receiver: None,
            fetch_silent: false,
            config,
            last_refresh_time: now,
            last_fetch_time: now,
        })
    }

    /// Clear all diff caches
    fn clear_all_diff_caches(&mut self) {
        self.quick_diff_cache = None;
        self.quick_diff_target = None;
        self.diff_cache = None;
        self.diff_cache_oid = None;
        self.diff_loading_oid = None;
        self.diff_receiver = None;
        self.clear_uncommitted_diff_cache();
    }

    /// Clear uncommitted diff cache only
    fn clear_uncommitted_diff_cache(&mut self) {
        self.uncommitted_diff_cache = None;
        self.uncommitted_diff_failed = false;
        self.uncommitted_diff_loading = false;
        self.uncommitted_diff_receiver = None;
        self.uncommitted_cache_key = None;
    }

    /// Invalidate the uncommitted diff cache key to trigger a background reload,
    /// while keeping the cached data visible to avoid UI flicker.
    ///
    /// When a background computation is already in flight, keep the receiver
    /// alive so the result can still be received.  Only the cache key is
    /// cleared — once the thread completes the key will be set from the
    /// thread's own status snapshot (see `update_diff_cache`).
    fn invalidate_uncommitted_diff_cache(&mut self) {
        self.uncommitted_diff_failed = false;
        if !self.uncommitted_diff_loading {
            self.uncommitted_diff_receiver = None;
        }
        self.uncommitted_cache_key = None;
    }

    fn can_reuse_uncommitted_cache(
        &self,
        was_uncommitted_selected: bool,
        has_uncommitted_node: bool,
    ) -> bool {
        let Some(cache_key) = self.uncommitted_cache_key.as_ref() else {
            return false;
        };
        let Some(current_status) = self.working_tree_status.as_ref() else {
            return false;
        };

        was_uncommitted_selected
            && has_uncommitted_node
            && cache_key.is_precise_cache_key()
            && current_status.is_precise_cache_key()
            && cache_key == current_status
    }

    fn current_diff_target(&self) -> Option<DiffTarget> {
        let node = self
            .graph_list_state
            .selected()
            .and_then(|idx| self.graph_layout.nodes.get(idx))?;

        if node.is_uncommitted {
            Some(DiffTarget::Uncommitted)
        } else {
            node.commit
                .as_ref()
                .map(|commit| DiffTarget::Commit(commit.oid))
        }
    }

    fn sync_selected_diff_target(&mut self) -> Option<DiffTarget> {
        let target = self.current_diff_target();
        if self.selected_diff_target != target {
            self.selected_diff_target = target;
            self.selected_diff_target_changed_at = Instant::now();

            // Compute quick file list synchronously for instant display
            if let Some(t) = target {
                if self.quick_diff_target != Some(t) {
                    self.quick_diff_target = Some(t);
                    self.quick_diff_cache = match t {
                        DiffTarget::Commit(oid) => {
                            CommitDiffInfo::quick_file_list_for_commit(&self.repo.repo, oid).ok()
                        }
                        DiffTarget::Uncommitted => {
                            CommitDiffInfo::quick_file_list_for_working_tree(&self.repo.repo).ok()
                        }
                    };
                }
            }
        }
        target
    }

    fn has_cached_diff_for_target(&self, target: DiffTarget) -> bool {
        match target {
            DiffTarget::Commit(oid) => self.diff_cache_oid == Some(oid),
            DiffTarget::Uncommitted => {
                // A present cache key means the diff was computed and has not
                // been invalidated by refresh().  Staleness detection is handled
                // by can_reuse_uncommitted_cache() inside refresh(), which
                // clears the key when the working tree changes.
                let has_key = self.uncommitted_cache_key.is_some();
                has_key && (self.uncommitted_diff_cache.is_some() || self.uncommitted_diff_failed)
            }
        }
    }

    fn is_diff_loading_for_target(&self, target: DiffTarget) -> bool {
        match target {
            DiffTarget::Commit(oid) => self.diff_loading_oid == Some(oid),
            DiffTarget::Uncommitted => self.uncommitted_diff_loading,
        }
    }

    fn is_diff_debouncing_for_target(&self, target: DiffTarget) -> bool {
        self.selected_diff_target == Some(target)
            && self.selected_diff_target_changed_at.elapsed() < DIFF_LOAD_DEBOUNCE
    }

    fn has_in_flight_diff(&self) -> bool {
        self.diff_loading_oid.is_some() || self.uncommitted_diff_loading
    }

    /// Refresh repository data
    /// If `force` is true, always clears diff cache (for manual refresh)
    /// If `force` is false, keeps cache when the same content is selected (for auto-refresh)
    pub fn refresh(&mut self, force: bool) -> Result<()> {
        // Save the current selection state for restoration
        let was_uncommitted_selected = self
            .graph_list_state
            .selected()
            .and_then(|idx| self.graph_layout.nodes.get(idx))
            .is_some_and(|node| node.is_uncommitted);
        let prev_selected_commit_oid = self
            .graph_list_state
            .selected()
            .and_then(|idx| self.graph_layout.nodes.get(idx))
            .and_then(|node| node.commit.as_ref())
            .map(|commit| commit.oid);

        let prev_branch_name = self
            .selected_branch_position
            .and_then(|pos| self.branch_positions.get(pos))
            .map(|(_, name)| name.clone());

        // Get working tree status once and reuse
        let (working_tree_status, status_message) = Self::working_tree_status_snapshot(&self.repo);
        if let Some(message) = status_message {
            self.set_message(message);
        }
        let uncommitted_count = working_tree_status
            .as_ref()
            .map(|s| s.accurate_file_count());
        self.working_tree_status = working_tree_status;

        self.commits = self.repo.get_commits(500)?;
        self.branches = self.repo.get_branches()?;
        let visible_branches: Vec<BranchInfo> = self
            .branches
            .iter()
            .filter(|b| !self.hidden_branches.contains(&b.name))
            .cloned()
            .collect();
        let head_commit_oid = self.repo.head_oid();
        self.graph_layout = build_graph(
            &self.commits,
            &visible_branches,
            uncommitted_count,
            head_commit_oid,
        );
        self.head_name = self.repo.head_name();

        // Rebuild branch positions
        self.branch_positions = Self::build_branch_positions(&self.graph_layout);

        // Restore selection state
        // Check if uncommitted node still exists in the new graph
        let has_uncommitted_node = self
            .graph_layout
            .nodes
            .first()
            .is_some_and(|node| node.is_uncommitted);

        if was_uncommitted_selected && has_uncommitted_node {
            // Restore uncommitted node selection
            self.graph_list_state.select(Some(0));
            self.selected_branch_position = None;
        } else {
            // Restore branch selection if the branch still exists
            self.selected_branch_position = prev_branch_name
                .and_then(|name| self.branch_positions.iter().position(|(_, n)| n == &name));

            // Sync node selection with branch selection
            if let Some(pos) = self.selected_branch_position {
                if let Some((node_idx, _)) = self.branch_positions.get(pos) {
                    self.graph_list_state.select(Some(*node_idx));
                }
            } else if let Some(oid) = prev_selected_commit_oid {
                let node_idx =
                    self.graph_layout.nodes.iter().position(|node| {
                        node.commit.as_ref().is_some_and(|commit| commit.oid == oid)
                    });
                if let Some(idx) = node_idx {
                    self.graph_list_state.select(Some(idx));
                } else if let Some(prev) = self.graph_list_state.selected() {
                    // OID pushed out of range — keep cursor at the nearest
                    // valid row instead of clearing the selection.
                    let max = self.graph_layout.nodes.len().saturating_sub(1);
                    self.graph_list_state.select(Some(prev.min(max)));
                }
            }
        }

        // Handle diff cache based on force flag
        if force {
            self.clear_all_diff_caches();
        } else {
            // Auto-refresh: smart cache - only clear if selection changed
            let selected_oid = self
                .graph_list_state
                .selected()
                .and_then(|idx| self.graph_layout.nodes.get(idx))
                .and_then(|n| n.commit.as_ref())
                .map(|c| c.oid);

            // Keep commit diff cache if the same commit is still selected
            if self.diff_cache_oid != selected_oid {
                self.diff_cache = None;
                self.diff_cache_oid = None;
                self.diff_loading_oid = None;
                self.diff_receiver = None;
            }

            // Keep uncommitted diff cache only if:
            // 1. Uncommitted node is still selected (was_uncommitted_selected && has_uncommitted_node)
            // 2. The working tree status hasn't changed (same files and mtimes)
            if !self.can_reuse_uncommitted_cache(was_uncommitted_selected, has_uncommitted_node) {
                // Invalidate cache key to trigger a background reload, but keep
                // the cached data so the UI can keep showing it (no flicker).
                self.invalidate_uncommitted_diff_cache();
            }
        }

        // Clear search state on refresh to avoid stale indices
        // Skip if in search mode to prevent clearing active search results
        if !self.is_in_search_mode() {
            self.search_state = SearchState::default();
        }

        // Clamp the selection
        let max_commit = self.graph_layout.nodes.len().saturating_sub(1);
        if let Some(selected) = self.graph_list_state.selected() {
            if selected > max_commit {
                self.graph_list_state.select(Some(max_commit));
            }
        }

        Ok(())
    }

    /// Update fuzzy search results for the given query
    fn update_fuzzy_search(&mut self, query: &str) {
        self.search_state.fuzzy_matches = fuzzy_search_branches(query, &self.branch_positions);
        self.search_state.clamp_selection();
    }

    /// Jump to the currently selected search result
    fn jump_to_search_result(&mut self) {
        let Some(result) = self.search_state.selected_result() else {
            return;
        };
        let branch_idx = result.branch_idx;
        let Some((node_idx, _)) = self.branch_positions.get(branch_idx) else {
            return;
        };

        self.selected_branch_position = Some(branch_idx);
        self.graph_list_state.select(Some(*node_idx));
    }

    /// Save current position before starting search
    fn save_search_position(&mut self) {
        self.search_state.original_position = self.selected_branch_position;
        self.search_state.original_node = self.graph_list_state.selected();
    }

    /// Restore position saved before search (for cancel)
    fn restore_search_position(&mut self) {
        self.selected_branch_position = self.search_state.original_position;
        if let Some(node) = self.search_state.original_node {
            self.graph_list_state.select(Some(node));
        }
    }

    /// Get current search results for UI rendering
    pub fn search_results(&self) -> &[FuzzySearchResult] {
        &self.search_state.fuzzy_matches
    }

    /// Get current dropdown selection index
    pub fn search_selection(&self) -> Option<usize> {
        self.search_state.dropdown_selection
    }

    /// Check if currently in search input mode
    pub fn is_in_search_mode(&self) -> bool {
        matches!(
            &self.mode,
            AppMode::Input {
                action: InputAction::Search,
                ..
            }
        )
    }

    /// Jump to the currently checked out branch (HEAD)
    fn jump_to_head(&mut self) {
        // Find the HEAD branch name
        let Some(head_name) = &self.head_name else {
            return;
        };

        // Find the branch position index that matches HEAD
        let Some((branch_pos_idx, (node_idx, _))) = self
            .branch_positions
            .iter()
            .enumerate()
            .find(|(_, (_, name))| name == head_name)
        else {
            return;
        };

        self.selected_branch_position = Some(branch_pos_idx);
        self.graph_list_state.select(Some(*node_idx));
    }

    /// Check if async fetch has completed and process the result
    pub fn update_fetch_status(&mut self) {
        let Some(rx) = &self.fetch_receiver else {
            return;
        };
        let Ok(fetch_result) = rx.try_recv() else {
            return;
        };

        let silent = self.fetch_silent;
        self.fetch_receiver = None;
        self.fetch_silent = false;

        match fetch_result {
            Ok(()) => {
                self.reset_timers();
                if matches!(
                    self.mode,
                    AppMode::FileSelect { .. } | AppMode::FileDiff { .. }
                ) {
                    self.pending_refresh = true;
                } else {
                    // Snapshot state before refresh to detect changes
                    let prev_head = self.repo.head_oid();
                    let prev_branch_count = self.branches.len();
                    match self.refresh(true) {
                        Ok(()) => {
                            let new_head = self.repo.head_oid();
                            let new_branch_count = self.branches.len();
                            if prev_head != new_head
                                || prev_branch_count != new_branch_count
                            {
                                self.set_message("Fetched from origin");
                            }
                        }
                        Err(e) => self.show_error(format!("Refresh failed: {e}")),
                    }
                }
            }
            Err(e) if !silent => self.show_error(e),
            Err(_) => {} // Silent mode: suppress error dialog for auto-fetch
        }
    }

    /// Check if fetch is currently in progress
    pub fn is_fetching(&self) -> bool {
        self.fetch_receiver.is_some()
    }

    /// Check and perform auto-refresh if interval has elapsed
    pub fn check_auto_refresh(&mut self) {
        if self.is_fetching() {
            return;
        }
        if matches!(
            self.mode,
            AppMode::FileSelect { .. } | AppMode::FileDiff { .. }
        ) {
            return;
        }

        let now = Instant::now();
        let refresh_config = &self.config.refresh;

        // Auto-fetch (check first as it includes refresh)
        if refresh_config.auto_fetch
            && now.duration_since(self.last_fetch_time).as_secs() >= refresh_config.fetch_interval
        {
            self.start_fetch(false, true); // silent=true for auto-fetch
            return;
        }

        // Auto-refresh
        if refresh_config.auto_refresh
            && now.duration_since(self.last_refresh_time).as_secs()
                >= refresh_config.refresh_interval
        {
            if let Err(e) = self.refresh(false) {
                self.set_message(format!("Auto-refresh failed: {e}"));
            }
            self.last_refresh_time = now;
        }
    }

    /// Start fetch in background
    /// If `show_message` is true, displays "Fetching from origin..."
    /// If `silent` is true, errors will not show a dialog (for auto-fetch)
    fn start_fetch(&mut self, show_message: bool, silent: bool) {
        let (tx, rx) = mpsc::channel();
        let repo_path = self.repo_path.clone();

        thread::spawn(move || {
            let result = fetch_origin(&repo_path).map_err(|e| e.to_string());
            let _ = tx.send(result);
        });

        self.fetch_receiver = Some(rx);
        self.fetch_silent = silent;
        if show_message {
            self.set_message("Fetching from origin...");
        }
    }

    /// Reset both timers (call after manual refresh/fetch)
    fn reset_timers(&mut self) {
        let now = Instant::now();
        self.last_refresh_time = now;
        self.last_fetch_time = now;
    }

    /// Set a status message (will auto-clear after a few seconds)
    pub fn set_message(&mut self, msg: impl Into<String>) {
        self.message = Some(msg.into());
        self.message_time = Some(std::time::Instant::now());
    }

    /// Get current message if not expired (5 seconds timeout)
    pub fn get_message(&self) -> Option<&str> {
        const MESSAGE_TIMEOUT_SECS: u64 = 5;

        // Don't timeout while fetching
        if self.is_fetching() {
            return self.message.as_deref();
        }

        let msg = self.message.as_deref()?;
        let time = self.message_time.as_ref()?;

        if time.elapsed().as_secs() < MESSAGE_TIMEOUT_SECS {
            Some(msg)
        } else {
            None
        }
    }

    /// Get search match count
    pub fn search_match_count(&self) -> usize {
        self.search_state.fuzzy_matches.len()
    }

    /// Update diff info for the selected node (commit or uncommitted changes, async)
    pub fn update_diff_cache(&mut self) {
        // Pull in completed results for commit diff
        if let Some(ref receiver) = self.diff_receiver {
            match receiver.try_recv() {
                Ok(result) => {
                    match result.diff {
                        Ok(diff) => {
                            self.diff_cache = Some(diff);
                            self.diff_cache_oid = Some(result.oid);
                        }
                        Err(e) => {
                            self.diff_cache = None;
                            self.diff_cache_oid = Some(result.oid);
                            self.set_message(format!("Failed to load diff: {e}"));
                        }
                    }
                    self.diff_loading_oid = None;
                    self.diff_receiver = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Thread panicked or dropped sender — clear loading state
                    self.diff_loading_oid = None;
                    self.diff_receiver = None;
                    self.set_message("Diff computation failed unexpectedly");
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        // Pull in completed results for uncommitted diff
        if let Some(ref receiver) = self.uncommitted_diff_receiver {
            match receiver.try_recv() {
                Ok((result, status)) => {
                    match result {
                        Ok(diff) => {
                            self.uncommitted_diff_cache = Some(diff);
                            self.uncommitted_diff_failed = false;
                            self.sync_file_list_with_uncommitted_diff();
                        }
                        Err(e) => {
                            self.uncommitted_diff_cache = None;
                            self.uncommitted_diff_failed = true;
                            self.set_message(format!("Failed to load diff: {e}"));
                        }
                    }
                    // Set the cache key only when the thread's status snapshot
                    // still matches the current working tree status.  If
                    // refresh() has already observed a newer state, leave the
                    // key as None so the next update_diff_cache() tick starts a
                    // fresh computation.  The stale diff data is kept in
                    // uncommitted_diff_cache for display to avoid flicker until
                    // the new result arrives.
                    let effective_status = status.or_else(|| self.working_tree_status.clone());
                    if effective_status.as_ref() == self.working_tree_status.as_ref() {
                        self.uncommitted_cache_key = effective_status;
                    }
                    self.uncommitted_diff_loading = false;
                    self.uncommitted_diff_receiver = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // Thread panicked or dropped sender — clear loading state
                    self.uncommitted_diff_loading = false;
                    self.uncommitted_diff_receiver = None;
                    self.uncommitted_diff_failed = true;
                    self.uncommitted_cache_key = self.working_tree_status.clone();
                    self.set_message("Diff computation failed unexpectedly");
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        let Some(target) = self.sync_selected_diff_target() else {
            return;
        };

        if self.has_cached_diff_for_target(target)
            || self.is_diff_loading_for_target(target)
            || self.is_diff_debouncing_for_target(target)
        {
            return;
        }

        // Keep only one heavy diff computation in flight to avoid CPU contention
        // during fast scrolling. Once it completes, the latest selection will load.
        if self.has_in_flight_diff() {
            return;
        }

        match target {
            DiffTarget::Uncommitted => {
                // Compute uncommitted diff in the background
                let (tx, rx) = mpsc::channel();
                let repo_path = self.repo_path.clone();

                self.uncommitted_diff_failed = false;
                self.uncommitted_diff_loading = true;
                self.uncommitted_diff_receiver = Some(rx);

                thread::spawn(move || {
                    let repo = GitRepository {
                        path: repo_path.clone(),
                        repo: match git2::Repository::open(&repo_path) {
                            Ok(r) => r,
                            Err(e) => {
                                let _ = tx.send((Err(e.to_string()), None));
                                return;
                            }
                        },
                    };
                    // Snapshot status BEFORE computing the diff so the cache
                    // key represents the state the diff was computed against.
                    // If the working tree changes during computation, the key
                    // will no longer match the refresh-time status, correctly
                    // triggering a reload instead of caching a stale diff.
                    let status = repo.get_working_tree_status().unwrap_or_default();
                    let diff =
                        CommitDiffInfo::from_working_tree(&repo.repo).map_err(|e| e.to_string());
                    let _ = tx.send((diff, status));
                });
            }
            DiffTarget::Commit(oid) => {
                // Compute diff in the background
                let (tx, rx) = mpsc::channel();
                let repo_path = self.repo_path.clone();

                self.diff_loading_oid = Some(oid);
                self.diff_receiver = Some(rx);

                thread::spawn(move || {
                    let diff = git2::Repository::open(&repo_path)
                        .map_err(|e| e.to_string())
                        .and_then(|repo| {
                            CommitDiffInfo::from_commit(&repo, oid).map_err(|e| e.to_string())
                        });

                    let _ = tx.send(DiffResult { oid, diff });
                });
            }
        }
    }

    /// Get cached diff info for the currently selected node
    pub fn cached_diff(&self) -> Option<&CommitDiffInfo> {
        match self.current_diff_target()? {
            DiffTarget::Commit(oid) if self.diff_cache_oid == Some(oid) => self.diff_cache.as_ref(),
            DiffTarget::Commit(_) => None,
            DiffTarget::Uncommitted => self.uncommitted_diff_cache.as_ref(),
        }
    }

    /// Get the best available diff: full if cached, otherwise quick file list
    pub fn cached_diff_or_quick(&self) -> Option<&CommitDiffInfo> {
        self.cached_diff().or(self.quick_diff_cache.as_ref())
    }

    /// Whether line stats are still loading (full diff not yet available but quick is)
    pub fn is_line_stats_loading(&self) -> bool {
        self.cached_diff().is_none() && self.quick_diff_cache.is_some()
    }

    /// Whether diff is loading or pending (debouncing) for the selected node
    pub fn is_diff_loading(&self) -> bool {
        let Some(target) = self.current_diff_target() else {
            return false;
        };

        !self.has_cached_diff_for_target(target)
            && (self.is_diff_loading_for_target(target)
                || self.is_diff_debouncing_for_target(target)
                || self.has_in_flight_diff())
    }

    /// Check if the currently selected node is the uncommitted changes node
    pub fn is_uncommitted_selected(&self) -> bool {
        self.graph_list_state
            .selected()
            .and_then(|idx| self.graph_layout.nodes.get(idx))
            .is_some_and(|node| node.is_uncommitted)
    }

    /// Get the file list for the files pane (staged then unstaged for uncommitted,
    /// or flat list for committed)
    pub fn files_pane_items(&self) -> Vec<FilesPaneItem> {
        let diff = self.cached_diff_or_quick();

        let raw_files: Vec<FileDiffInfo> = if self.is_uncommitted_selected() {
            if let Some(diff) = diff {
                if !diff.staged_files.is_empty() || !diff.unstaged_files.is_empty() {
                    // Return staged + unstaged separately (handled below)
                    let mut all = diff.staged_files.clone();
                    all.extend(diff.unstaged_files.clone());
                    all
                } else {
                    diff.files.clone()
                }
            } else {
                return Vec::new();
            }
        } else if let Some(diff) = diff {
            diff.files.clone()
        } else {
            return Vec::new();
        };

        // Apply fuzzy filter
        let filtered: Vec<FileDiffInfo> = if self.files_filter.is_empty() {
            raw_files
        } else {
            let query = self.files_filter.to_lowercase();
            raw_files
                .into_iter()
                .filter(|f| {
                    let path = f.path.to_string_lossy().to_lowercase();
                    // Simple substring matching (fuzzy-ish)
                    query.chars().all(|c| path.contains(c))
                })
                .collect()
        };

        // Build items with optional folder grouping
        if self.files_group_by_folder {
            self.build_folder_grouped_items(&filtered)
        } else if self.is_uncommitted_selected() {
            self.build_staged_unstaged_items(&filtered)
        } else {
            filtered.into_iter().map(FilesPaneItem::File).collect()
        }
    }

    fn build_staged_unstaged_items(&self, files: &[FileDiffInfo]) -> Vec<FilesPaneItem> {
        let staged: Vec<_> = files
            .iter()
            .filter(|f| matches!(f.stage_status, Some(StageStatus::Staged)))
            .cloned()
            .collect();
        let unstaged: Vec<_> = files
            .iter()
            .filter(|f| !matches!(f.stage_status, Some(StageStatus::Staged)))
            .cloned()
            .collect();

        let mut items = Vec::new();
        if !staged.is_empty() {
            items.push(FilesPaneItem::Header("Staged Changes".to_string()));
            items.extend(staged.into_iter().map(FilesPaneItem::File));
        }
        if !unstaged.is_empty() {
            items.push(FilesPaneItem::Header("Unstaged Changes".to_string()));
            items.extend(unstaged.into_iter().map(FilesPaneItem::File));
        }
        items
    }

    fn build_folder_grouped_items(&self, files: &[FileDiffInfo]) -> Vec<FilesPaneItem> {
        use std::collections::BTreeMap;

        // Group files by parent directory
        let mut folders: BTreeMap<String, Vec<FileDiffInfo>> = BTreeMap::new();
        for file in files {
            let folder = file
                .path
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let key = if folder.is_empty() {
                ".".to_string()
            } else {
                folder
            };
            folders.entry(key).or_default().push(file.clone());
        }

        let mut items = Vec::new();
        for (folder, folder_files) in &folders {
            items.push(FilesPaneItem::Header(format!("{}/", folder)));
            for f in folder_files {
                items.push(FilesPaneItem::File(f.clone()));
            }
        }
        items
    }

    /// Handle an action
    pub fn handle_action(&mut self, action: Action) -> Result<()> {
        // Ctrl+Q always quits
        if matches!(action, Action::ForceQuit) {
            self.should_quit = true;
            return Ok(());
        }

        match &self.mode {
            AppMode::Normal => self.handle_normal_action(action)?,
            AppMode::Help => self.handle_help_action(action),
            AppMode::Input { .. } => self.handle_input_action(action)?,
            AppMode::Confirm { .. } => self.handle_confirm_action(action)?,
            AppMode::Error { .. } => self.handle_error_action(action),
            AppMode::CommitMenu { .. } => self.handle_commit_menu_action(action)?,
            AppMode::BranchFilter { .. } => self.handle_branch_filter_action(action)?,
            AppMode::FileSelect { .. } => self.handle_file_select_action(action)?,
            AppMode::FileDiff { .. } => self.handle_file_diff_action(action)?,
        }
        Ok(())
    }

    /// Show an error
    pub fn show_error(&mut self, message: String) {
        self.mode = AppMode::Error { message };
    }

    fn handle_normal_action(&mut self, action: Action) -> Result<()> {
        // Panel navigation works from any panel
        match action {
            Action::PanelLeft => {
                self.editing_commit_message = false;
                self.focused_panel = match self.focused_panel {
                    FocusedPanel::Graph => FocusedPanel::CommitDetail,
                    FocusedPanel::CommitDetail => FocusedPanel::Files,
                    FocusedPanel::Files => FocusedPanel::Graph,
                };
                return Ok(());
            }
            Action::PanelRight => {
                self.editing_commit_message = false;
                self.focused_panel = match self.focused_panel {
                    FocusedPanel::Graph => FocusedPanel::Files,
                    FocusedPanel::Files => FocusedPanel::CommitDetail,
                    FocusedPanel::CommitDetail => FocusedPanel::Graph,
                };
                return Ok(());
            }
            Action::FocusGraph => {
                if self.editing_commit_message {
                    self.editing_commit_message = false;
                } else if !self.files_filter.is_empty()
                    && self.focused_panel == FocusedPanel::Files
                {
                    // First Esc clears filter, second Esc goes to graph
                    self.files_filter.clear();
                } else {
                    self.focused_panel = FocusedPanel::Graph;
                    self.files_filter.clear();
                }
                return Ok(());
            }
            _ => {}
        }

        // Route to panel-specific handler
        match self.focused_panel {
            FocusedPanel::Graph => self.handle_graph_action(action),
            FocusedPanel::Files => self.handle_files_action(action),
            FocusedPanel::CommitDetail => self.handle_commit_detail_action(action),
        }
    }

    fn handle_graph_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::MoveUp => {
                self.move_selection(-1);
            }
            Action::MoveDown => {
                self.move_selection(1);
            }
            Action::PageUp => {
                self.move_selection(-10);
            }
            Action::PageDown => {
                self.move_selection(10);
            }
            Action::GoToTop => {
                self.select_first();
            }
            Action::GoToBottom => {
                self.select_last();
            }
            Action::JumpToHead => {
                self.jump_to_head();
            }
            Action::NextBranch => {
                self.move_to_next_branch();
            }
            Action::PrevBranch => {
                self.move_to_prev_branch();
            }
            Action::BranchLeft => {
                self.move_branch_left();
            }
            Action::BranchRight => {
                self.move_branch_right();
            }
            Action::ToggleHelp => {
                self.mode = AppMode::Help;
            }
            Action::Refresh => {
                self.refresh(true)?;
                self.reset_timers();
            }
            Action::Fetch => {
                if !self.is_fetching() {
                    self.start_fetch(true, false);
                }
            }
            Action::OpenCommitMenu => {
                self.open_commit_menu();
            }
            Action::OpenBranchFilter => {
                self.open_branch_filter();
            }
            Action::CreateBranch => {
                self.mode = AppMode::Input {
                    title: "New Branch Name".to_string(),
                    input: String::new(),
                    action: InputAction::CreateBranch,
                };
            }
            Action::Search => {
                self.save_search_position();
                self.mode = AppMode::Input {
                    title: "Search branches".to_string(),
                    input: String::new(),
                    action: InputAction::Search,
                };
            }
            Action::DeleteBranch => {
                if let Some(branch) = self.selected_branch() {
                    if !branch.is_head && !branch.is_remote {
                        self.mode = AppMode::Confirm {
                            message: format!("Delete branch '{}'?", branch.name),
                            action: ConfirmAction::DeleteBranch(branch.name.clone()),
                        };
                    }
                }
            }
            Action::EnterFileSelect => {
                if let Some(diff) = self.cached_diff() {
                    if diff.files.is_empty() {
                        self.set_message("No changed files in this diff");
                    } else {
                        let file_list = diff.files.clone();
                        self.mode = AppMode::FileSelect {
                            selected_index: 0,
                            file_list,
                        };
                    }
                } else if self.is_diff_loading() {
                    self.set_message("Loading diff...");
                } else {
                    self.set_message("Diff not available");
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_files_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::MoveUp | Action::MoveDown | Action::PageUp | Action::PageDown
            | Action::GoToTop | Action::GoToBottom => {
                // Auto-enter FileSelect mode if not already in it
                if !matches!(self.mode, AppMode::FileSelect { .. }) {
                    if let Some(diff) = self.cached_diff() {
                        if !diff.files.is_empty() {
                            let file_list = diff.files.clone();
                            self.mode = AppMode::FileSelect {
                                selected_index: 0,
                                file_list,
                            };
                        }
                    }
                }
                // Now delegate the actual movement to the FileSelect handler
                if matches!(self.mode, AppMode::FileSelect { .. }) {
                    let mapped = match action {
                        Action::MoveUp => Action::FileSelectUp,
                        Action::MoveDown => Action::FileSelectDown,
                        _ => action,
                    };
                    self.handle_file_select_action(mapped)?;
                }
            }
            Action::ToggleStage => {
                self.toggle_stage_selected_file()?;
            }
            Action::ToggleFolderView => {
                self.files_group_by_folder = !self.files_group_by_folder;
            }
            Action::FilesFilterChar(c) => {
                self.files_filter.push(c);
            }
            Action::FilesFilterBackspace => {
                if !self.files_filter.is_empty() {
                    self.files_filter.pop();
                } else {
                    // Empty filter + backspace = return to graph
                    self.focused_panel = FocusedPanel::Graph;
                }
            }
            Action::FilesFilterClear => {
                self.files_filter.clear();
            }
            Action::EnterFileSelect => {
                if let Some(diff) = self.cached_diff() {
                    if !diff.files.is_empty() {
                        let file_list = diff.files.clone();
                        self.mode = AppMode::FileSelect {
                            selected_index: 0,
                            file_list,
                        };
                    }
                }
            }
            Action::ToggleHelp => {
                self.mode = AppMode::Help;
            }
            Action::Refresh => {
                self.refresh(true)?;
                self.reset_timers();
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_commit_detail_action(&mut self, action: Action) -> Result<()> {
        if self.editing_commit_message {
            return self.handle_editor_action(action);
        }

        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::StartEditing => {
                if self.is_uncommitted_selected() {
                    self.editing_commit_message = true;
                }
            }
            Action::ToggleHelp => {
                self.mode = AppMode::Help;
            }
            Action::Refresh => {
                self.refresh(true)?;
                self.reset_timers();
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_editor_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::StopEditing => {
                self.editing_commit_message = false;
            }
            Action::CommitChanges => {
                let msg = self.commit_editor.text.trim().to_string();
                if !msg.is_empty() {
                    commit_with_message(&self.repo_path, &msg)?;
                    self.commit_editor = crate::text_editor::TextEditor::new();
                    self.editing_commit_message = false;
                    self.refresh(true)?;
                    self.set_message("Changes committed");
                }
            }
            Action::EditorChar(c) => self.commit_editor.insert_char(c),
            Action::EditorNewline => self.commit_editor.insert_newline(),
            Action::EditorBackspace => self.commit_editor.backspace(),
            Action::EditorDelete => self.commit_editor.delete(),
            Action::EditorLeft(s) => self.commit_editor.move_left(s),
            Action::EditorRight(s) => self.commit_editor.move_right(s),
            Action::EditorUp(s) => self.commit_editor.move_up(s),
            Action::EditorDown(s) => self.commit_editor.move_down(s),
            Action::EditorHome(s) => self.commit_editor.move_home(s),
            Action::EditorEnd(s) => self.commit_editor.move_end(s),
            Action::EditorWordLeft(s) => self.commit_editor.move_word_left(s),
            Action::EditorWordRight(s) => self.commit_editor.move_word_right(s),
            Action::EditorBackspaceWord => self.commit_editor.backspace_word(),
            Action::EditorDeleteWord => self.commit_editor.delete_word(),
            Action::EditorTextStart(s) => self.commit_editor.move_text_start(s),
            Action::EditorTextEnd(s) => self.commit_editor.move_text_end(s),
            _ => {}
        }
        Ok(())
    }

    fn open_commit_menu(&mut self) {
        let Some(node) = self.selected_commit_node() else {
            return;
        };

        if node.is_uncommitted {
            // For uncommitted node, go to files panel
            self.focused_panel = FocusedPanel::Files;
            return;
        }

        let has_branch = self.selected_branch().is_some();
        let mut items = Vec::new();

        // Push at top if available
        if has_branch {
            items.push(CommitMenuItem::Push);
        }

        items.push(CommitMenuItem::Checkout);
        items.push(CommitMenuItem::CreateBranch);

        if has_branch {
            if let Some(branch) = self.selected_branch() {
                if !branch.is_head {
                    items.push(CommitMenuItem::MergeIntoCurrent);
                }
            }
        }

        items.push(CommitMenuItem::CherryPick);

        if has_branch {
            if let Some(branch) = self.selected_branch() {
                if !branch.is_head {
                    items.push(CommitMenuItem::Rebase);
                }
            }
        }

        items.extend([
            CommitMenuItem::Reset,
            CommitMenuItem::AddTag,
            CommitMenuItem::Revert,
            CommitMenuItem::CopyHash,
        ]);

        self.mode = AppMode::CommitMenu {
            items,
            selected: 0,
        };
    }

    fn handle_commit_menu_action(&mut self, action: Action) -> Result<()> {
        let AppMode::CommitMenu { items, selected } = &self.mode else {
            return Ok(());
        };
        let items = items.clone();
        let selected = *selected;

        match action {
            Action::MoveUp => {
                let new = if selected == 0 { items.len().saturating_sub(1) } else { selected - 1 };
                self.mode = AppMode::CommitMenu { items, selected: new };
            }
            Action::MoveDown => {
                let new = if selected + 1 >= items.len() { 0 } else { selected + 1 };
                self.mode = AppMode::CommitMenu { items, selected: new };
            }
            Action::MenuSelect | Action::Confirm => {
                if let Some(item) = items.get(selected) {
                    self.execute_menu_item(*item)?;
                }
            }
            Action::Cancel | Action::Quit => {
                self.mode = AppMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    fn open_branch_filter(&mut self) {
        let mut all_branches: Vec<String> = self
            .branches
            .iter()
            .map(|b| b.name.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        all_branches.sort();
        self.mode = AppMode::BranchFilter {
            filter: String::new(),
            selected: 0,
            all_branches,
        };
    }

    fn handle_branch_filter_action(&mut self, action: Action) -> Result<()> {
        let AppMode::BranchFilter {
            filter,
            selected,
            all_branches,
        } = &self.mode
        else {
            return Ok(());
        };
        let filter = filter.clone();
        let selected = *selected;
        let all_branches = all_branches.clone();

        // Compute filtered list for navigation
        let filtered: Vec<&String> = all_branches
            .iter()
            .filter(|b| b.to_lowercase().contains(&filter.to_lowercase()))
            .collect();

        match action {
            Action::MoveUp => {
                if filtered.is_empty() {
                    return Ok(());
                }
                let new = if selected == 0 {
                    filtered.len().saturating_sub(1)
                } else {
                    selected - 1
                };
                self.mode = AppMode::BranchFilter {
                    filter,
                    selected: new,
                    all_branches,
                };
            }
            Action::MoveDown => {
                if filtered.is_empty() {
                    return Ok(());
                }
                let new = if selected + 1 >= filtered.len() {
                    0
                } else {
                    selected + 1
                };
                self.mode = AppMode::BranchFilter {
                    filter,
                    selected: new,
                    all_branches,
                };
            }
            Action::Confirm | Action::MenuSelect => {
                // Toggle the selected branch
                if let Some(branch_name) = filtered.get(selected) {
                    let name = (*branch_name).clone();
                    if self.hidden_branches.contains(&name) {
                        self.hidden_branches.remove(&name);
                    } else {
                        self.hidden_branches.insert(name);
                    }
                }
                // Stay in BranchFilter mode
                self.mode = AppMode::BranchFilter {
                    filter,
                    selected,
                    all_branches,
                };
            }
            Action::InputChar('a') => {
                self.hidden_branches.clear();
                self.mode = AppMode::BranchFilter {
                    filter,
                    selected,
                    all_branches,
                };
            }
            Action::InputChar('n') => {
                for b in &all_branches {
                    self.hidden_branches.insert(b.clone());
                }
                self.mode = AppMode::BranchFilter {
                    filter,
                    selected,
                    all_branches,
                };
            }
            Action::InputChar(c) => {
                let mut new_filter = filter;
                new_filter.push(c);
                // Reset selection when filter changes
                self.mode = AppMode::BranchFilter {
                    filter: new_filter,
                    selected: 0,
                    all_branches,
                };
            }
            Action::InputBackspace => {
                let mut new_filter = filter;
                new_filter.pop();
                self.mode = AppMode::BranchFilter {
                    filter: new_filter,
                    selected: 0,
                    all_branches,
                };
            }
            Action::Cancel | Action::Quit => {
                self.mode = AppMode::Normal;
                self.refresh(true)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn execute_menu_item(&mut self, item: CommitMenuItem) -> Result<()> {
        self.mode = AppMode::Normal;

        let commit_oid = self
            .selected_commit_node()
            .and_then(|n| n.commit.as_ref())
            .map(|c| c.oid);

        match item {
            CommitMenuItem::Checkout => self.do_checkout()?,
            CommitMenuItem::CreateBranch => {
                self.mode = AppMode::Input {
                    title: "New Branch Name".to_string(),
                    input: String::new(),
                    action: InputAction::CreateBranch,
                };
            }
            CommitMenuItem::MergeIntoCurrent => {
                if let Some(branch) = self.selected_branch() {
                    if !branch.is_head {
                        self.mode = AppMode::Confirm {
                            message: format!("Merge '{}' into current branch?", branch.name),
                            action: ConfirmAction::Merge(branch.name.clone()),
                        };
                    }
                }
            }
            CommitMenuItem::CherryPick => {
                if let Some(oid) = commit_oid {
                    self.mode = AppMode::Confirm {
                        message: format!("Cherry-pick commit {}?", &oid.to_string()[..7]),
                        action: ConfirmAction::CherryPick(oid),
                    };
                }
            }
            CommitMenuItem::Rebase => {
                if let Some(branch) = self.selected_branch() {
                    if !branch.is_head {
                        self.mode = AppMode::Confirm {
                            message: format!("Rebase current branch onto '{}'?", branch.name),
                            action: ConfirmAction::Rebase(branch.name.clone()),
                        };
                    }
                }
            }
            CommitMenuItem::Reset => {
                // Open reset submenu
                self.mode = AppMode::CommitMenu {
                    items: vec![
                        CommitMenuItem::ResetSoft,
                        CommitMenuItem::ResetMixed,
                        CommitMenuItem::ResetHard,
                    ],
                    selected: 0,
                };
            }
            CommitMenuItem::ResetSoft => {
                if let Some(oid) = commit_oid {
                    self.mode = AppMode::Confirm {
                        message: format!("Reset (soft) to {}?", &oid.to_string()[..7]),
                        action: ConfirmAction::ResetSoft(oid),
                    };
                }
            }
            CommitMenuItem::ResetMixed => {
                if let Some(oid) = commit_oid {
                    self.mode = AppMode::Confirm {
                        message: format!("Reset (mixed) to {}?", &oid.to_string()[..7]),
                        action: ConfirmAction::ResetMixed(oid),
                    };
                }
            }
            CommitMenuItem::ResetHard => {
                if let Some(oid) = commit_oid {
                    self.mode = AppMode::Confirm {
                        message: format!(
                            "Reset (HARD) to {}? This will discard changes!",
                            &oid.to_string()[..7]
                        ),
                        action: ConfirmAction::ResetHard(oid),
                    };
                }
            }
            CommitMenuItem::AddTag => {
                self.mode = AppMode::Input {
                    title: "Tag Name".to_string(),
                    input: String::new(),
                    action: InputAction::AddTag,
                };
            }
            CommitMenuItem::Revert => {
                if let Some(oid) = commit_oid {
                    self.mode = AppMode::Confirm {
                        message: format!("Revert commit {}?", &oid.to_string()[..7]),
                        action: ConfirmAction::Revert(oid),
                    };
                }
            }
            CommitMenuItem::CopyHash => {
                if let Some(oid) = commit_oid {
                    let hash = oid.to_string();
                    match copy_to_clipboard(&hash) {
                        Ok(()) => self.set_message(format!("Copied {}", &hash[..7])),
                        Err(e) => self.set_message(format!("Clipboard error: {}", e)),
                    }
                }
            }
            CommitMenuItem::Push => {
                self.mode = AppMode::Confirm {
                    message: "Push current branch to origin?".to_string(),
                    action: ConfirmAction::Push,
                };
            }
        }
        Ok(())
    }

    fn toggle_stage_selected_file(&mut self) -> Result<()> {
        if !self.is_uncommitted_selected() {
            return Ok(());
        }

        // Get the selected file - from FileSelect mode or first file
        let file_to_toggle = if let AppMode::FileSelect {
            selected_index,
            file_list,
        } = &self.mode
        {
            file_list.get(*selected_index).cloned()
        } else {
            // Not in FileSelect mode - try to get the first file from diff
            self.cached_diff()
                .and_then(|diff| diff.files.first().cloned())
        };

        if let Some(file) = file_to_toggle {
            let path_str = file.path.to_string_lossy().to_string();
            match file.stage_status {
                Some(StageStatus::Staged) => unstage_file(&self.repo_path, &path_str)?,
                _ => stage_file(&self.repo_path, &path_str)?,
            }
            self.clear_uncommitted_diff_cache();
            self.refresh(true)?;

            // Re-enter FileSelect mode if we were in it, keeping position
            if let Some(diff) = self.cached_diff() {
                if !diff.files.is_empty() {
                    let file_list = diff.files.clone();
                    let idx = if let AppMode::FileSelect { selected_index, .. } = &self.mode {
                        (*selected_index).min(file_list.len().saturating_sub(1))
                    } else {
                        0
                    };
                    self.mode = AppMode::FileSelect {
                        selected_index: idx,
                        file_list,
                    };
                }
            }
        }
        Ok(())
    }

    fn handle_help_action(&mut self, action: Action) {
        if matches!(action, Action::ToggleHelp | Action::Quit | Action::Cancel) {
            self.mode = AppMode::Normal;
        }
    }

    fn handle_error_action(&mut self, action: Action) {
        // Close the error on any key
        if matches!(action, Action::Quit | Action::Cancel | Action::Confirm) {
            self.mode = AppMode::Normal;
        }
    }

    fn handle_file_select_action(&mut self, action: Action) -> Result<()> {
        let AppMode::FileSelect {
            selected_index,
            file_list,
        } = &self.mode
        else {
            return Ok(());
        };
        let selected_index = *selected_index;
        let file_count = file_list.len();

        match action {
            Action::FileSelectUp => {
                if selected_index > 0 {
                    if let AppMode::FileSelect { selected_index, .. } = &mut self.mode {
                        *selected_index -= 1;
                    }
                }
            }
            Action::FileSelectDown => {
                if selected_index + 1 < file_count {
                    if let AppMode::FileSelect { selected_index, .. } = &mut self.mode {
                        *selected_index += 1;
                    }
                }
            }
            Action::PageUp => {
                if let AppMode::FileSelect { selected_index, .. } = &mut self.mode {
                    *selected_index = selected_index.saturating_sub(10);
                }
            }
            Action::PageDown => {
                if let AppMode::FileSelect { selected_index, .. } = &mut self.mode {
                    *selected_index = (*selected_index + 10).min(file_count.saturating_sub(1));
                }
            }
            Action::ToggleStage => {
                self.toggle_stage_selected_file()?;
            }
            Action::OpenFileDiff => {
                let file_list_snapshot = if let AppMode::FileSelect { file_list, .. } = &self.mode {
                    file_list.clone()
                } else {
                    return Ok(());
                };

                if selected_index < file_list_snapshot.len() {
                    let path = file_list_snapshot[selected_index].path.clone();
                    if let Err(e) = self.enter_file_diff(selected_index, file_list_snapshot, &path)
                    {
                        self.set_message(format!("Cannot open diff: {e}"));
                    }
                }
            }
            Action::Cancel | Action::Quit => {
                self.return_to_normal();
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_file_diff_action(&mut self, action: Action) -> Result<()> {
        let AppMode::FileDiff {
            total_lines,
            max_line_width,
            file_index,
            ..
        } = &self.mode
        else {
            return Ok(());
        };
        let total_lines = *total_lines;
        let max_line_width = *max_line_width;
        let file_index = *file_index;
        let viewport = self.diff_viewport_height as usize;
        let half_page = (viewport / 2).max(1);
        let max_scroll = total_lines.saturating_sub(viewport);
        let h_viewport = self.diff_viewport_width as usize;
        let max_horizontal = max_line_width.saturating_sub(h_viewport);
        const H_SCROLL_STEP: usize = 4;

        match action {
            Action::ScrollDown => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = (*scroll_offset + 1).min(max_scroll);
                }
            }
            Action::ScrollUp => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = scroll_offset.saturating_sub(1);
                }
            }
            Action::ScrollPageDown => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = (*scroll_offset + half_page).min(max_scroll);
                }
            }
            Action::ScrollPageUp => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = scroll_offset.saturating_sub(half_page);
                }
            }
            Action::PageDown => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = (*scroll_offset + viewport).min(max_scroll);
                }
            }
            Action::PageUp => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = scroll_offset.saturating_sub(viewport);
                }
            }
            Action::ScrollToTop => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = 0;
                }
            }
            Action::ScrollToBottom => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = max_scroll;
                }
            }
            Action::ScrollRight => {
                if let AppMode::FileDiff {
                    horizontal_offset, ..
                } = &mut self.mode
                {
                    *horizontal_offset = (*horizontal_offset + H_SCROLL_STEP).min(max_horizontal);
                }
            }
            Action::ScrollLeft => {
                if let AppMode::FileDiff {
                    horizontal_offset, ..
                } = &mut self.mode
                {
                    *horizontal_offset = horizontal_offset.saturating_sub(H_SCROLL_STEP);
                }
            }
            Action::ScrollToLineStart => {
                if let AppMode::FileDiff {
                    horizontal_offset, ..
                } = &mut self.mode
                {
                    *horizontal_offset = 0;
                }
            }
            Action::NextHunk => {
                if let AppMode::FileDiff {
                    scroll_offset,
                    hunk_positions,
                    ..
                } = &mut self.mode
                {
                    // Find next hunk after current scroll position
                    if let Some(&pos) = hunk_positions.iter().find(|&&p| p > *scroll_offset) {
                        *scroll_offset = pos.min(max_scroll);
                    }
                }
            }
            Action::PrevHunk => {
                if let AppMode::FileDiff {
                    scroll_offset,
                    hunk_positions,
                    ..
                } = &mut self.mode
                {
                    // Find previous hunk before current scroll position
                    if let Some(&pos) = hunk_positions.iter().rev().find(|&&p| p < *scroll_offset) {
                        *scroll_offset = pos.min(max_scroll);
                    }
                }
            }
            Action::NextFile => {
                let file_list_snapshot = if let AppMode::FileDiff { file_list, .. } = &self.mode {
                    file_list.clone()
                } else {
                    return Ok(());
                };
                if !file_list_snapshot.is_empty() {
                    let new_index = (file_index + 1) % file_list_snapshot.len();
                    let path = file_list_snapshot[new_index].path.clone();
                    if let Err(e) = self.enter_file_diff(new_index, file_list_snapshot, &path) {
                        self.set_message(format!("Cannot open diff: {e}"));
                    }
                }
            }
            Action::PrevFile => {
                let file_list_snapshot = if let AppMode::FileDiff { file_list, .. } = &self.mode {
                    file_list.clone()
                } else {
                    return Ok(());
                };
                if !file_list_snapshot.is_empty() {
                    let new_index = if file_index == 0 {
                        file_list_snapshot.len() - 1
                    } else {
                        file_index - 1
                    };
                    let path = file_list_snapshot[new_index].path.clone();
                    if let Err(e) = self.enter_file_diff(new_index, file_list_snapshot, &path) {
                        self.set_message(format!("Cannot open diff: {e}"));
                    }
                }
            }
            Action::Cancel | Action::Quit => {
                // Return to FileSelect with file_index preserved
                let (file_index, file_list) = if let AppMode::FileDiff {
                    file_index,
                    file_list,
                    ..
                } = &self.mode
                {
                    (*file_index, file_list.clone())
                } else {
                    return Ok(());
                };
                self.mode = AppMode::FileSelect {
                    selected_index: file_index,
                    file_list,
                };
            }
            _ => {}
        }
        Ok(())
    }

    fn enter_file_diff(
        &mut self,
        file_index: usize,
        file_list: Vec<FileDiffInfo>,
        file_path: &std::path::Path,
    ) -> Result<()> {
        use crate::ui::file_diff_view::build_highlighted_lines;

        // NOTE: Runs synchronously on the UI thread. For very large diffs (e.g. generated
        // files, large refactors) this may briefly block input. If this becomes a problem,
        // consider moving to a background task with a loading state, similar to commit diff summaries.
        let content = self.load_file_diff_content(file_path)?;
        let (rendered_lines, hunk_positions) = build_highlighted_lines(&content);
        let total_lines = rendered_lines.len();
        let max_line_width = rendered_lines.iter().map(|l| l.width()).max().unwrap_or(0);

        self.mode = AppMode::FileDiff {
            file_index,
            file_list,
            content,
            rendered_lines,
            hunk_positions,
            scroll_offset: 0,
            horizontal_offset: 0,
            max_line_width,
            total_lines,
        };
        Ok(())
    }

    fn load_file_diff_content(&self, file_path: &std::path::Path) -> Result<FileDiffContent> {
        match self.current_diff_target() {
            Some(DiffTarget::Commit(oid)) => {
                FileDiffContent::from_commit(&self.repo.repo, oid, file_path)
            }
            Some(DiffTarget::Uncommitted) | None => {
                FileDiffContent::from_working_tree(&self.repo.repo, file_path)
            }
        }
    }

    /// Sync the file_list held by FileSelect / FileDiff with the latest
    /// uncommitted diff cache.  Called right after `uncommitted_diff_cache` is
    /// updated so that navigation and display stay consistent.
    fn sync_file_list_with_uncommitted_diff(&mut self) {
        if self.current_diff_target() != Some(DiffTarget::Uncommitted) {
            return;
        }

        let new_files = match &self.uncommitted_diff_cache {
            Some(diff) => diff.files.clone(),
            None => return,
        };

        if new_files.is_empty() {
            if matches!(
                self.mode,
                AppMode::FileSelect { .. } | AppMode::FileDiff { .. }
            ) {
                self.mode = AppMode::Normal;
                self.set_message("No changed files in this diff");
            }
            return;
        }

        match &mut self.mode {
            AppMode::FileSelect {
                selected_index,
                file_list,
            } => {
                *file_list = new_files;
                if *selected_index >= file_list.len() {
                    *selected_index = file_list.len() - 1;
                }
            }
            AppMode::FileDiff {
                file_index,
                file_list,
                ..
            } => {
                let current_path = file_list.get(*file_index).map(|f| f.path.clone());
                *file_list = new_files;
                if let Some(path) = current_path {
                    if let Some(new_idx) = file_list.iter().position(|f| f.path == path) {
                        *file_index = new_idx;
                    } else if *file_index >= file_list.len() {
                        *file_index = file_list.len() - 1;
                    }
                }
            }
            _ => {}
        }
    }

    fn return_to_normal(&mut self) {
        self.mode = AppMode::Normal;
        if self.pending_refresh {
            self.pending_refresh = false;
            if let Err(e) = self.refresh(true) {
                self.set_message(format!("Refresh failed: {e}"));
            }
        }
    }

    fn handle_input_action(&mut self, action: Action) -> Result<()> {
        let AppMode::Input {
            title,
            input,
            action: input_action,
        } = &self.mode
        else {
            return Ok(());
        };
        let (title, mut input, input_action) = (title.clone(), input.clone(), input_action.clone());

        match action {
            Action::Confirm => {
                match input_action {
                    InputAction::CreateBranch => {
                        if !input.is_empty() {
                            if let Some(node) = self.selected_commit_node() {
                                if let Some(commit) = &node.commit {
                                    create_branch(&self.repo.repo, &input, commit.oid)?;
                                    self.refresh(true)?;
                                }
                            }
                        }
                    }
                    InputAction::AddTag => {
                        if !input.is_empty() {
                            if let Some(node) = self.selected_commit_node() {
                                if let Some(commit) = &node.commit {
                                    add_tag(&self.repo.repo, &input, commit.oid)?;
                                    self.refresh(true)?;
                                    self.set_message(format!("Tag '{}' created", input));
                                }
                            }
                        }
                    }
                    InputAction::Search => {
                        self.jump_to_search_result();
                    }
                }
                // Clear search state after confirming
                self.search_state = SearchState::default();
                self.mode = AppMode::Normal;
            }
            Action::Cancel => {
                // Restore position when canceling search
                if matches!(input_action, InputAction::Search) {
                    self.restore_search_position();
                }
                self.search_state = SearchState::default();
                self.mode = AppMode::Normal;
            }
            Action::InputChar(c) => {
                input.push(c);

                // Incremental fuzzy search with live preview
                if matches!(input_action, InputAction::Search) {
                    self.update_fuzzy_search(&input);
                    self.jump_to_search_result();
                }

                self.mode = AppMode::Input {
                    title,
                    input,
                    action: input_action,
                };
            }
            Action::InputBackspace => {
                // Empty input + backspace = cancel (like Esc)
                if input.is_empty() {
                    if matches!(input_action, InputAction::Search) {
                        self.restore_search_position();
                    }
                    self.search_state = SearchState::default();
                    self.mode = AppMode::Normal;
                    return Ok(());
                }

                input.pop();

                // Update fuzzy search on backspace with live preview
                if matches!(input_action, InputAction::Search) {
                    self.update_fuzzy_search(&input);
                    self.jump_to_search_result();
                }

                self.mode = AppMode::Input {
                    title,
                    input,
                    action: input_action,
                };
            }
            Action::SearchSelectUp => {
                self.search_state.select_up();
                self.jump_to_search_result();
            }
            Action::SearchSelectDown => {
                self.search_state.select_down();
                self.jump_to_search_result();
            }
            Action::SearchSelectUpQuiet => {
                self.search_state.select_up();
                // No graph jump - just move in dropdown
            }
            Action::SearchSelectDownQuiet => {
                self.search_state.select_down();
                // No graph jump - just move in dropdown
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_confirm_action(&mut self, action: Action) -> Result<()> {
        let AppMode::Confirm {
            action: confirm_action,
            ..
        } = &self.mode
        else {
            return Ok(());
        };
        let confirm_action = confirm_action.clone();

        match action {
            Action::Confirm => {
                match confirm_action {
                    ConfirmAction::DeleteBranch(name) => {
                        delete_branch(&self.repo.repo, &name)?;
                    }
                    ConfirmAction::Merge(name) => {
                        merge_branch(&self.repo.repo, &name)?;
                    }
                    ConfirmAction::Rebase(name) => {
                        rebase_branch(&self.repo.repo, &name)?;
                    }
                    ConfirmAction::CherryPick(oid) => {
                        cherry_pick(&self.repo_path, oid)?;
                    }
                    ConfirmAction::Revert(oid) => {
                        revert_commit(&self.repo_path, oid)?;
                    }
                    ConfirmAction::ResetSoft(oid) => {
                        reset_to_commit(&self.repo_path, oid, ResetMode::Soft)?;
                    }
                    ConfirmAction::ResetMixed(oid) => {
                        reset_to_commit(&self.repo_path, oid, ResetMode::Mixed)?;
                    }
                    ConfirmAction::ResetHard(oid) => {
                        reset_to_commit(&self.repo_path, oid, ResetMode::Hard)?;
                    }
                    ConfirmAction::Push => {
                        push_to_origin(&self.repo_path)?;
                        self.set_message("Pushed to origin");
                    }
                }
                self.refresh(true)?;
                self.mode = AppMode::Normal;
            }
            Action::Cancel => {
                self.mode = AppMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    fn move_selection(&mut self, delta: i32) {
        let max = self.graph_layout.nodes.len().saturating_sub(1);
        let current = self.graph_list_state.selected().unwrap_or(0);
        let new = (current as i32 + delta).clamp(0, max as i32) as usize;
        self.graph_list_state.select(Some(new));
        self.sync_branch_selection_to_node(new);
    }

    fn select_first(&mut self) {
        self.graph_list_state.select(Some(0));
        self.sync_branch_selection_to_node(0);
    }

    fn select_last(&mut self) {
        let max = self.graph_layout.nodes.len().saturating_sub(1);
        self.graph_list_state.select(Some(max));
        self.sync_branch_selection_to_node(max);
    }

    /// Sync branch selection to the first branch of the given node
    fn sync_branch_selection_to_node(&mut self, node_idx: usize) {
        self.selected_branch_position = self
            .branch_positions
            .iter()
            .position(|(idx, _)| *idx == node_idx);
    }

    /// Move to the next branch (across all commits)
    fn move_to_next_branch(&mut self) {
        if self.branch_positions.is_empty() {
            return;
        }

        let next = match self.selected_branch_position {
            Some(pos) => {
                if pos + 1 < self.branch_positions.len() {
                    pos + 1
                } else {
                    return; // Already at the last branch
                }
            }
            None => 0, // No branch selected, select the first one
        };

        self.selected_branch_position = Some(next);
        if let Some((node_idx, _)) = self.branch_positions.get(next) {
            self.graph_list_state.select(Some(*node_idx));
        }
    }

    /// Move to the previous branch (across all commits)
    fn move_to_prev_branch(&mut self) {
        if self.branch_positions.is_empty() {
            return;
        }

        let prev = match self.selected_branch_position {
            Some(pos) => {
                if pos > 0 {
                    pos - 1
                } else {
                    return; // Already at the first branch
                }
            }
            None => self.branch_positions.len() - 1, // No branch selected, select the last one
        };

        self.selected_branch_position = Some(prev);
        if let Some((node_idx, _)) = self.branch_positions.get(prev) {
            self.graph_list_state.select(Some(*node_idx));
        }
    }

    /// Move to an adjacent branch within the same commit
    fn move_branch_within_node(&mut self, delta: isize) {
        let Some(pos) = self.selected_branch_position else {
            return;
        };

        let new_pos = (pos as isize + delta) as usize;
        if new_pos >= self.branch_positions.len() {
            return;
        }

        let Some((current_node, _)) = self.branch_positions.get(pos) else {
            return;
        };
        let Some((target_node, _)) = self.branch_positions.get(new_pos) else {
            return;
        };

        // Only move within the same commit
        if current_node == target_node {
            self.selected_branch_position = Some(new_pos);
        }
    }

    /// Move to the left branch within the same commit
    fn move_branch_left(&mut self) {
        self.move_branch_within_node(-1);
    }

    /// Move to the right branch within the same commit
    fn move_branch_right(&mut self) {
        self.move_branch_within_node(1);
    }

    /// Get the currently selected branch
    fn selected_branch(&self) -> Option<&BranchInfo> {
        let (_, branch_name) = self
            .selected_branch_position
            .and_then(|pos| self.branch_positions.get(pos))?;
        self.branches.iter().find(|b| &b.name == branch_name)
    }

    /// Get the name of the currently selected branch
    pub fn selected_branch_name(&self) -> Option<&str> {
        self.selected_branch_position
            .and_then(|pos| self.branch_positions.get(pos))
            .map(|(_, name)| name.as_str())
    }

    /// Returns all branch names for the currently selected node
    pub fn selected_node_branches(&self) -> Vec<&str> {
        let Some(node_idx) = self.graph_list_state.selected() else {
            return vec![];
        };
        self.branch_positions
            .iter()
            .filter(|(idx, _)| *idx == node_idx)
            .map(|(_, name)| name.as_str())
            .collect()
    }

    fn selected_commit_node(&self) -> Option<&crate::git::graph::GraphNode> {
        self.graph_list_state
            .selected()
            .and_then(|i| self.graph_layout.nodes.get(i))
    }

    fn do_checkout(&mut self) -> Result<()> {
        if let Some(branch) = self.selected_branch() {
            let branch_name = branch.name.clone();
            if branch_name.starts_with("origin/") {
                // For remote branches, create a local branch and check it out
                checkout_remote_branch(&self.repo.repo, &branch_name)?;
            } else {
                checkout_branch(&self.repo.repo, &branch_name)?;
            }
            self.refresh(true)?;
        } else if let Some(node) = self.selected_commit_node() {
            if let Some(commit) = &node.commit {
                checkout_commit(&self.repo.repo, commit.oid)?;
                self.refresh(true)?;
            }
        }
        Ok(())
    }

    /// Build a flat list of (node_index, branch_name) for all branches
    /// Excludes remote branches that have a matching local branch (e.g., origin/main when main exists)
    /// Order matches optimize_branch_display: local branches first, then remote-only branches
    fn build_branch_positions(graph_layout: &GraphLayout) -> Vec<(usize, String)> {
        graph_layout
            .nodes
            .iter()
            .enumerate()
            .flat_map(|(node_idx, node)| {
                filter_remote_duplicates(&node.branch_names)
                    .into_iter()
                    .map(move |name| (node_idx, name.to_string()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use chrono::Local;
    use git2::{Oid, Repository, Signature};
    use tempfile::TempDir;

    use super::*;
    use crate::git::graph::{CellType, GraphNode};

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

    fn make_app_from_repo(repo: GitRepository) -> App {
        let now = Instant::now();
        let commits = repo.get_commits(500).unwrap();
        let branches = repo.get_branches().unwrap();
        let (working_tree_status, initial_message) = App::working_tree_status_snapshot(&repo);
        let initial_message_time = initial_message.as_ref().map(|_| now);
        let uncommitted_count = working_tree_status
            .as_ref()
            .map(|s| s.accurate_file_count());
        let head_commit_oid = repo.head_oid();
        let graph_layout = build_graph(&commits, &branches, uncommitted_count, head_commit_oid);

        let mut graph_list_state = ListState::default();
        graph_list_state.select(Some(0));

        let branch_positions = App::build_branch_positions(&graph_layout);
        let has_uncommitted_node = graph_layout
            .nodes
            .first()
            .is_some_and(|node| node.is_uncommitted);
        let selected_branch_position = if has_uncommitted_node || branch_positions.is_empty() {
            None
        } else {
            Some(0)
        };

        App {
            mode: AppMode::Normal,
            repo,
            repo_path: String::new(),
            head_name: None,
            commits,
            branches,
            graph_layout,
            graph_list_state,
            focused_panel: FocusedPanel::Graph,
            files_group_by_folder: false,
            files_filter: String::new(),
            hidden_branches: std::collections::HashSet::new(),
            commit_editor: crate::text_editor::TextEditor::new(),
            editing_commit_message: false,
            branch_positions,
            selected_branch_position,
            search_state: SearchState::default(),
            working_tree_status,
            quick_diff_cache: None,
            quick_diff_target: None,
            diff_cache: None,
            diff_cache_oid: None,
            diff_loading_oid: None,
            diff_receiver: None,
            uncommitted_diff_cache: None,
            uncommitted_diff_failed: false,
            uncommitted_diff_loading: false,
            uncommitted_diff_receiver: None,
            uncommitted_cache_key: None,
            selected_diff_target: None,
            selected_diff_target_changed_at: now,
            should_quit: false,
            pending_refresh: false,
            diff_viewport_height: 40,
            diff_viewport_width: 80,
            message: initial_message,
            message_time: initial_message_time,
            fetch_receiver: None,
            fetch_silent: false,
            config: Config::default(),
            last_refresh_time: now,
            last_fetch_time: now,
        }
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
        let mut graph_list_state = ListState::default();
        graph_list_state.select(Some(0));

        let commits = node.commit.iter().cloned().collect();

        App {
            mode: AppMode::Normal,
            repo_path: repo.path.clone(),
            repo,
            head_name: None,
            commits,
            branches: Vec::new(),
            graph_layout: GraphLayout {
                nodes: vec![node],
                max_lane: 0,
            },
            graph_list_state,
            focused_panel: FocusedPanel::Graph,
            files_group_by_folder: false,
            files_filter: String::new(),
            hidden_branches: std::collections::HashSet::new(),
            commit_editor: crate::text_editor::TextEditor::new(),
            editing_commit_message: false,
            branch_positions: Vec::new(),
            selected_branch_position: None,
            search_state: SearchState::default(),
            working_tree_status,
            quick_diff_cache: None,
            quick_diff_target: None,
            diff_cache: None,
            diff_cache_oid: None,
            diff_loading_oid: None,
            diff_receiver: None,
            uncommitted_diff_cache: None,
            uncommitted_diff_failed: false,
            uncommitted_diff_loading: false,
            uncommitted_diff_receiver: None,
            uncommitted_cache_key: None,
            selected_diff_target: Some(diff_target),
            selected_diff_target_changed_at: Instant::now() - DIFF_LOAD_DEBOUNCE,
            should_quit: false,
            pending_refresh: false,
            diff_viewport_height: 40,
            diff_viewport_width: 80,
            message: None,
            message_time: None,
            fetch_receiver: None,
            fetch_silent: false,
            config: Config::default(),
            last_refresh_time: Instant::now(),
            last_fetch_time: Instant::now(),
        }
    }

    fn make_app(selected_oid: Oid, in_flight_oid: Option<Oid>) -> App {
        let node = GraphNode {
            commit: Some(make_commit(selected_oid)),
            lane: 0,
            color_index: 0,
            branch_names: Vec::new(),
            is_head: false,
            is_uncommitted: false,
            uncommitted_count: None,
            cells: vec![CellType::Commit(0)],
        };
        let mut app = make_base_app(node, DiffTarget::Commit(selected_oid), None);
        app.diff_loading_oid = in_flight_oid;
        app
    }

    fn make_uncommitted_app() -> App {
        let node = GraphNode {
            commit: None,
            lane: 0,
            color_index: 0,
            branch_names: Vec::new(),
            is_head: false,
            is_uncommitted: true,
            uncommitted_count: Some(1),
            cells: vec![CellType::Commit(0)],
        };
        let wts = WorkingTreeStatus {
            file_paths: vec![PathBuf::from("tracked.txt")],
            mtime_hash: 1,
            has_collapsed_untracked_dirs: false,
        };
        make_base_app(node, DiffTarget::Uncommitted, Some(wts))
    }

    #[test]
    fn selected_diff_stays_loading_while_another_diff_is_in_flight() {
        let selected_oid = Oid::from_str("1111111111111111111111111111111111111111").unwrap();
        let in_flight_oid = Oid::from_str("2222222222222222222222222222222222222222").unwrap();
        let app = make_app(selected_oid, Some(in_flight_oid));

        assert!(app.is_diff_loading());
    }

    #[test]
    fn cached_selected_diff_is_not_marked_loading_by_other_requests() {
        let selected_oid = Oid::from_str("1111111111111111111111111111111111111111").unwrap();
        let in_flight_oid = Oid::from_str("2222222222222222222222222222222222222222").unwrap();
        let mut app = make_app(selected_oid, Some(in_flight_oid));
        app.diff_cache = Some(CommitDiffInfo::default());
        app.diff_cache_oid = Some(selected_oid);

        assert!(!app.is_diff_loading());
    }

    #[test]
    fn failed_commit_diff_load_is_cached_to_avoid_immediate_retry() {
        let selected_oid = Oid::from_str("1111111111111111111111111111111111111111").unwrap();
        let mut app = make_app(selected_oid, Some(selected_oid));
        let (tx, rx) = mpsc::channel();
        tx.send(DiffResult {
            oid: selected_oid,
            diff: Err("boom".to_string()),
        })
        .unwrap();
        app.diff_receiver = Some(rx);

        app.update_diff_cache();
        app.update_diff_cache();

        assert!(app.diff_cache.is_none());
        assert_eq!(app.diff_cache_oid, Some(selected_oid));
        assert!(app.cached_diff().is_none());
        assert!(!app.is_diff_loading());
        assert!(app.diff_loading_oid.is_none());
        assert!(app.diff_receiver.is_none());
        assert_eq!(app.message.as_deref(), Some("Failed to load diff: boom"));
    }

    #[test]
    fn failed_uncommitted_diff_load_is_cached_to_avoid_immediate_retry() {
        let mut app = make_uncommitted_app();
        let (tx, rx) = mpsc::channel();
        let cache_key = app.working_tree_status.clone();
        tx.send((Err("boom".to_string()), cache_key)).unwrap();
        app.uncommitted_diff_loading = true;
        app.uncommitted_diff_receiver = Some(rx);

        app.update_diff_cache();
        app.update_diff_cache();

        assert!(app.uncommitted_diff_cache.is_none());
        assert!(app.uncommitted_diff_failed);
        assert!(app.cached_diff().is_none());
        assert!(!app.is_diff_loading());
        assert!(!app.uncommitted_diff_loading);
        assert!(app.uncommitted_diff_receiver.is_none());
        assert_eq!(app.message.as_deref(), Some("Failed to load diff: boom"));
    }

    #[test]
    fn refresh_reuses_uncommitted_cache_for_nested_untracked_directories() {
        let tempdir = tempfile::tempdir().unwrap();
        let repo = Repository::init(tempdir.path()).unwrap();
        let _oid = commit_file(&repo, "tracked.txt", "tracked\n", "initial");
        fs::create_dir_all(tempdir.path().join("dir/sub")).unwrap();
        fs::write(tempdir.path().join("dir/sub/file.txt"), "hello\n").unwrap();

        let git_repo = GitRepository::open(tempdir.path()).unwrap();
        let mut app = make_app_from_repo(git_repo);
        app.uncommitted_diff_cache = Some(CommitDiffInfo::default());
        app.uncommitted_cache_key = app.working_tree_status.clone();

        app.refresh(false).unwrap();

        // recurse_untracked_dirs lists individual files instead of collapsed
        // directory entries, so the cache key is precise and can be reused.
        assert!(app.uncommitted_diff_cache.is_some());
        assert!(app.uncommitted_cache_key.is_some());
    }

    #[test]
    fn refresh_restores_non_branch_selection_by_commit_oid_when_uncommitted_row_is_added() {
        let tempdir = tempfile::tempdir().unwrap();
        let repo = Repository::init(tempdir.path()).unwrap();
        let first_oid = commit_file(&repo, "tracked.txt", "first\n", "first");
        let _second_oid = commit_file(&repo, "tracked.txt", "second\n", "second");

        let git_repo = GitRepository::open(tempdir.path()).unwrap();
        let mut app = make_app_from_repo(git_repo);

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
        app.graph_list_state.select(Some(first_node_idx));
        app.sync_branch_selection_to_node(first_node_idx);

        fs::write(tempdir.path().join("untracked.txt"), "hello\n").unwrap();
        app.refresh(false).unwrap();

        let selected_oid = app
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
}
