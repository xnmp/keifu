//! Application state management

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};
use std::{path::PathBuf, process::Command};

use anyhow::Result;
use ratatui::widgets::ListState;

use git2::Oid;

use crate::{
    action::Action,
    config::Config,
    files_pane::FilesPane,
    git::{
        build_graph,
        graph::GraphLayout,
        operations::{
            checkout_branch, checkout_commit, checkout_remote_branch, commit_with_message,
            create_branch, delete_branch, fetch_origin, merge_branch, rebase_branch,
        },
        BranchInfo, CommitDiffInfo, CommitInfo, GitRepository, WorkingTreeStatus,
    },
    search::{fuzzy_search_branches, FuzzySearchResult},
};

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

/// Application modes
#[derive(Debug, Clone)]
pub enum AppMode {
    Graph,
    Files,
    Detail,
    Modal {
        title: String,
        message: String,
        scroll: u16,
    },
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
}

/// Input action kinds
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    CreateBranch,
    Search,
}

/// Confirmation action kinds
#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteBranch(String),
    Merge(String),
    Rebase(String),
}

/// Result of async diff computation
struct DiffResult {
    oid: Oid,
    diff: Result<CommitDiffInfo, String>,
}

struct ModalDiffResult {
    key: (DiffTarget, std::path::PathBuf),
    value: Result<(String, String), String>,
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
const DIFF_LOAD_DEBOUNCE: Duration = Duration::from_millis(50);

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
    pub files_pane: FilesPane,

    // Branch selection state
    /// List of (node_index, branch_name) for all branches
    pub branch_positions: Vec<(usize, String)>,
    /// Currently selected branch position index
    pub selected_branch_position: Option<usize>,

    // Search state
    search_state: SearchState,

    // Latest working tree status snapshot
    working_tree_status: Option<WorkingTreeStatus>,

    // Diff cache (async load)
    diff_cache: Option<CommitDiffInfo>,
    diff_cache_oid: Option<Oid>,
    diff_loading_oid: Option<Oid>,
    diff_receiver: Option<Receiver<DiffResult>>,

    // Fast commit file list cache (for instant rendering)
    commit_files_cache_oid: Option<Oid>,
    commit_files_cache: Option<Vec<(crate::git::FileChangeKind, PathBuf)>>,

    // Uncommitted diff cache
    uncommitted_diff_cache: Option<CommitDiffInfo>,
    uncommitted_diff_failed: bool,
    uncommitted_diff_loading: bool,
    uncommitted_diff_receiver: Option<Receiver<UncommittedDiffResult>>,
    /// Cache key: working tree status at the time of caching (for invalidation)
    uncommitted_cache_key: Option<WorkingTreeStatus>,
    selected_diff_target: Option<DiffTarget>,
    selected_diff_target_changed_at: Instant,

    // Diff modal cache (to avoid re-running external diff renderer)
    diff_modal_cache_key: Option<(DiffTarget, std::path::PathBuf)>,
    diff_modal_cache_value: Option<(String, String)>,
    diff_modal_receiver: Option<Receiver<ModalDiffResult>>,

    // Flags
    pub should_quit: bool,

    // Debug: key event inspector
    key_debug_enabled: bool,
    last_key_debug: Option<String>,

    // Status message with auto-clear
    message: Option<String>,
    message_time: Option<std::time::Instant>,

    // Commit message editor (used when the uncommitted node is selected)
    pub commit_message: String,
    pub commit_message_cursor: usize,
    pub commit_message_editing: bool,
    commit_message_selection_anchor: Option<usize>,

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
    pub fn is_key_debug_enabled(&self) -> bool {
        self.key_debug_enabled
    }

    pub fn last_key_debug(&self) -> Option<&str> {
        self.last_key_debug.as_deref()
    }

    pub fn debug_commit_cursor(&self) -> String {
        let msg_chars: Vec<char> = self.commit_message.chars().collect();
        let cursor = self.commit_message_cursor.min(msg_chars.len());

        let prev = cursor
            .checked_sub(1)
            .and_then(|i| msg_chars.get(i).copied());
        let cur = msg_chars.get(cursor).copied();

        format!(
            "editing={} cursor={}/{} prev={:?} cur={:?}",
            self.commit_message_editing,
            cursor,
            msg_chars.len(),
            prev,
            cur
        )
    }

