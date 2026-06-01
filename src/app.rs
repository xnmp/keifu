//! Application state management

use std::time::Instant;

use anyhow::Result;

use git2::Oid;

use crate::{
    action::Action,
    config::{Config, UiState},
    diff_cache::{DiffCache, DiffTarget},
    files_pane_state::{FilesPaneState, section_of},
    graph_nav::GraphNav,
    network::NetworkManager,
    git::{
        build_graph,
        graph::GraphLayout,
        operations::{
            add_tag, cherry_pick, checkout_branch, checkout_commit, checkout_remote_branch,
            commit_amend, commit_amend_no_edit, commit_with_message, create_branch,
            delete_branch, get_last_commit_message, merge_branch,
            rebase_branch, reset_to_commit, restore_files, revert_commit, stage_file,
            unstage_file, ResetMode,
        },
        BranchInfo, CommitDiffInfo, CommitInfo, FileChangeKind, FileDiffContent, FileDiffInfo,
        GitRepository, StageStatus, WorkingTreeStatus,
    },
    search::{fuzzy_search_branches, FuzzySearchResult},
    workspace::{add_to_gitignore, archive_path, remove_from_gitignore, unarchive_path},
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

// Re-export FilesPaneItem so external code can use `crate::app::FilesPaneItem`
pub use crate::files_pane_state::FilesPaneItem;

/// An operation that can be undone with Ctrl+Z
#[derive(Debug, Clone)]
pub enum UndoableOperation {
    Stage { path: String, was_staged: bool },
    Gitignore { pattern: String },
    Archive { relative_path: String },
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
    BranchPicker {
        branches: Vec<String>,
        selected: usize,
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
    TrashFile(Vec<String>),
    RestoreFile(Vec<String>),
}


/// Search state for branch search feature
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub fuzzy_matches: Vec<FuzzySearchResult>,
    pub dropdown_selection: Option<usize>,
    pub original_position: Option<usize>,
    pub original_node: Option<usize>,
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
    pub head_detached: bool,

    // Data
    pub commits: Vec<CommitInfo>,
    pub branches: Vec<BranchInfo>,
    pub graph_layout: GraphLayout,

    // UI state
    pub graph_nav: GraphNav,
    pub focused_panel: FocusedPanel,
    /// Files pane subsystem state
    pub files_pane: FilesPaneState,
    pub hidden_branches: std::collections::HashSet<String>,
    pub commit_editor: crate::text_editor::TextEditor,
    pub editing_commit_message: bool,
    /// When true, the editor is amending the HEAD commit (not creating a new one)
    pub amending_commit: bool,
    pub commit_detail_scroll: u16,
    pub commit_detail_max_scroll: u16,
    /// Number of lines before the editor text in the commit detail pane
    pub commit_editor_line_offset: u16,
    /// Visible rows in the commit detail pane (updated during render)
    pub commit_detail_visible_rows: u16,

    // Search state
    pub search_state: SearchState,

    // Latest working tree status snapshot
    pub working_tree_status: Option<WorkingTreeStatus>,

    // Diff caching subsystem
    pub diff_cache: DiffCache,

    // Flags
    pub should_quit: bool,
    pub pending_refresh: bool,
    pub diff_viewport_height: u16,
    pub diff_viewport_width: u16,

    // Status message with auto-clear
    pub message: Option<String>,
    pub message_time: Option<std::time::Instant>,

    // Network operations (fetch/push/auto-refresh)
    pub network: NetworkManager,

    // Undo
    pub last_undoable_op: Option<UndoableOperation>,

    // Layout
    pub side_panel_layout: bool,

    // Debug mode
    pub debug_keys: bool,

    // Config
    pub config: Config,
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

    /// Create an App from a given repository (for testing and embedding)
    pub fn from_repo(repo: GitRepository) -> Result<Self> {
        let config = Config::default();
        let now = Instant::now();
        let repo_path = repo.path.clone();
        let head_name = repo.head_name();
        let head_detached = repo.is_head_detached();

        let commits = repo.get_commits(500)?;
        let branches = repo.get_branches()?;
        let (working_tree_status, initial_message) = Self::working_tree_status_snapshot(&repo);
        let initial_message_time = initial_message.as_ref().map(|_| now);
        let uncommitted_count = working_tree_status
            .as_ref()
            .map(|s| s.accurate_file_count());
        let head_commit_oid = repo.head_oid();
        let graph_layout = build_graph(&commits, &branches, uncommitted_count, head_commit_oid);

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
            graph_layout,
            graph_nav,
            focused_panel: FocusedPanel::Graph,
            files_pane: FilesPaneState::new(),
            hidden_branches: std::collections::HashSet::new(),
            commit_editor: crate::text_editor::TextEditor::new(),
            editing_commit_message: false,
            amending_commit: false,
            commit_detail_scroll: 0,
            commit_detail_max_scroll: 0,
            commit_editor_line_offset: 0,
            commit_detail_visible_rows: 20,
            search_state: SearchState::default(),
            working_tree_status,
            diff_cache: DiffCache::new(),
            should_quit: false,
            pending_refresh: false,
            diff_viewport_height: 40,
            diff_viewport_width: 80,
            message: initial_message,
            message_time: initial_message_time,
            network: NetworkManager::new(),
            last_undoable_op: None,
            side_panel_layout: false,
            debug_keys: false,
            config,
        })
    }

    /// Create a new application
    pub fn new() -> Result<Self> {
        let config = Config::load();
        let ui_state = UiState::load();
        let now = Instant::now();

        let repo = GitRepository::discover()?;
        let repo_path = repo.path.clone();
        let head_name = repo.head_name();
        let head_detached = repo.is_head_detached();

        let commits = repo.get_commits(500)?;
        let branches = repo.get_branches()?;
        let (working_tree_status, initial_message) = Self::working_tree_status_snapshot(&repo);
        let initial_message_time = initial_message.as_ref().map(|_| now);
        let uncommitted_count = working_tree_status
            .as_ref()
            .map(|s| s.accurate_file_count());
        let head_commit_oid = repo.head_oid();
        let graph_layout = build_graph(&commits, &branches, uncommitted_count, head_commit_oid);

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
            graph_layout,
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
            search_state: SearchState::default(),
            working_tree_status,
            diff_cache: DiffCache::new(),
            should_quit: false,
            pending_refresh: false,
            diff_viewport_height: 40,
            diff_viewport_width: 80,
            message: initial_message,
            message_time: initial_message_time,
            network: NetworkManager::new(),
            last_undoable_op: None,
            side_panel_layout: ui_state.side_panel_layout,
            debug_keys: false,
            config,
        })
    }

    /// Refresh after a file operation (stage/unstage/gitignore/archive/trash).
    ///
    /// After the file list reshuffles, selects the next file in the same
    /// section. Falls back to previous in section, then to any file.
    pub fn refresh_after_file_op(&mut self) -> Result<()> {
        // Snapshot the current items and selection BEFORE the refresh.
        // Use files_pane_items() directly — the cache might be stale.
        let old_items = self.files_pane_items();
        let old_idx = self.files_pane.file_selected_index_in(&old_items);
        let old_section = section_of(&old_items, old_idx);

        // Next files in same section (forward until next header)
        let next_in_section: Vec<std::path::PathBuf> = old_items[old_idx + 1..]
            .iter()
            .take_while(|item| !matches!(item, FilesPaneItem::Header(_)))
            .filter_map(|item| match item {
                FilesPaneItem::File(f) => Some(f.path.clone()),
                _ => None,
            })
            .collect();

        // Previous files in same section (backward until header)
        let prev_in_section: Vec<std::path::PathBuf> = old_items[..old_idx]
            .iter()
            .rev()
            .take_while(|item| !matches!(item, FilesPaneItem::Header(_)))
            .filter_map(|item| match item {
                FilesPaneItem::File(f) => Some(f.path.clone()),
                _ => None,
            })
            .collect();

        // Invalidate and clear the stale full diff so the fresh quick diff
        // takes precedence via cached_diff_or_quick().
        self.diff_cache.invalidate_uncommitted();
        self.diff_cache.clear_uncommitted_data();
        self.refresh(false)?;
        // Force quick diff recomputation so file list updates immediately
        self.diff_cache.set_quick_uncommitted(self.repo.repo());
        self.sync_file_list_cache();

        // Find best target: next in same section, then prev, then any file
        let new_items = self.files_pane.display_items();
        let target = next_in_section
            .iter()
            .chain(prev_in_section.iter())
            .find_map(|path| {
                let i = new_items.iter().position(
                    |item| matches!(item, FilesPaneItem::File(f) if f.path == *path),
                )?;
                if section_of(new_items, i) == old_section {
                    Some((path.clone(), old_section.map(|s| s.to_string())))
                } else {
                    None
                }
            });

        if let Some((path, sec)) = target {
            self.files_pane.set_selection(Some(path), sec);
        }
        // Otherwise file_selection keeps its current path; resolve() will
        // find it in whatever section it landed in, or fall back to first file.

        Ok(())
    }

    fn current_diff_target(&self) -> Option<DiffTarget> {
        let node = self
            .graph_nav.graph_list_state
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
        self.diff_cache.sync_selected_target(target, self.repo.repo())
    }

    /// Refresh repository data
    /// If `force` is true, always clears diff cache (for manual refresh)
    /// If `force` is false, keeps cache when the same content is selected (for auto-refresh)
    pub fn refresh(&mut self, force: bool) -> Result<()> {
        // Save the current selection state for restoration
        let was_uncommitted_selected = self
            .graph_nav.graph_list_state
            .selected()
            .and_then(|idx| self.graph_layout.nodes.get(idx))
            .is_some_and(|node| node.is_uncommitted);
        let prev_selected_commit_oid = self
            .graph_nav.graph_list_state
            .selected()
            .and_then(|idx| self.graph_layout.nodes.get(idx))
            .and_then(|node| node.commit.as_ref())
            .map(|commit| commit.oid);

        let prev_branch_name = self
            .graph_nav.selected_branch_position
            .and_then(|pos| self.graph_nav.branch_positions.get(pos))
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
        self.head_detached = self.repo.is_head_detached();

        // Rebuild branch positions
        self.graph_nav.rebuild_branch_positions(&self.graph_layout);

        // Restore selection state
        // Check if uncommitted node still exists in the new graph
        let has_uncommitted_node = self
            .graph_layout
            .nodes
            .first()
            .is_some_and(|node| node.is_uncommitted);

        if was_uncommitted_selected && has_uncommitted_node {
            // Restore uncommitted node selection
            self.graph_nav.graph_list_state.select(Some(0));
            self.graph_nav.selected_branch_position = None;
        } else {
            // Restore branch selection if the branch still exists
            self.graph_nav.selected_branch_position = prev_branch_name
                .and_then(|name| self.graph_nav.branch_positions.iter().position(|(_, n)| n == &name));

            // Sync node selection with branch selection
            if let Some(pos) = self.graph_nav.selected_branch_position {
                if let Some((node_idx, _)) = self.graph_nav.branch_positions.get(pos) {
                    self.graph_nav.graph_list_state.select(Some(*node_idx));
                }
            } else if let Some(oid) = prev_selected_commit_oid {
                let node_idx =
                    self.graph_layout.nodes.iter().position(|node| {
                        node.commit.as_ref().is_some_and(|commit| commit.oid == oid)
                    });
                if let Some(idx) = node_idx {
                    self.graph_nav.graph_list_state.select(Some(idx));
                } else if let Some(prev) = self.graph_nav.graph_list_state.selected() {
                    // OID pushed out of range — keep cursor at the nearest
                    // valid row instead of clearing the selection.
                    let max = self.graph_layout.nodes.len().saturating_sub(1);
                    self.graph_nav.graph_list_state.select(Some(prev.min(max)));
                }
            }
        }

        // If no branch is selected but the selected node has branches, pick the first one.
        // This happens after committing from the uncommitted node — the selection lands
        // on the new HEAD commit but selected_branch_position was never set.
        if self.graph_nav.selected_branch_position.is_none() {
            if let Some(selected_idx) = self.graph_nav.graph_list_state.selected() {
                if let Some(pos) = self
                    .graph_nav.branch_positions
                    .iter()
                    .position(|(node_idx, _)| *node_idx == selected_idx)
                {
                    self.graph_nav.selected_branch_position = Some(pos);
                }
            }
        }

        // Handle diff cache based on force flag
        if force {
            self.diff_cache.clear_all();
        } else {
            // Auto-refresh: smart cache - only clear if selection changed
            let selected_oid = self
                .graph_nav.graph_list_state
                .selected()
                .and_then(|idx| self.graph_layout.nodes.get(idx))
                .and_then(|n| n.commit.as_ref())
                .map(|c| c.oid);

            self.diff_cache.invalidate_for_auto_refresh(
                selected_oid,
                was_uncommitted_selected,
                has_uncommitted_node,
                self.working_tree_status.as_ref(),
            );
        }

        // Clear search state on refresh to avoid stale indices
        // Skip if in search mode to prevent clearing active search results
        if !self.is_in_search_mode() {
            self.search_state = SearchState::default();
        }

        // Clamp the selection
        let max_commit = self.graph_layout.nodes.len().saturating_sub(1);
        if let Some(selected) = self.graph_nav.graph_list_state.selected() {
            if selected > max_commit {
                self.graph_nav.graph_list_state.select(Some(max_commit));
            }
        }

        Ok(())
    }

    /// Update fuzzy search results for the given query
    fn update_fuzzy_search(&mut self, query: &str) {
        self.search_state.fuzzy_matches = fuzzy_search_branches(query, &self.graph_nav.branch_positions);
        self.search_state.clamp_selection();
    }

    /// Jump to the currently selected search result
    fn jump_to_search_result(&mut self) {
        let Some(result) = self.search_state.selected_result() else {
            return;
        };
        let branch_idx = result.branch_idx;
        let Some((node_idx, _)) = self.graph_nav.branch_positions.get(branch_idx) else {
            return;
        };

        self.graph_nav.selected_branch_position = Some(branch_idx);
        self.graph_nav.graph_list_state.select(Some(*node_idx));
    }

    /// Save current position before starting search
    fn save_search_position(&mut self) {
        self.search_state.original_position = self.graph_nav.selected_branch_position;
        self.search_state.original_node = self.graph_nav.graph_list_state.selected();
    }

    /// Restore position saved before search (for cancel)
    fn restore_search_position(&mut self) {
        self.graph_nav.selected_branch_position = self.search_state.original_position;
        if let Some(node) = self.search_state.original_node {
            self.graph_nav.graph_list_state.select(Some(node));
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

    fn jump_to_head(&mut self) {
        self.graph_nav.jump_to_head(self.head_name.as_deref());
    }

    pub fn update_fetch_status(&mut self) {
        let Some(result) = self.network.poll_fetch() else {
            return;
        };
        match result {
            Ok(()) => {
                self.network.reset_timers();
                if matches!(self.mode, AppMode::FileDiff { .. }) {
                    self.pending_refresh = true;
                } else {
                    let prev_head = self.repo.head_oid();
                    let prev_branch_count = self.branches.len();
                    match self.refresh(true) {
                        Ok(()) => {
                            let new_head = self.repo.head_oid();
                            let new_branch_count = self.branches.len();
                            if prev_head != new_head || prev_branch_count != new_branch_count {
                                self.set_message("Fetched from origin");
                            }
                        }
                        Err(e) => self.show_error(format!("Refresh failed: {e}")),
                    }
                }
            }
            Err(e) => self.show_error(e),
        }
    }

    pub fn is_fetching(&self) -> bool {
        self.network.is_fetching()
    }

    pub fn is_pushing(&self) -> bool {
        self.network.is_pushing()
    }

    pub fn is_network_busy(&self) -> bool {
        self.network.is_busy()
    }

    pub fn check_auto_refresh(&mut self) {
        if matches!(self.mode, AppMode::FileDiff { .. }) {
            return;
        }
        let events = self.network.check_auto_timers(&self.config.refresh);
        if events.should_auto_fetch {
            self.start_fetch(false, true);
        } else if events.should_auto_refresh {
            if let Err(e) = self.refresh(false) {
                self.set_message(format!("Auto-refresh failed: {e}"));
            }
            self.network.mark_refreshed();
        }
    }

    fn start_fetch(&mut self, show_message: bool, silent: bool) {
        if let Some(msg) = self.network.start_fetch(&self.repo_path, show_message, silent) {
            self.set_message(msg);
        }
    }

    fn start_push(&mut self) {
        let msg = self.network.start_push(&self.repo_path);
        self.set_message(msg);
    }

    pub fn update_push_status(&mut self) {
        let Some(result) = self.network.poll_push() else {
            return;
        };
        match result {
            Ok(()) => {
                self.set_message("Pushed to origin");
                if let Err(e) = self.refresh(true) {
                    self.show_error(format!("Refresh failed: {e}"));
                }
            }
            Err(e) => self.show_error(e),
        }
    }

    fn reset_timers(&mut self) {
        self.network.reset_timers();
    }

    /// Set a status message (will auto-clear after a few seconds)
    pub fn set_message(&mut self, msg: impl Into<String>) {
        self.message = Some(msg.into());
        self.message_time = Some(std::time::Instant::now());
    }

    /// Get current message if not expired (5 seconds timeout)
    pub fn get_message(&self) -> Option<&str> {
        const MESSAGE_TIMEOUT_SECS: u64 = 5;

        // Don't timeout while a network operation is in progress
        if self.is_network_busy() {
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
        let target = self.sync_selected_diff_target();
        let events = self.diff_cache.poll(
            target,
            &self.repo_path,
            self.working_tree_status.as_ref(),
        );
        if let Some(msg) = events.message {
            self.set_message(msg);
        }
        if events.uncommitted_diff_loaded {
            self.sync_file_list_with_uncommitted_diff();
        }
    }

    /// Get cached diff info for the currently selected node
    pub fn cached_diff(&self) -> Option<&CommitDiffInfo> {
        self.diff_cache.cached_diff(self.current_diff_target())
    }

    /// Get the best available diff: full if cached, otherwise quick file list
    pub fn cached_diff_or_quick(&self) -> Option<&CommitDiffInfo> {
        self.diff_cache.cached_diff_or_quick(self.current_diff_target())
    }

    /// Whether line stats are still loading (full diff not yet available but quick is)
    pub fn is_line_stats_loading(&self) -> bool {
        self.diff_cache.is_line_stats_loading(self.current_diff_target())
    }

    /// Whether diff is loading or pending (debouncing) for the selected node
    pub fn is_diff_loading(&self) -> bool {
        self.diff_cache.is_diff_loading(self.current_diff_target())
    }

    /// Check if the currently selected node is the uncommitted changes node
    /// Return the active theme based on config.
    pub fn theme(&self) -> crate::ui::theme::Theme {
        match self.config.ui.theme.as_str() {
            "light" => crate::ui::theme::Theme::light(),
            "dark" => crate::ui::theme::Theme::dark(),
            _ => {
                // Auto-detect from terminal background
                match terminal_light::luma() {
                    Ok(luma) if luma > 0.5 => crate::ui::theme::Theme::light(),
                    _ => crate::ui::theme::Theme::dark(),
                }
            }
        }
    }

    pub fn is_uncommitted_selected(&self) -> bool {
        self.graph_nav.is_uncommitted_selected(&self.graph_layout)
    }

    pub fn is_head_commit_selected(&self) -> bool {
        self.graph_nav.is_head_commit_selected(&self.graph_layout)
    }

    // ── Files pane delegation ──────────────────────────────────────

    /// Sync the file list cache and display items from the current diff.
    pub fn sync_file_list_cache(&mut self) {
        let diff = self.diff_cache.cached_diff_or_quick(self.current_diff_target());
        let is_uncommitted = self.is_uncommitted_selected();
        self.files_pane.sync_file_list_cache(diff, is_uncommitted, &self.repo_path);
    }

    /// Resolve the current selection to an index in display_items_cache.
    pub fn file_selected_index(&self) -> usize {
        self.files_pane.file_selected_index()
    }

    /// Get the cached display items.
    pub fn display_items(&self) -> &[FilesPaneItem] {
        self.files_pane.display_items()
    }

    /// Build a fresh set of files pane items (not cached).
    pub fn files_pane_items(&self) -> Vec<FilesPaneItem> {
        let diff = self.diff_cache.cached_diff_or_quick(self.current_diff_target());
        let is_uncommitted = self.is_uncommitted_selected();
        self.files_pane.build_files_pane_items(diff, is_uncommitted, &self.repo_path)
    }

    fn select_file_at(&mut self, idx: usize) {
        self.files_pane.select_file_at(idx);
    }

    fn selected_display_item(&self) -> Option<&FilesPaneItem> {
        self.files_pane.selected_display_item()
    }

    fn selected_file(&self) -> Option<&FileDiffInfo> {
        self.files_pane.selected_file()
    }

    fn selected_files(&self) -> Vec<&FileDiffInfo> {
        self.files_pane.selected_files()
    }

    fn display_index_to_flat_index(&self, display_index: usize) -> usize {
        self.files_pane.display_index_to_flat_index(display_index)
    }

    fn flat_index_to_display_index(&self, flat_index: usize) -> usize {
        self.files_pane.flat_index_to_display_index(flat_index)
    }

    fn move_file_selection(&mut self, delta: i32) {
        self.files_pane.move_file_selection(delta);
    }

    fn is_in_archived_section(&self) -> bool {
        self.files_pane.is_in_archived_section()
    }

    /// Handle an action
    pub fn handle_action(&mut self, action: Action) -> Result<()> {
        // Ctrl+Q always quits
        if matches!(action, Action::ForceQuit) {
            self.should_quit = true;
            return Ok(());
        }
        if matches!(action, Action::ToggleLayout) {
            self.side_panel_layout = !self.side_panel_layout;
            UiState {
                side_panel_layout: self.side_panel_layout,
            }
            .save();
            return Ok(());
        }
        if matches!(action, Action::ToggleDebugKeys) {
            self.debug_keys = !self.debug_keys;
            self.set_message(if self.debug_keys {
                "Debug keys ON"
            } else {
                "Debug keys OFF"
            });
            return Ok(());
        }

        match &self.mode {
            AppMode::Normal => self.handle_normal_action(action)?,
            AppMode::Help => self.handle_help_action(action),
            AppMode::Input { .. } => self.handle_input_action(action)?,
            AppMode::Confirm { .. } => self.handle_confirm_action(action)?,
            AppMode::Error { .. } => self.handle_error_action(action),
            AppMode::CommitMenu { .. } => self.handle_commit_menu_action(action)?,
            AppMode::BranchPicker { .. } => self.handle_branch_picker_action(action)?,
            AppMode::BranchFilter { .. } => self.handle_branch_filter_action(action)?,
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
                } else {
                    self.focused_panel = FocusedPanel::Graph;
                    self.files_pane.files_filter.clear();
                    self.files_pane.files_filter_active = false;
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
        // Reset commit detail scroll on any graph navigation
        if matches!(
            action,
            Action::MoveUp
                | Action::MoveDown
                | Action::PageUp
                | Action::PageDown
                | Action::GoToTop
                | Action::GoToBottom
                | Action::JumpToHead
                | Action::NextBranch
                | Action::PrevBranch
        ) {
            self.commit_detail_scroll = 0;
        }
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
            Action::OpenFileDiff => {
                self.sync_file_list_cache();
                if let Some(file) = self.selected_file().cloned() {
                    let file_list = self.files_pane.file_list_cache.clone();
                    let flat_idx = self.display_index_to_flat_index(self.file_selected_index());
                    if let Err(e) = self.enter_file_diff(flat_idx, file_list, &file.path) {
                        self.set_message(format!("Cannot open diff: {e}"));
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
        self.sync_file_list_cache();
        let item_count = self.files_pane.display_items().len();

        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::MoveUp => {
                self.move_file_selection(-1);
            }
            Action::MoveDown => {
                self.move_file_selection(1);
            }
            Action::PageUp => {
                self.move_file_selection(-10);
            }
            Action::PageDown => {
                self.move_file_selection(10);
            }
            Action::GoToTop => {
                self.move_file_selection(-(item_count as i32));
            }
            Action::GoToBottom => {
                self.move_file_selection(item_count as i32);
            }
            Action::OpenFileDiff => {
                if let Some(file) = self.selected_file().cloned() {
                    let file_list = self.files_pane.file_list_cache.clone();
                    let flat_idx = self.display_index_to_flat_index(self.file_selected_index());
                    if let Err(e) = self.enter_file_diff(flat_idx, file_list, &file.path) {
                        self.set_message(format!("Cannot open diff: {e}"));
                    }
                }
            }
            Action::OpenWithDefault => {
                if let Some(file) = self.selected_file() {
                    let path = file.path.clone();
                    let full_path = if self.is_uncommitted_selected() {
                        // Working tree file — open directly
                        std::path::Path::new(&self.repo_path).join(&path)
                    } else if let Some(node) = self.selected_commit_node() {
                        // Committed file — extract blob to temp file
                        if let Some(commit) = &node.commit {
                            match self.extract_blob_to_temp(commit.oid, &path) {
                                Ok(tmp) => tmp,
                                Err(e) => {
                                    self.set_message(format!("Cannot extract file: {e}"));
                                    return Ok(());
                                }
                            }
                        } else {
                            return Ok(());
                        }
                    } else {
                        std::path::Path::new(&self.repo_path).join(&path)
                    };
                    self.open_with_default(&full_path, &path);
                }
            }
            Action::ToggleStage => {
                self.toggle_stage_selected_file()?;
            }
            Action::AddToGitignore => {
                self.add_selected_to_gitignore()?;
            }
            Action::ArchiveFile => {
                if self.is_in_archived_section() {
                    self.unarchive_selected_file()?;
                } else {
                    self.archive_selected_file()?;
                }
            }
            Action::TrashFile => {
                self.trash_selected_file()?;
            }
            Action::RestoreFile => {
                self.restore_selected_file()?;
            }
            Action::UndoLastFileOp => {
                self.undo_last_file_op()?;
            }
            Action::ToggleFolderView => {
                self.files_pane.files_group_by_folder = !self.files_pane.files_group_by_folder;
            }
            Action::StartFilesFilter => {
                self.files_pane.files_filter_active = true;
                self.files_pane.files_filter.clear();
            }
            Action::FilesFilterChar(c) => {
                self.files_pane.files_filter.push(c);
            }
            Action::FilesFilterBackspace => {
                if !self.files_pane.files_filter.is_empty() {
                    self.files_pane.files_filter.pop();
                } else {
                    // Empty filter + backspace exits filter mode
                    self.files_pane.files_filter_active = false;
                }
            }
            Action::Confirm => {
                // Enter: keep filter, exit filter mode
                self.files_pane.files_filter_active = false;
            }
            Action::Cancel => {
                // Esc: clear filter, exit filter mode
                self.files_pane.files_filter.clear();
                self.files_pane.files_filter_active = false;
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

        // Auto-start editing on character input
        if let Action::EditorChar(c) = action {
            if self.is_uncommitted_selected() {
                self.editing_commit_message = true;
                self.amending_commit = false;
                self.commit_editor.insert_char(c);
                self.scroll_to_editor_cursor();
                return Ok(());
            } else if self.is_head_commit_selected() {
                if let Ok(msg) = get_last_commit_message(&self.repo_path) {
                    self.commit_editor = crate::text_editor::TextEditor::from_text(&msg);
                    self.editing_commit_message = true;
                    self.amending_commit = true;
                    self.commit_editor.insert_char(c);
                    self.scroll_to_editor_cursor();
                    return Ok(());
                }
            }
            return Ok(());
        }

        if matches!(action, Action::AmendCommit) {
            if self.is_uncommitted_selected() {
                // Ctrl+Enter with no message: amend --no-edit
                commit_amend_no_edit(&self.repo_path)?;
                self.refresh(true)?;
                self.set_message("Commit amended (--no-edit)");
                self.focused_panel = FocusedPanel::Graph;
            }
            return Ok(());
        }

        match action {
            Action::StartEditing => {
                if self.is_uncommitted_selected() {
                    self.editing_commit_message = true;
                    self.amending_commit = false;
                } else if self.is_head_commit_selected() {
                    // Edit HEAD commit message for amending
                    if let Ok(msg) = get_last_commit_message(&self.repo_path) {
                        self.commit_editor = crate::text_editor::TextEditor::from_text(&msg);
                        self.editing_commit_message = true;
                        self.amending_commit = true;
                    }
                }
            }
            Action::MoveUp => {
                self.commit_detail_scroll = self.commit_detail_scroll.saturating_sub(1);
            }
            Action::MoveDown => {
                self.commit_detail_scroll =
                    (self.commit_detail_scroll + 1).min(self.commit_detail_max_scroll);
            }
            Action::PageUp => {
                self.commit_detail_scroll = self.commit_detail_scroll.saturating_sub(10);
            }
            Action::PageDown => {
                self.commit_detail_scroll =
                    (self.commit_detail_scroll + 10).min(self.commit_detail_max_scroll);
            }
            Action::GoToTop => {
                self.commit_detail_scroll = 0;
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
                self.amending_commit = false;
            }
            Action::CommitChanges => {
                let msg = self.commit_editor.text.trim().to_string();
                if self.amending_commit {
                    // Amending: use the edited message
                    if !msg.is_empty() {
                        commit_amend(&self.repo_path, &msg)?;
                        self.commit_editor = crate::text_editor::TextEditor::new();
                        self.editing_commit_message = false;
                        self.amending_commit = false;
                        self.refresh(true)?;
                        self.set_message("Commit amended");
                        self.focused_panel = FocusedPanel::Graph;
                    }
                } else if !msg.is_empty() {
                    commit_with_message(&self.repo_path, &msg)?;
                    self.commit_editor = crate::text_editor::TextEditor::new();
                    self.editing_commit_message = false;
                    self.refresh(true)?;
                    self.set_message("Changes committed");
                    self.focused_panel = FocusedPanel::Graph;
                }
            }
            Action::AmendCommit => {
                if self.amending_commit {
                    // Already editing HEAD commit — Ctrl+Enter acts same as Enter (save amend)
                    let msg = self.commit_editor.text.trim().to_string();
                    if !msg.is_empty() {
                        commit_amend(&self.repo_path, &msg)?;
                        self.commit_editor = crate::text_editor::TextEditor::new();
                        self.editing_commit_message = false;
                        self.amending_commit = false;
                        self.refresh(true)?;
                        self.set_message("Commit amended");
                        self.focused_panel = FocusedPanel::Graph;
                    }
                } else {
                    // On uncommitted node — amend last commit
                    let msg = self.commit_editor.text.trim().to_string();
                    if msg.is_empty() {
                        commit_amend_no_edit(&self.repo_path)?;
                    } else {
                        commit_amend(&self.repo_path, &msg)?;
                    }
                    self.commit_editor = crate::text_editor::TextEditor::new();
                    self.editing_commit_message = false;
                    self.refresh(true)?;
                    self.set_message("Commit amended");
                    self.focused_panel = FocusedPanel::Graph;
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
        self.scroll_to_editor_cursor();
        Ok(())
    }

    /// Auto-scroll the commit detail pane to keep the editor cursor visible.
    fn scroll_to_editor_cursor(&mut self) {
        let (cursor_row, _) = self.commit_editor.cursor_position();
        let absolute_row = self.commit_editor_line_offset as usize + cursor_row;
        let scroll = self.commit_detail_scroll as usize;
        let visible = self.commit_detail_visible_rows as usize;
        if visible == 0 {
            return;
        }
        if absolute_row < scroll {
            self.commit_detail_scroll = absolute_row as u16;
        } else if absolute_row >= scroll + visible {
            self.commit_detail_scroll = (absolute_row - visible + 1) as u16;
        }
        // Don't clamp to max_scroll here — the editor may have added lines
        // that haven't been rendered yet, so max_scroll is stale. The next
        // render will recompute the correct max.
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

    fn handle_branch_picker_action(&mut self, action: Action) -> Result<()> {
        let AppMode::BranchPicker { branches, selected } = &self.mode else {
            return Ok(());
        };
        let branches = branches.clone();
        let selected = *selected;

        match action {
            Action::MoveUp => {
                let new = if selected == 0 { branches.len().saturating_sub(1) } else { selected - 1 };
                self.mode = AppMode::BranchPicker { branches, selected: new };
            }
            Action::MoveDown => {
                let new = if selected + 1 >= branches.len() { 0 } else { selected + 1 };
                self.mode = AppMode::BranchPicker { branches, selected: new };
            }
            Action::MenuSelect | Action::Confirm => {
                if let Some(branch_name) = branches.get(selected) {
                    let name = branch_name.clone();
                    self.mode = AppMode::Normal;
                    self.checkout_branch_by_name(&name)?;
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
            Action::SelectAll => {
                self.hidden_branches.clear();
                self.mode = AppMode::BranchFilter {
                    filter,
                    selected,
                    all_branches,
                };
            }
            Action::SelectNone => {
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
                self.start_push();
            }
        }
        Ok(())
    }

    fn toggle_stage_selected_file(&mut self) -> Result<()> {
        if !self.is_uncommitted_selected() {
            return Ok(());
        }

        let files = self
            .selected_files()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        if files.is_empty() {
            return Ok(());
        }

        // Determine direction: if any file is unstaged, we stage; otherwise unstage all
        let any_unstaged = files
            .iter()
            .any(|f| !matches!(f.stage_status, Some(StageStatus::Staged)));
        let staging = any_unstaged;

        for file in &files {
            let path_str = file.path.to_string_lossy().to_string();
            if staging {
                stage_file(&self.repo_path, &path_str)?;
            } else {
                unstage_file(&self.repo_path, &path_str)?;
            }
        }

        // Record undo for single file; for multiple, record the first
        if files.len() == 1 {
            self.last_undoable_op = Some(UndoableOperation::Stage {
                path: files[0].path.to_string_lossy().to_string(),
                was_staged: !staging,
            });
        }

        self.refresh_after_file_op()?;
        Ok(())
    }

    fn add_selected_to_gitignore(&mut self) -> Result<()> {
        if !self.is_uncommitted_selected() {
            return Ok(());
        }

        // Resolve the pattern: header → folder path, file → file path
        let pattern = match self.selected_display_item() {
            Some(FilesPaneItem::Header(text)) => {
                // Header text is like "src/utils/" — use as-is
                text.clone()
            }
            Some(FilesPaneItem::File(file)) => {
                file.path.to_string_lossy().to_string()
            }
            None => return Ok(()),
        };

        match add_to_gitignore(&self.repo_path, &pattern)? {
            true => {
                self.last_undoable_op = Some(UndoableOperation::Gitignore {
                    pattern: pattern.clone(),
                });
                self.set_message(format!("Added '{}' to .gitignore", pattern));
                self.refresh_after_file_op()?;
            }
            false => {
                self.set_message(format!("'{}' already in .gitignore", pattern));
            }
        }

        Ok(())
    }

    fn archive_selected_file(&mut self) -> Result<()> {
        if !self.is_uncommitted_selected() {
            return Ok(());
        }

        // Resolve target: header → folder path (without trailing /), file → file path
        let target = match self.selected_display_item() {
            Some(FilesPaneItem::Header(text)) => {
                text.trim_end_matches('/').to_string()
            }
            Some(FilesPaneItem::File(file)) => {
                file.path.to_string_lossy().to_string()
            }
            None => return Ok(()),
        };

        archive_path(&self.repo_path, &target)?;
        // Ensure .archive is in .gitignore
        let _ = add_to_gitignore(&self.repo_path, ".archive");
        self.last_undoable_op = Some(UndoableOperation::Archive {
            relative_path: target.clone(),
        });
        self.set_message(format!("Archived '{}'", target));
        self.refresh_after_file_op()?;

        Ok(())
    }

    fn unarchive_selected_file(&mut self) -> Result<()> {
        let Some(FilesPaneItem::File(file)) = self.selected_display_item().cloned() else {
            return Ok(());
        };
        let target = file.path.to_string_lossy().to_string();
        unarchive_path(&self.repo_path, &target)?;
        self.set_message(format!("Unarchived '{}'", target));
        self.refresh_after_file_op()?;
        Ok(())
    }

    fn trash_selected_file(&mut self) -> Result<()> {
        if !self.is_uncommitted_selected() {
            return Ok(());
        }

        // Only allow trashing untracked files; tracked files should use restore (r)
        let paths: Vec<String> = self
            .selected_files()
            .into_iter()
            .filter(|f| matches!(f.stage_status, Some(StageStatus::Untracked)))
            .map(|f| f.path.to_string_lossy().to_string())
            .collect();
        if paths.is_empty() {
            return Ok(());
        }

        let label = if paths.len() == 1 {
            format!("'{}'", paths[0])
        } else {
            format!("{} files", paths.len())
        };
        self.mode = AppMode::Confirm {
            message: format!("Move {} to recycle bin?", label),
            action: ConfirmAction::TrashFile(paths),
        };
        Ok(())
    }

    fn restore_selected_file(&mut self) -> Result<()> {
        if !self.is_uncommitted_selected() {
            return Ok(());
        }

        let files: Vec<_> = self.selected_files().into_iter().cloned().collect();
        if files.is_empty() {
            return Ok(());
        }

        let all_new = files.iter().all(|f| {
            matches!(f.kind, FileChangeKind::Added)
                || matches!(f.stage_status, Some(StageStatus::Untracked))
        });

        let paths: Vec<String> = files
            .iter()
            .map(|f| f.path.to_string_lossy().to_string())
            .collect();

        let label = if paths.len() == 1 {
            format!("'{}'", paths[0])
        } else {
            format!("{} files", paths.len())
        };
        let message = if all_new {
            format!(
                "Delete {}? This file is untracked and will be permanently removed.",
                label
            )
        } else {
            format!("Discard changes to {}?", label)
        };
        self.mode = AppMode::Confirm {
            message,
            action: ConfirmAction::RestoreFile(paths),
        };
        Ok(())
    }

    fn undo_last_file_op(&mut self) -> Result<()> {
        let Some(op) = self.last_undoable_op.take() else {
            self.set_message("Nothing to undo");
            return Ok(());
        };

        match op {
            UndoableOperation::Stage { path, was_staged } => {
                // Reverse: if it was_staged before the toggle, we unstaged it, so re-stage.
                // If it wasn't staged, we staged it, so unstage.
                if was_staged {
                    stage_file(&self.repo_path, &path)?;
                } else {
                    unstage_file(&self.repo_path, &path)?;
                }
                self.set_message(format!("Undid stage/unstage '{}'", path));
            }
            UndoableOperation::Gitignore { pattern } => {
                match remove_from_gitignore(&self.repo_path, &pattern)? {
                    true => self.set_message(format!(
                        "Removed '{}' from .gitignore",
                        pattern
                    )),
                    false => {
                        self.set_message(format!(
                            "'{}' not found in .gitignore",
                            pattern
                        ));
                        return Ok(());
                    }
                }
            }
            UndoableOperation::Archive { relative_path } => {
                unarchive_path(&self.repo_path, &relative_path)?;
                self.set_message(format!("Restored '{}' from archive", relative_path));
            }
        }

        self.refresh_after_file_op()?;
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
                // Return to normal with files panel focused, preserving file index
                let flat_index = if let AppMode::FileDiff { file_index, .. } = &self.mode {
                    Some(*file_index)
                } else {
                    None
                };
                if let Some(fi) = flat_index {
                    self.sync_file_list_cache();
                    self.select_file_at(self.flat_index_to_display_index(fi));
                }
                self.focused_panel = FocusedPanel::Files;
                self.return_to_normal();
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
        let ui_theme = self.theme();
        let (rendered_lines, hunk_positions) = build_highlighted_lines(&content, &ui_theme);
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
        let result = match self.current_diff_target() {
            Some(DiffTarget::Commit(oid)) => {
                FileDiffContent::from_commit(self.repo.repo(), oid, file_path)
            }
            Some(DiffTarget::Uncommitted) | None => {
                FileDiffContent::from_working_tree(self.repo.repo(), file_path)
            }
        };
        // If diff fails (e.g. added file with no parent entry), return empty content
        result.or_else(|_| {
            Ok(FileDiffContent {
                path: file_path.to_path_buf(),
                kind: FileChangeKind::Added,
                is_binary: false,
                hunks: Vec::new(),
                total_additions: 0,
                total_deletions: 0,
            })
        })
    }

    /// Extract a file blob from a commit to a temp file, preserving the extension.
    fn extract_blob_to_temp(
        &self,
        commit_oid: git2::Oid,
        file_path: &std::path::Path,
    ) -> Result<std::path::PathBuf> {
        let commit = self.repo.repo().find_commit(commit_oid)?;
        let tree = commit.tree()?;
        let entry = tree.get_path(file_path)?;
        let blob = self.repo.repo().find_blob(entry.id())?;

        let ext = file_path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let stem = file_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());

        let tmp_dir = std::env::temp_dir().join("keifu");
        std::fs::create_dir_all(&tmp_dir)?;
        let tmp_path = tmp_dir.join(format!("{}{}", stem, ext));
        std::fs::write(&tmp_path, blob.content())?;
        Ok(tmp_path)
    }

    /// Open a file with the default system application.
    fn open_with_default(&mut self, full_path: &std::path::Path, display_path: &std::path::Path) {
        use std::process::{Command, Stdio};
        let result = if cfg!(target_os = "macos") {
            Command::new("open")
                .arg(full_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .stdin(Stdio::null())
                .spawn()
        } else if cfg!(target_os = "windows") {
            Command::new("cmd")
                .args(["/C", "start", ""])
                .arg(full_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .stdin(Stdio::null())
                .spawn()
        } else {
            Command::new("xdg-open")
                .arg(full_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .stdin(Stdio::null())
                .spawn()
        };
        match result {
            Ok(_) => self.set_message(format!("Opening {}", display_path.display())),
            Err(e) => self.set_message(format!("Cannot open file: {e}")),
        }
    }

    /// Sync the file_list held by FileDiff with the latest
    /// uncommitted diff cache.  Called right after `uncommitted_diff_cache` is
    /// updated so that navigation and display stay consistent.
    fn sync_file_list_with_uncommitted_diff(&mut self) {
        if self.current_diff_target() != Some(DiffTarget::Uncommitted) {
            return;
        }

        let new_files = match self.diff_cache.cached_diff(Some(DiffTarget::Uncommitted)) {
            Some(diff) => diff.files.clone(),
            None => return,
        };

        if new_files.is_empty() {
            if matches!(self.mode, AppMode::FileDiff { .. }) {
                self.mode = AppMode::Normal;
                self.set_message("No changed files in this diff");
            }
            return;
        }

        if let AppMode::FileDiff {
            file_index,
            file_list,
            ..
        } = &mut self.mode
        {
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
                                    create_branch(self.repo.repo(), &input, commit.oid)?;
                                    self.refresh(true)?;
                                }
                            }
                        }
                    }
                    InputAction::AddTag => {
                        if !input.is_empty() {
                            if let Some(node) = self.selected_commit_node() {
                                if let Some(commit) = &node.commit {
                                    add_tag(self.repo.repo(), &input, commit.oid)?;
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
                        delete_branch(self.repo.repo(), &name)?;
                    }
                    ConfirmAction::Merge(name) => {
                        merge_branch(self.repo.repo(), &name)?;
                    }
                    ConfirmAction::Rebase(name) => {
                        rebase_branch(self.repo.repo(), &name)?;
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
                        self.start_push();
                    }
                    ConfirmAction::RestoreFile(paths) => {
                        restore_files(&self.repo_path, &paths)?;
                        let label = if paths.len() == 1 {
                            format!("'{}'", paths[0])
                        } else {
                            format!("{} files", paths.len())
                        };
                        self.set_message(format!("Restored {}", label));
                        self.mode = AppMode::Normal;
                        self.refresh_after_file_op()?;
                        return Ok(());
                    }
                    ConfirmAction::TrashFile(paths) => {
                        let mut errors = Vec::new();
                        for path in &paths {
                            let full = std::path::Path::new(&self.repo_path).join(path);
                            if let Err(e) = trash::delete(&full) {
                                errors.push(format!("{}: {}", path, e));
                            }
                        }
                        if errors.is_empty() {
                            let label = if paths.len() == 1 {
                                format!("'{}'", paths[0])
                            } else {
                                format!("{} files", paths.len())
                            };
                            self.set_message(format!("Moved {} to recycle bin", label));
                        } else {
                            self.set_message(format!("Trash errors: {}", errors.join("; ")));
                        }
                        self.mode = AppMode::Normal;
                        self.refresh_after_file_op()?;
                        return Ok(());
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

    // ── Graph navigation delegates ────────────────────────────────

    fn move_selection(&mut self, delta: i32) {
        self.graph_nav.move_selection(&self.graph_layout, delta);
    }

    fn select_first(&mut self) {
        self.graph_nav.select_first(&self.graph_layout);
    }

    fn select_last(&mut self) {
        self.graph_nav.select_last(&self.graph_layout);
    }

    fn move_to_next_branch(&mut self) {
        self.graph_nav.move_to_next_branch();
    }

    fn move_to_prev_branch(&mut self) {
        self.graph_nav.move_to_prev_branch();
    }

    fn selected_branch(&self) -> Option<&BranchInfo> {
        self.graph_nav.selected_branch(&self.branches)
    }

    pub fn selected_branch_name(&self) -> Option<&str> {
        self.graph_nav.selected_branch_name()
    }

    pub fn selected_node_branches(&self) -> Vec<&str> {
        self.graph_nav.selected_node_branches()
    }

    fn selected_commit_node(&self) -> Option<&crate::git::graph::GraphNode> {
        self.graph_nav.selected_node(&self.graph_layout)
    }

    fn do_checkout(&mut self) -> Result<()> {
        let branches: Vec<String> = self
            .selected_node_branches()
            .iter()
            .map(|s| s.to_string())
            .collect();

        match branches.len() {
            0 => {
                if let Some(node) = self.selected_commit_node() {
                    if let Some(commit) = &node.commit {
                        checkout_commit(self.repo.repo(), commit.oid)?;
                        self.refresh(true)?;
                    }
                }
            }
            1 => {
                self.checkout_branch_by_name(&branches[0])?;
            }
            _ => {
                self.mode = AppMode::BranchPicker {
                    branches,
                    selected: 0,
                };
            }
        }
        Ok(())
    }

    fn checkout_branch_by_name(&mut self, branch_name: &str) -> Result<()> {
        if branch_name.starts_with("origin/") {
            checkout_remote_branch(self.repo.repo(), branch_name)?;
        } else {
            checkout_branch(self.repo.repo(), branch_name)?;
        }
        self.refresh(true)?;
        Ok(())
    }
}
