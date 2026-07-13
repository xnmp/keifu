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
    network::{NetworkManager, PushSpec},
    git::{
        build_graph,
        graph::GraphLayout,
        operations::{
            abort_operation, accept_ours, accept_theirs, add_tag, apply_patch_cached,
            apply_patch_cached_reverse, apply_patch_worktree_reverse, cherry_pick,
            checkout_branch, checkout_commit, checkout_remote_branch, commit_amend,
            commit_amend_no_edit, commit_with_message, continue_operation, create_branch,
            delete_branch, delete_remote_branch, delete_tag, get_last_commit_message,
            merge_branch, prune_remote, push_tag, rebase_branch, rename_branch,
            reset_to_commit, restore_files, revert_commit, stage_all, stage_file,
            stash_all, stash_apply, stash_branch, stash_drop, stash_pop, stash_staged,
            unstage_all, unstage_file, OpOutcome, ResetMode,
        },
        extract_hunk_from_working_tree, render_hunk_patch,
        BranchInfo, CommitDiffInfo, CommitInfo, FileChangeKind, FileDiffContent, FileDiffInfo,
        GitRepository, OperationState, StageStatus, WorkingTreeStatus,
    },
    search::{fuzzy_search_branches, FuzzySearchResult},
    workspace::{add_to_gitignore, archive_path, remove_from_gitignore, unarchive_path},
};

mod init;
mod refresh;
mod conflict_actions;
mod network_ops;
mod remote_ops;
mod status_message;
mod search_ops;
mod graph_actions;
mod file_ops;
mod commit_editor_actions;
mod commit_menu_actions;
mod branch_picker_actions;
mod file_diff_actions;
mod input_actions;
mod confirm_actions;
mod compare_actions;
mod file_history_actions;

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

/// Format a file-count label: the single quoted path, or "N files".
fn file_count_label(paths: &[String]) -> String {
    if paths.len() == 1 {
        format!("'{}'", paths[0])
    } else {
        format!("{} files", paths.len())
    }
}

/// Previous index with wrap-around; returns 0 when `len` is 0.
fn cyclic_prev(selected: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    if selected == 0 {
        len - 1
    } else {
        selected - 1
    }
}

/// Next index with wrap-around; returns 0 when `len` is 0.
fn cyclic_next(selected: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    if selected + 1 >= len {
        0
    } else {
        selected + 1
    }
}

/// Which panel is focused
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusedPanel {
    Graph,
    Files,
    CommitDetail,
}

/// Which network operation a remote picker runs once a remote is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteOp {
    Fetch,
    Pull,
    Push,
    Prune,
}

/// Items in the commit context menu
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitMenuItem {
    Push,
    Pull,
    Checkout,
    CreateBranch,
    DeleteBranch,
    MergeIntoCurrent,
    CherryPick,
    Rebase,
    RenameBranch,
    Reset,
    ResetSoft,
    ResetMixed,
    ResetHard,
    AddTag,
    DeleteTag,
    PushTag,
    Revert,
    Prune,
    CopyHash,
    CopyMessage,
    MarkForCompare,
    CompareWithMarked,
    StashApply,
    StashPop,
    StashDrop,
    BranchFromStash,
    /// Stash options (shown in the stash menu on the uncommitted node).
    StashPushStaged,
    StashPushAll,
    StashPushUntracked,
}

impl CommitMenuItem {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Push => "Push",
            Self::Pull => "Pull",
            Self::Checkout => "Checkout",
            Self::CreateBranch => "Create branch here",
            Self::DeleteBranch => "Delete branch",
            Self::MergeIntoCurrent => "Merge into current branch",
            Self::CherryPick => "Cherry-pick",
            Self::Rebase => "Rebase current branch onto this",
            Self::RenameBranch => "Rename branch",
            Self::Reset => "Reset to this commit...",
            Self::ResetSoft => "Soft (keep changes staged)",
            Self::ResetMixed => "Mixed (keep changes unstaged)",
            Self::ResetHard => "Hard (discard all changes)",
            Self::AddTag => "Add tag",
            Self::DeleteTag => "Delete tag",
            Self::PushTag => "Push tag",
            Self::Revert => "Revert this commit",
            Self::Prune => "Prune remote-tracking refs",
            Self::CopyHash => "Copy commit hash",
            Self::CopyMessage => "Copy commit message",
            Self::MarkForCompare => "Mark for compare",
            Self::CompareWithMarked => "Compare with marked commit",
            Self::StashApply => "Apply stash",
            Self::StashPop => "Pop stash (apply + drop)",
            Self::StashDrop => "Drop stash",
            Self::BranchFromStash => "Branch from stash",
            Self::StashPushStaged => "Stash staged changes",
            Self::StashPushAll => "Stash all changes",
            Self::StashPushUntracked => "Stash all + untracked",
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
        filter: String,
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
    BranchDeletePicker {
        branches: Vec<String>,
        selected: usize,
    },
    /// Pick which tag to act on when a commit carries more than one.
    TagPicker {
        tags: Vec<String>,
        selected: usize,
        action: TagAction,
    },
    /// Pick which remote a network op (fetch/pull/push/prune) targets, shown
    /// only when the repo has multiple remotes and the branch's upstream can't
    /// disambiguate.
    RemotePicker {
        remotes: Vec<String>,
        selected: usize,
        op: RemoteOp,
    },
    FileDiff {
        /// Which diff this viewer was opened for. Navigation (next/prev file)
        /// and reloads stay pinned to this target rather than re-deriving it
        /// from the currently selected graph node, so a diff opened from file
        /// history or a comparison keeps showing the right commit(s).
        diff_target: DiffTarget,
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
    /// Per-file commit history picker (commits that touched a path).
    FileHistory {
        path: std::path::PathBuf,
        entries: Vec<FileHistoryEntry>,
        selected: usize,
    },
}

/// One entry in the per-file history list.
#[derive(Debug, Clone)]
pub struct FileHistoryEntry {
    pub oid: Oid,
    pub short_id: String,
    pub date: String,
    pub subject: String,
}

/// Which tag operation a [`AppMode::TagPicker`] resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagAction {
    Delete,
    Push,
}