    pub fn record_key_debug(&mut self, key: &crossterm::event::KeyEvent) {
        if !self.key_debug_enabled {
            return;
        }
        self.last_key_debug = Some(format!("code={:?} mods={:?}", key.code, key.modifiers));
    }

    pub fn cached_commit_files(&mut self) -> Option<&[(crate::git::FileChangeKind, PathBuf)]> {
        let DiffTarget::Commit(oid) = self.current_diff_target()? else {
            return None;
        };

        if self.commit_files_cache_oid == Some(oid) {
            return self.commit_files_cache.as_deref();
        }

        let output = Command::new("git")
            .args(["-C", self.repo_path.as_str()])
            .args(["diff-tree", "--no-commit-id", "--name-status", "-r"])
            .arg(oid.to_string())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut entries: Vec<(crate::git::FileChangeKind, PathBuf)> = Vec::new();
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Format examples:
            // M\tpath
            // A\tpath
            // D\tpath
            // R100\told\tnew
            // C100\told\tnew
            let mut parts = line.split('\t');
            let Some(status) = parts.next() else {
                continue;
            };
            let status_char = status.chars().next().unwrap_or('?');
            let kind = match status_char {
                'A' => crate::git::FileChangeKind::Added,
                'D' => crate::git::FileChangeKind::Deleted,
                'R' => crate::git::FileChangeKind::Renamed,
                'C' => crate::git::FileChangeKind::Copied,
                _ => crate::git::FileChangeKind::Modified,
            };

            // For rename/copy we want to display the destination path.
            let path = match kind {
                crate::git::FileChangeKind::Renamed | crate::git::FileChangeKind::Copied => {
                    // status, old, new
                    let _old = parts.next();
                    parts.next().unwrap_or("")
                }
                _ => parts.next().unwrap_or(""),
            };
            if path.is_empty() {
                continue;
            }
            entries.push((kind, PathBuf::from(path)));
        }
        entries.sort_by(|a, b| a.1.cmp(&b.1));
        entries.dedup_by(|a, b| a.1 == b.1);

        self.commit_files_cache_oid = Some(oid);
        self.commit_files_cache = Some(entries);
        self.commit_files_cache.as_deref()
    }

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

        let files_pane = FilesPane::default();

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
            mode: AppMode::Graph,
            repo,
            repo_path,
            head_name,
            commits,
            branches,
            graph_layout,
            graph_list_state,
            files_pane,
            branch_positions,
            selected_branch_position,
            search_state: SearchState::default(),
            working_tree_status,
            diff_cache: None,
            diff_cache_oid: None,
            diff_loading_oid: None,
            diff_receiver: None,
            commit_files_cache_oid: None,
            commit_files_cache: None,
            uncommitted_diff_cache: None,
            uncommitted_diff_failed: false,
            uncommitted_diff_loading: false,
            uncommitted_diff_receiver: None,
            uncommitted_cache_key: None,
            selected_diff_target: None,
            selected_diff_target_changed_at: now,
            diff_modal_cache_key: None,
            diff_modal_cache_value: None,
            diff_modal_receiver: None,
            should_quit: false,
            key_debug_enabled: false,
            last_key_debug: None,
            message: initial_message,
            message_time: initial_message_time,
            commit_message: String::new(),
            commit_message_cursor: 0,
            commit_message_editing: false,
            commit_message_selection_anchor: None,
            fetch_receiver: None,
            fetch_silent: false,
            config,
            last_refresh_time: now,
            last_fetch_time: now,
        })
    }

    /// Clear all diff caches
    fn clear_all_diff_caches(&mut self) {
        self.diff_cache = None;
        self.diff_cache_oid = None;
        self.diff_loading_oid = None;
        self.diff_receiver = None;
        self.commit_files_cache_oid = None;
        self.commit_files_cache = None;
        self.clear_uncommitted_diff_cache();
        self.diff_modal_cache_key = None;
        self.diff_modal_cache_value = None;
        self.diff_modal_receiver = None;
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
        let head_commit_oid = self.repo.head_oid();
        self.graph_layout = build_graph(
            &self.commits,
            &self.branches,
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
                self.commit_files_cache_oid = None;
                self.commit_files_cache = None;
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
                match self.refresh(true) {
                    Ok(()) => self.set_message("Fetched from origin"),
                    Err(e) => self.show_error(format!("Refresh failed: {e}")),
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

    pub fn poll_modal_diff_results(&mut self) {
        let Some(rx) = self.diff_modal_receiver.as_ref() else {
            return;
        };

        let mut last_error: Option<String> = None;
        while let Ok(result) = rx.try_recv() {
            match result.value {
                Ok(value) => {
                    self.diff_modal_cache_key = Some(result.key);
                    self.diff_modal_cache_value = Some(value);
                }
                Err(e) => {
                    last_error = Some(e);
                }
            }
        }

        if let Some(e) = last_error {
            self.set_message(format!("Failed to render diff: {e}"));
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

    pub fn working_tree_status(&self) -> Option<&WorkingTreeStatus> {
        self.working_tree_status.as_ref()
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

    /// Handle an action
    pub fn handle_action(&mut self, action: Action) -> Result<()> {
        match &self.mode {
            AppMode::Graph => self.handle_graph_action(action)?,
            AppMode::Files => self.handle_files_action(action)?,
            AppMode::Detail => self.handle_detail_action(action)?,
            AppMode::Modal { .. } => self.handle_modal_action(action),
            AppMode::Help => self.handle_help_action(action),
            AppMode::Input { .. } => self.handle_input_action(action)?,
            AppMode::Confirm { .. } => self.handle_confirm_action(action)?,
            AppMode::Error { .. } => self.handle_error_action(action),
        }
        Ok(())
    }

    /// Show an error
    pub fn show_error(&mut self, message: String) {
        self.mode = AppMode::Error { message };
    }

    fn handle_graph_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::ToggleKeyDebug => {
                self.key_debug_enabled = !self.key_debug_enabled;
                if !self.key_debug_enabled {
                    self.last_key_debug = None;
                }
            }
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
            Action::FocusLeftPane => {
                // No-op: graph is the leftmost panel.
            }
            Action::FocusRightPane => {
                self.mode = AppMode::Files;
                // Keep existing selection if present.
                if self.files_pane.list_state.selected().is_none() {
                    self.files_pane.list_state.select(Some(0));
                }
            }
            Action::FocusDownPane => {
                self.mode = AppMode::Files;
                if self.files_pane.list_state.selected().is_none() {
                    self.files_pane.list_state.select(Some(0));
                }
            }
            Action::Refresh => {
                self.refresh(true)?;
                self.reset_timers();
            }
            Action::Fetch => {
                if !self.is_fetching() {
                    self.start_fetch(true, false); // silent=false for manual fetch
                }
            }
            Action::Checkout => {
                self.do_checkout()?;
            }
            Action::FocusFiles => {
                self.enter_files_mode();
            }
            Action::CreateBranch => {
                self.mode = AppMode::Input {
                    title: "New Branch Name".to_string(),
                    input: String::new(),
                    action: InputAction::CreateBranch,
                };
            }
            Action::Search => {
                // Save position for cancel restoration
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
            Action::Merge => {
                if let Some(branch) = self.selected_branch() {
                    if !branch.is_head {
                        self.mode = AppMode::Confirm {
                            message: format!("Merge '{}' into current branch?", branch.name),
                            action: ConfirmAction::Merge(branch.name.clone()),
                        };
                    }
                }
            }
            Action::Rebase => {
                if let Some(branch) = self.selected_branch() {
                    if !branch.is_head {
                        self.mode = AppMode::Confirm {
                            message: format!("Rebase current branch onto '{}'?", branch.name),
                            action: ConfirmAction::Rebase(branch.name.clone()),
                        };
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_files_action(&mut self, action: Action) -> Result<()> {
        let file_count = if let Some(diff) = self.cached_diff() {
            diff.files.len()
        } else if let Some(paths) = self.cached_commit_files() {
            paths.len()
        } else {
            self.working_tree_status
                .as_ref()
                .map(|s| s.file_paths.len())
                .unwrap_or(0)
        };
        match action {
            Action::ToggleKeyDebug => {
                self.key_debug_enabled = !self.key_debug_enabled;
                if !self.key_debug_enabled {
                    self.last_key_debug = None;
                }
            }
            Action::Quit => {
                self.files_pane.exit(&mut self.mode);
            }
            Action::FocusLeftPane => {
                self.mode = AppMode::Graph;
                self.files_pane.list_state.select(None);
            }
            Action::FocusRightPane => {
                self.mode = AppMode::Detail;
            }
            Action::MoveUp => {
                self.files_pane.move_selection(file_count, -1);
            }
            Action::MoveDown => {
                self.files_pane.move_selection(file_count, 1);
            }
            Action::PageUp => {
                self.files_pane.move_selection(file_count, -10);
            }
            Action::PageDown => {
                self.files_pane.move_selection(file_count, 10);
            }
            Action::FilesSelect => {
                self.files_pane.select_current();
                self.files_pane.exit(&mut self.mode);
            }
            Action::FilesOpenModal => {
                self.files_pane.select_current();
                let (title, message) = self.build_selected_file_diff_preview()?;
                self.mode = AppMode::Modal {
                    title,
                    message,
                    scroll: 0,
                };
            }
            Action::ToggleStage => {
                self.toggle_stage_selected_file()?;
            }
            _ => {}
        }
        Ok(())
    }

    fn toggle_stage_selected_file(&mut self) -> Result<()> {
        let Some(target) = self.current_diff_target() else {
            return Ok(());
        };
        if !matches!(target, DiffTarget::Uncommitted) {
            self.set_message("Staging only available for uncommitted files".to_string());
            return Ok(());
        }

        let Some(diff) = self.cached_diff() else {
            return Ok(());
        };
        let Some(file_idx) = self.files_pane.list_state.selected() else {
            return Ok(());
        };
        let Some(file) = diff.files.get(file_idx) else {
            return Ok(());
        };
        let file_path = file.path.clone();
        let was_staged = file.is_staged;

        if was_staged {
            crate::git::operations::unstage_path(&self.repo.repo, &file_path)?;
        } else {
            crate::git::operations::stage_path(&self.repo.repo, &file_path)?;
        }

        // Refresh and keep selection on the same path if it still exists.
        self.refresh(true)?;
        if let Some(new_diff) = self.cached_diff() {
            if let Some(new_idx) = new_diff.files.iter().position(|f| f.path == file_path) {
                self.files_pane.list_state.select(Some(new_idx));
            }
        }

        Ok(())
    }

    fn handle_detail_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::ToggleKeyDebug => {
                self.key_debug_enabled = !self.key_debug_enabled;
                if !self.key_debug_enabled {
                    self.last_key_debug = None;
                }
            }
            Action::Quit => {
                self.mode = AppMode::Graph;
            }
            Action::FocusLeftPane => {
                if self.commit_message_editing {
                    // When editing commit message, Left/Right navigates the cursor.
                    self.clear_commit_message_selection();
                    self.move_commit_message_left();
                } else {
                    self.mode = AppMode::Files;
                    self.clear_commit_message_selection();
                    if self.files_pane.list_state.selected().is_none() {
                        self.files_pane.list_state.select(Some(0));
                    }
                }
            }
            Action::FocusRightPane => {
                if self.commit_message_editing {
                    // When editing commit message, Left/Right navigates the cursor.
                    self.clear_commit_message_selection();
                    self.move_commit_message_right();
                } else {
                    // No-op: detail is the rightmost panel.
                }
            }
            Action::InputChar(c) => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.replace_commit_message_selection_if_any();
                    self.insert_commit_message_char(c);
                }
            }
            Action::InputBackspace => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    if !self.delete_commit_message_selection_if_any() {
                        self.delete_commit_message_backspace();
                    }
                }
            }
            Action::CommitMessageDeleteForward => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    if !self.delete_commit_message_selection_if_any() {
                        self.delete_commit_message_forward();
                    }
                }
            }
            Action::CommitMessageInsertNewline => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.replace_commit_message_selection_if_any();
                    self.insert_commit_message_char('\n');
                }
            }
            Action::CommitMessageToggleEdit => {
                if self.is_uncommitted_selected() {
                    if self.commit_message_editing {
                        // Enter while editing inserts a newline.
                        self.replace_commit_message_selection_if_any();
                        self.insert_commit_message_char('\n');
                    } else {
                        self.commit_message_editing = true;
                    }
                }
            }
            Action::CommitMessageStopEdit => {
                if self.commit_message_editing {
                    self.commit_message_editing = false;
                    self.clear_commit_message_selection();
                }
            }
            Action::CommitMessageMoveLeft => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.clear_commit_message_selection();
                    self.move_commit_message_left();
                }
            }
            Action::CommitMessageMoveRight => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.clear_commit_message_selection();
                    self.move_commit_message_right();
                }
            }
            Action::CommitMessageMoveHome => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.clear_commit_message_selection();
                    self.move_commit_message_home();
                }
            }
            Action::CommitMessageMoveEnd => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.clear_commit_message_selection();
                    self.move_commit_message_end();
                }
            }
            Action::CommitMessageMoveStart => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.clear_commit_message_selection();
                    self.commit_message_cursor = 0;
                }
            }
            Action::CommitMessageMoveFinish => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.clear_commit_message_selection();
                    self.commit_message_cursor = self.commit_message.chars().count();
                }
            }
            Action::CommitMessageSelectLeft => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.extend_selection();
                    self.move_commit_message_left();
                }
            }
            Action::CommitMessageSelectRight => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.extend_selection();
                    self.move_commit_message_right();
                }
            }
            Action::CommitMessageSelectHome => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.extend_selection();
                    self.move_commit_message_home();
                }
            }
            Action::CommitMessageSelectEnd => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.extend_selection();
                    self.move_commit_message_end();
                }
            }
            Action::CommitMessageSelectStart => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.extend_selection();
                    self.commit_message_cursor = 0;
                }
            }
            Action::CommitMessageSelectFinish => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.extend_selection();
                    self.commit_message_cursor = self.commit_message.chars().count();
                }
            }
            Action::CommitMessageMoveWordLeft => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.clear_commit_message_selection();
                    self.move_commit_message_word_left();
                }
            }
            Action::CommitMessageMoveWordRight => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    self.clear_commit_message_selection();
                    self.move_commit_message_word_right();
                }
            }
            Action::CommitMessageDeleteWordBack => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    if !self.delete_commit_message_selection_if_any() {
                        self.delete_commit_message_word_back();
                    }
                }
            }
            Action::CommitMessageDeleteWordForward => {
                if self.is_uncommitted_selected() && self.commit_message_editing {
                    if !self.delete_commit_message_selection_if_any() {
                        self.delete_commit_message_word_forward();
                    }
                }
            }
            Action::CommitMessageCommit => {
                if self.is_uncommitted_selected() {
                    commit_with_message(&self.repo_path, &self.commit_message)?;
                    self.commit_message.clear();
                    self.commit_message_cursor = 0;
                    self.commit_message_selection_anchor = None;
                    self.commit_message_editing = false;
                    self.refresh(true)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn insert_commit_message_char(&mut self, c: char) {
        let mut chars: Vec<char> = self.commit_message.chars().collect();
        let cursor = self.commit_message_cursor.min(chars.len());
        chars.insert(cursor, c);
        self.commit_message = chars.into_iter().collect();
        self.commit_message_cursor = cursor + 1;
    }

    fn clear_commit_message_selection(&mut self) {
        self.commit_message_selection_anchor = None;
    }

    fn extend_selection(&mut self) {
        if self.commit_message_selection_anchor.is_none() {
            self.commit_message_selection_anchor = Some(self.commit_message_cursor);
        }
    }

    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.commit_message_selection_anchor?;
        let a = anchor.min(self.commit_message_cursor);
        let b = anchor.max(self.commit_message_cursor);
        if a == b {
            None
        } else {
            Some((a, b))
        }
    }

    fn delete_commit_message_selection_if_any(&mut self) -> bool {
        let Some((start, end)) = self.selection_range() else {
            return false;
        };
        let mut chars: Vec<char> = self.commit_message.chars().collect();
        let end = end.min(chars.len());
        let start = start.min(end);
        chars.drain(start..end);
        self.commit_message = chars.into_iter().collect();
        self.commit_message_cursor = start;
        self.commit_message_selection_anchor = None;
        true
    }

    fn replace_commit_message_selection_if_any(&mut self) {
        let _ = self.delete_commit_message_selection_if_any();
    }

    fn move_commit_message_left(&mut self) {
        if self.commit_message_cursor > 0 {
            self.commit_message_cursor -= 1;
        }
    }

    fn move_commit_message_right(&mut self) {
        let len = self.commit_message.chars().count();
        if self.commit_message_cursor < len {
            self.commit_message_cursor += 1;
        }
    }

    fn move_commit_message_home(&mut self) {
        let chars: Vec<char> = self.commit_message.chars().collect();
        let mut i = self.commit_message_cursor.min(chars.len());
        while i > 0 {
            if chars[i - 1] == '\n' {
                break;
            }
            i -= 1;
        }
        self.commit_message_cursor = i;
    }

    fn move_commit_message_end(&mut self) {
        let chars: Vec<char> = self.commit_message.chars().collect();
        let mut i = self.commit_message_cursor.min(chars.len());
        while i < chars.len() {
            if chars[i] == '\n' {
                break;
            }
            i += 1;
        }
        self.commit_message_cursor = i;
    }

    fn delete_commit_message_backspace(&mut self) {
        if self.commit_message.is_empty() || self.commit_message_cursor == 0 {
            return;
        }
        let mut chars: Vec<char> = self.commit_message.chars().collect();
        let cursor = self.commit_message_cursor.min(chars.len());
        chars.remove(cursor - 1);
        self.commit_message = chars.into_iter().collect();
        self.commit_message_cursor = cursor - 1;
    }

    fn delete_commit_message_forward(&mut self) {
        let mut chars: Vec<char> = self.commit_message.chars().collect();
        let cursor = self.commit_message_cursor.min(chars.len());
        if cursor >= chars.len() {
            return;
        }
        chars.remove(cursor);
        self.commit_message = chars.into_iter().collect();
        self.commit_message_cursor = cursor;
    }

    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    fn move_commit_message_word_left(&mut self) {
        let chars: Vec<char> = self.commit_message.chars().collect();
        let mut i = self.commit_message_cursor.min(chars.len());

        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && Self::is_word_char(chars[i - 1]) {
            i -= 1;
        }
        self.commit_message_cursor = i;
    }

    fn move_commit_message_word_right(&mut self) {
        let chars: Vec<char> = self.commit_message.chars().collect();
        let mut i = self.commit_message_cursor.min(chars.len());

        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && Self::is_word_char(chars[i]) {
            i += 1;
        }
        self.commit_message_cursor = i;
    }

    fn delete_commit_message_word_back(&mut self) {
        if self.commit_message_cursor == 0 {
            return;
        }
        let mut chars: Vec<char> = self.commit_message.chars().collect();
        let mut start = self.commit_message_cursor.min(chars.len());

        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && Self::is_word_char(chars[start - 1]) {
            start -= 1;
        }
        let end = self.commit_message_cursor.min(chars.len());
        chars.drain(start..end);
        self.commit_message = chars.into_iter().collect();
        self.commit_message_cursor = start;
    }

    fn delete_commit_message_word_forward(&mut self) {
        let mut chars: Vec<char> = self.commit_message.chars().collect();
        let start = self.commit_message_cursor.min(chars.len());
        let mut end = start;

        while end < chars.len() && chars[end].is_whitespace() {
            end += 1;
        }
        while end < chars.len() && Self::is_word_char(chars[end]) {
            end += 1;
        }
        if end == start {
            return;
        }
        chars.drain(start..end);
        self.commit_message = chars.into_iter().collect();
        self.commit_message_cursor = start;
    }

    fn is_uncommitted_selected(&self) -> bool {
        self.graph_list_state
            .selected()
            .and_then(|idx| self.graph_layout.nodes.get(idx))
            .is_some_and(|node| node.is_uncommitted)
    }

    fn build_selected_file_diff_preview(&mut self) -> Result<(String, String)> {
        let file_path = {
            let Some(diff) = self.cached_diff() else {
                return Ok(("Diff".to_string(), "Diff not loaded yet".to_string()));
            };
            let Some(file_idx) = self.files_pane.selected_file_index else {
                return Ok(("Diff".to_string(), "No file selected".to_string()));
            };
            let Some(file) = diff.files.get(file_idx) else {
                return Ok(("Diff".to_string(), "No file selected".to_string()));
            };
            file.path.clone()
        };

        let Some(selected_node) = self.graph_list_state.selected() else {
            return Ok((
                file_path.to_string_lossy().to_string(),
                "No commit selected".to_string(),
            ));
        };
        let (is_uncommitted, commit_oid) = self
            .graph_layout
            .nodes
            .get(selected_node)
            .map(|n| (n.is_uncommitted, n.commit.as_ref().map(|c| c.oid)))
            .unwrap_or((false, None));

        if !is_uncommitted && commit_oid.is_none() {
            return Ok((
                file_path.to_string_lossy().to_string(),
                "No commit selected".to_string(),
            ));
        }

        let backend = crate::diff_view::DiffBackend::Difftastic;

        let Some(target) = self.current_diff_target() else {
            return Ok(("Diff".to_string(), "No commit selected".to_string()));
        };
        let cache_key = (target, file_path.clone());
        if self.diff_modal_cache_key.as_ref() == Some(&cache_key) {
            if let Some(value) = self.diff_modal_cache_value.clone() {
                return Ok(value);
            }
        }

        // Opportunistically pick up any background-computed result.
        self.poll_modal_diff_results();
        if self.diff_modal_cache_key.as_ref() == Some(&cache_key) {
            if let Some(value) = self.diff_modal_cache_value.clone() {
                return Ok(value);
            }
        }
        let render = if is_uncommitted {
            crate::diff_view::render_worktree_file_diff(
                &self.repo_path,
                file_path.as_path(),
                backend,
            )?
        } else {
            let Some(commit_oid) = commit_oid else {
                return Ok((
                    file_path.to_string_lossy().to_string(),
                    "No commit selected".to_string(),
                ));
            };
            crate::diff_view::render_commit_file_diff(
                &self.repo_path,
                commit_oid,
                file_path.as_path(),
                backend,
            )?
        };

        let text = render.ansi;

        let value = if text.trim().is_empty() {
            (render.title, "(no diff output)".to_string())
        } else {
            (render.title, text)
        };

        self.diff_modal_cache_key = Some(cache_key);
        self.diff_modal_cache_value = Some(value.clone());
        Ok(value)
    }

    fn handle_modal_action(&mut self, action: Action) {
        match &mut self.mode {
            AppMode::Modal { scroll, .. } => match action {
                Action::Cancel | Action::Confirm | Action::Quit => {
                    self.mode = AppMode::Files;
                }
                Action::ModalScrollUp => {
                    *scroll = scroll.saturating_sub(1);
                }
                Action::ModalScrollDown => {
                    *scroll = scroll.saturating_add(1);
                }
                Action::ModalPageUp => {
                    *scroll = scroll.saturating_sub(10);
                }
                Action::ModalPageDown => {
                    *scroll = scroll.saturating_add(10);
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn enter_files_mode(&mut self) {
        let file_count = self.cached_diff().map(|d| d.files.len()).unwrap_or(0);
        if let Some(msg) = self.files_pane.enter(&mut self.mode, file_count) {
            self.set_message(msg);
            return;
        }

        // Start precomputing the diff for the initially-selected file.
        self.start_modal_diff_precompute();
    }

    fn start_modal_diff_precompute(&mut self) {
        let Some(target) = self.current_diff_target() else {
            return;
        };
        let Some(diff) = self.cached_diff() else {
            return;
        };
        let Some(file_idx) = self.files_pane.list_state.selected() else {
            return;
        };
        let Some(file) = diff.files.get(file_idx) else {
            return;
        };

        let key = (target, file.path.clone());
        if self.diff_modal_cache_key.as_ref() == Some(&key) {
            return;
        }

        let repo_path = self.repo_path.clone();
        let path = file.path.clone();
        let backend = crate::diff_view::DiffBackend::Difftastic;

        let (tx, rx) = mpsc::channel();
        self.diff_modal_receiver = Some(rx);

        thread::spawn(move || {
            let value = (|| -> Result<(String, String)> {
                let render = match target {
                    DiffTarget::Uncommitted => crate::diff_view::render_worktree_file_diff(
                        &repo_path,
                        path.as_path(),
                        backend,
                    )?,
                    DiffTarget::Commit(oid) => crate::diff_view::render_commit_file_diff(
                        &repo_path,
                        oid,
                        path.as_path(),
                        backend,
                    )?,
                };
                if render.ansi.trim().is_empty() {
                    Ok((render.title, "(no diff output)".to_string()))
                } else {
                    Ok((render.title, render.ansi))
                }
            })()
            .map_err(|e| e.to_string());

            let _ = tx.send(ModalDiffResult { key, value });
        });
    }

    fn handle_help_action(&mut self, action: Action) {
        if matches!(action, Action::ToggleHelp | Action::Quit | Action::Cancel) {
            self.mode = AppMode::Graph;
        }
    }

    fn handle_error_action(&mut self, action: Action) {
        // Close the error on any key
        if matches!(action, Action::Quit | Action::Cancel | Action::Confirm) {
            self.mode = AppMode::Graph;
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
                    InputAction::Search => {
                        // Jump to selected result and exit search mode
                        self.jump_to_search_result();
                    }
                }
                // Clear search state after confirming
                self.search_state = SearchState::default();
                self.mode = AppMode::Graph;
            }
            Action::Cancel => {
                // Restore position when canceling search
                if matches!(input_action, InputAction::Search) {
                    self.restore_search_position();
                }
                self.search_state = SearchState::default();
                self.mode = AppMode::Graph;
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
                    self.mode = AppMode::Graph;
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
                }
                self.refresh(true)?;
                self.mode = AppMode::Graph;
            }
            Action::Cancel => {
                self.mode = AppMode::Graph;
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

        let files_pane = FilesPane::default();

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
            mode: AppMode::Graph,
            repo,
            repo_path: String::new(),
            head_name: None,
            commits,
            branches,
            graph_layout,
            graph_list_state,
            files_pane,
            branch_positions,
            selected_branch_position,
            search_state: SearchState::default(),
            working_tree_status,
            diff_cache: None,
            diff_cache_oid: None,
            diff_loading_oid: None,
            diff_receiver: None,
            commit_files_cache_oid: None,
            commit_files_cache: None,
            uncommitted_diff_cache: None,
            uncommitted_diff_failed: false,
            uncommitted_diff_loading: false,
            uncommitted_diff_receiver: None,
            uncommitted_cache_key: None,
            selected_diff_target: None,
            selected_diff_target_changed_at: now,
            diff_modal_cache_key: None,
            diff_modal_cache_value: None,
            diff_modal_receiver: None,
            should_quit: false,
            key_debug_enabled: false,
            last_key_debug: None,
            message: initial_message,
            message_time: initial_message_time,
            commit_message: String::new(),
            commit_message_cursor: 0,
            commit_message_selection_anchor: None,
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

        let files_pane = FilesPane::default();

        let commits = node.commit.iter().cloned().collect();

        App {
            mode: AppMode::Graph,
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
            files_pane,
            branch_positions: Vec::new(),
            selected_branch_position: None,
            search_state: SearchState::default(),
            working_tree_status,
            diff_cache: None,
            diff_cache_oid: None,
            diff_loading_oid: None,
            diff_receiver: None,
            commit_files_cache_oid: None,
            commit_files_cache: None,
            uncommitted_diff_cache: None,
            uncommitted_diff_failed: false,
            uncommitted_diff_loading: false,
            uncommitted_diff_receiver: None,
            uncommitted_cache_key: None,
            selected_diff_target: Some(diff_target),
            selected_diff_target_changed_at: Instant::now() - DIFF_LOAD_DEBOUNCE,
            diff_modal_cache_key: None,
            diff_modal_cache_value: None,
            diff_modal_receiver: None,
            should_quit: false,
            key_debug_enabled: false,
            last_key_debug: None,
            message: None,
            message_time: None,
            commit_message: String::new(),
            commit_message_cursor: 0,
            commit_message_selection_anchor: None,
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