/// Scope of a `git stash push`, chosen from the stash options menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StashScope {
    /// Only staged changes (`git stash push --staged`).
    Staged,
    /// All tracked working-tree changes (`git stash push`).
    All,
    /// All changes including untracked files (`git stash push -u`).
    AllUntracked,
}

/// Input action kinds
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    CreateBranch,
    AddTag,
    Search,
    /// Rename the local branch `old_name` to the typed name.
    RenameBranch { old_name: String },
    /// Create a branch from `stash@{index}` with the typed name.
    BranchFromStash { index: usize },
    /// Stash the working tree with the typed (optional) message.
    StashPush { scope: StashScope },
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
    /// Delete a branch on a remote (`git push <remote> --delete <branch>`).
    DeleteRemoteBranch { remote: String, branch: String },
    TrashFile(Vec<String>),
    RestoreFile(Vec<String>),
    StashDrop(usize),
    DeleteTag(String),
    /// Abort the in-progress merge/rebase/cherry-pick/revert.
    AbortOperation(OperationState),
    /// Discard a single hunk in the working tree (reverse-apply). Carries the
    /// pre-rendered patch plus the FileDiff viewer state needed to reopen the
    /// diff at the same file/scroll after the confirmation.
    DiscardHunk {
        patch: String,
        file_path: std::path::PathBuf,
        scroll_offset: usize,
    },
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

    // Commit filter (graph panel Ctrl+F)
    pub commit_filter: String,
    pub commit_filter_active: bool,
    pub visible_commit_indices: Vec<usize>,

    // Search state
    pub search_state: SearchState,

    // Latest working tree status snapshot
    pub working_tree_status: Option<WorkingTreeStatus>,

    // In-progress operation (merge/rebase/cherry-pick/revert) and its conflict
    // count, refreshed alongside the working tree. Drives the status-bar
    // indicator and the conflict-resolution keybindings.
    pub op_state: OperationState,
    pub conflict_count: usize,

    // Diff caching subsystem
    pub diff_cache: DiffCache,

    // Commit comparison ("mark for compare"). `compare_marked` holds the first
    // pending commit; once a second commit is chosen, `compare_range` holds the
    // active (old, new) pair (ordered older → newer by commit time) which
    // overrides the diff target until cleared with Esc.
    pub compare_marked: Option<Oid>,
    pub compare_range: Option<(Oid, Oid)>,

    // Per-OID GPG signature status cache (%G? code). Commits are immutable so
    // this never needs invalidation; populated lazily on commit-detail render.
    pub sig_status_cache: std::collections::HashMap<Oid, char>,

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

    // Filesystem watcher
    pub watcher: Option<crate::watcher::FsWatcher>,
    // Watcher still being built on a background thread; installed into
    // `watcher` by poll_fs_watcher once ready.
    pub pending_watcher: Option<crate::watcher::PendingFsWatcher>,

    // Undo
    pub last_undoable_op: Option<UndoableOperation>,

    // Layout
    pub side_panel_layout: bool,

    // Debug mode
    pub debug_keys: bool,

    // Config
    pub config: Config,

    // Terminal background color (r, g, b), detected once at startup.
    // Used to derive theme-adaptive structural colors. `None` when the
    // terminal doesn't report it (e.g. headless tests).
    pub terminal_bg: Option<(u8, u8, u8)>,
}

impl App {

    /// Check if the currently selected node is the uncommitted changes node
    /// Return the active theme based on config.
    pub fn theme(&self) -> crate::ui::theme::Theme {
        use crate::ui::theme::Theme;

        // Pick the base palette: explicit config, else auto from the cached
        // terminal background (luma > 0.5 ⇒ light).
        let base = match self.config.ui.theme.as_str() {
            "light" => Theme::light(),
            "dark" => Theme::dark(),
            _ => match self.terminal_bg {
                Some((r, g, b)) => {
                    let luma = (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0;
                    if luma > 0.5 {
                        Theme::light()
                    } else {
                        Theme::dark()
                    }
                }
                None => Theme::dark(),
            },
        };

        // Derive structural colors (borders, dates, muted text) from the real
        // background so they stay visible and track the terminal theme.
        match self.terminal_bg {
            Some(bg) => base.adapt_to_background(bg),
            None => base,
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
        let is_uncommitted = self.diff_target_is_uncommitted();
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
        let is_uncommitted = self.diff_target_is_uncommitted();
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
            AppMode::BranchDeletePicker { .. } => self.handle_branch_delete_picker_action(action)?,
            AppMode::TagPicker { .. } => self.handle_tag_picker_action(action)?,
            AppMode::RemotePicker { .. } => self.handle_remote_picker_action(action)?,
            AppMode::BranchFilter { .. } => self.handle_branch_filter_action(action)?,
            AppMode::FileDiff { .. } => self.handle_file_diff_action(action)?,
            AppMode::FileHistory { .. } => self.handle_file_history_action(action)?,
        }
        Ok(())
    }

    /// Show an error
    pub fn show_error(&mut self, message: String) {
        self.mode = AppMode::Error { message };
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
}
