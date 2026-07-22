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
            create_lightweight_tag, delete_branch, delete_tag,
            extract_auth_url, get_last_commit_message, is_annotated_tag, is_https_auth_failure,
            reset_hard_checked, url_host,
            humanize_git_error, is_dirty_worktree_pull_error, is_divergent_pull_error,
            merge_branch, prune_remote, push_tag,
            rebase_branch, rename_branch, reset_to_commit, restore_files, revert_commit, stage_all,
            stage_file, stash_all, stash_apply, stash_branch, stash_drop, stash_pop, stash_staged,
            unstage_all, unstage_file, OpOutcome, PullMode, ResetMode,
        },
        extract_hunk_from_working_tree, remote_only_branch_names, render_hunk_patch, short_hash,
        BranchInfo, CommitDiffInfo, CommitInfo, Credentials, FileChangeKind, FileDiffContent,
        FileDiffInfo, GitRepository, OperationState, StageStatus, WorkingTreeStatus,
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
mod ci_checks_actions;
mod pr_thread_actions;
mod pr_action_actions;
mod issue_actions;
mod mouse_actions;
mod file_ops;
mod commit_editor_actions;
mod commit_menu_actions;
mod branch_picker_actions;
mod file_diff_actions;
mod input_actions;
mod credentials;
mod confirm_actions;
mod compare_actions;
mod file_history_actions;
mod palette_actions;
mod undo_actions;

/// Which mechanism handled a successful clipboard copy, and whether the
/// payload had to be truncated (OSC 52 fallback only, which some terminals
/// cap around 100KB of base64).
struct ClipboardOutcome {
    via_osc52: bool,
    truncated: bool,
}

impl ClipboardOutcome {
    /// Short suffix to append to status-line feedback. Empty for the common
    /// case (a shell clipboard tool handled it) so existing messages are
    /// unchanged; only decorated when the OSC 52 fallback was used.
    fn suffix(&self) -> &'static str {
        match (self.via_osc52, self.truncated) {
            (false, _) => "",
            (true, false) => " (via OSC 52)",
            (true, true) => " (via OSC 52, truncated)",
        }
    }
}

/// Copy text to system clipboard using platform-specific commands
/// (xclip/xsel/wl-copy/pbcopy). If none of those tools are available, falls
/// back to emitting an OSC 52 escape sequence directly to the terminal —
/// this works over SSH and needs no external binary, but not every terminal
/// emulator supports it, which is why it's a fallback rather than the
/// primary mechanism.
fn copy_to_clipboard(text: &str) -> Result<ClipboardOutcome> {
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
                return Ok(ClipboardOutcome {
                    via_osc52: false,
                    truncated: false,
                });
            }
        }
    }

    // No shell clipboard tool found (missing or every candidate failed) —
    // fall back to OSC 52. This is called from an action handler, which
    // runs after the frame has been drawn and before the next one, so
    // writing raw escape bytes to stdout here doesn't corrupt the ratatui
    // frame.
    let truncated = crate::tui::copy_to_clipboard_osc52(text)?;
    Ok(ClipboardOutcome {
        via_osc52: true,
        truncated,
    })
}

/// Open a URL in the user's default browser, detached so the TUI's terminal
/// state isn't disturbed (stdio redirected to null, no wait). Best-effort:
/// tries `xdg-open` (Linux) then `open` (macOS).
fn open_url(url: &str) -> Result<()> {
    use std::process::{Command, Stdio};

    for cmd in ["xdg-open", "open"] {
        if Command::new(cmd)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
    }

    anyhow::bail!("No URL opener found (install xdg-open)")
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
    /// Create a PR from the current branch (open PR compose).
    CreatePr,
    /// Merge the open PR on this commit (open merge-method picker).
    MergePr,
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
            Self::CreatePr => "Create pull request...",
            Self::MergePr => "Merge pull request...",
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
    CommitMenu {
        items: Vec<CommitMenuItem>,
        selected: usize,
        filter: String,
    },
    /// Checkbox menu to toggle which metadata columns show on commit rows.
    MetadataMenu {
        selected: usize,
    },
    /// Settings menu (Ctrl+,): the full inventory of user-facing settings,
    /// grouped and editable. `selected` indexes `settings::descriptors()`
    /// (always a real, absolute index — never a position within the filtered
    /// subset); `editing` holds the in-progress numeric buffer when typing a
    /// value; `query` fuzzy-filters the list, matching the command palette.
    Settings {
        selected: usize,
        editing: Option<String>,
        query: String,
    },
    /// A `--ff-only` pull failed on divergent branches: choose merge or rebase.
    /// The remote/branch to rerun with are held in `App.last_pull`.
    PullDivergence {
        selected: usize,
    },
    /// CI check details for the selected commit's PR. The data lives on
    /// `App.ci_checks` (filled asynchronously); this variant just routes keys.
    CiChecks,
    /// The selected commit's PR conversation (description, comments, reviews,
    /// review threads). Data on `App.pr_thread`.
    PrThread,
    /// Compose a PR title/body or a review body in `App.pr_editor`.
    PrCompose {
        purpose: ComposePurpose,
    },
    /// Pick a merge method (merge / squash / rebase) for a PR.
    PrMergePicker {
        number: u64,
        selected: usize,
    },
    /// Pick a review disposition (approve / request changes / comment) for a PR.
    PrReviewPicker {
        number: u64,
        selected: usize,
    },
    /// GitHub issue list. Data on `App.issue_list` (filled asynchronously).
    IssueList,
    /// A single issue's detail (body + comments). Data on `App.issue_detail`.
    IssueDetail,
    /// Compose a new issue (title + body) or a comment in `App.issue_editor`.
    IssueCompose {
        purpose: IssueComposePurpose,
    },
    /// Toggle labels on an issue. The label set + chosen state live on
    /// `App.issue_label_picker`; this variant carries only the cursor row.
    IssueLabelPicker {
        number: u64,
        selected: usize,
    },
    /// Filter the issue list by label. The label set + chosen state live on
    /// `App.issue_label_filter`; this variant carries only the cursor row.
    IssueLabelFilter {
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
    /// Fuzzy command palette (Ctrl+P): commands, branches, and commits in one
    /// ranked list. Holds the query string and the selected row.
    CommandPalette {
        query: String,
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

/// State of the CI checks popup for one PR.
pub struct CiChecksView {
    pub pr_number: u64,
    /// PR URL, opened when no specific check URL is available.
    pub pr_url: String,
    pub checks: ChecksState,
    /// Selected row in the check list.
    pub selected: usize,
    /// `Some` when drilled into a check's log/detail (list is hidden).
    pub log: Option<LogView>,
}

/// The check-list fetch state.
pub enum ChecksState {
    Loading,
    Loaded(Vec<crate::checks::CheckRun>),
    Error(String),
}

/// A drilled-in check detail: a failed run's log tail, an external URL, or an
/// error/loading placeholder.
pub struct LogView {
    pub title: String,
    /// The Actions run this log is for, so the async poll can match it.
    pub run_id: Option<u64>,
    pub content: LogContent,
    pub scroll: usize,
}

pub enum LogContent {
    Loading,
    Lines(Vec<String>),
    /// A non-Actions check: only a URL to open in the browser.
    External(String),
    Error(String),
}

/// State of the PR conversation popup.
pub struct PrThreadView {
    pub pr_number: u64,
    /// PR URL, opened with `o`.
    pub pr_url: String,
    pub state: ThreadViewState,
    /// Scroll offset in wrapped rows; clamped to `max_scroll` each frame.
    pub scroll: usize,
    pub max_scroll: usize,
}

pub enum ThreadViewState {
    Loading,
    Loaded(crate::pr_thread::PrThread),
    Error(String),
}

/// State of the GitHub issue-list popup. Errors render inline (never
/// `AppMode::Error`), mirroring `CiChecksView`/`PrThreadView`.
pub struct IssueListView {
    pub state: IssueListState,
    /// Selected row — an index into the *visible* (filtered) rows, not the raw
    /// `Ready` list, so navigation operates on what the user sees.
    pub selected: usize,
    pub filter: crate::issue::IssueFilter,
    /// Client-side narrowing (labels / unblocked-only) applied over the fetched
    /// rows without a refetch.
    pub view_filter: crate::issue::IssueViewFilter,
    /// First visible row, kept in range each frame by the widget.
    pub scroll: usize,
    /// Issue number to reselect once a refetch completes, captured when the view
    /// transitions to `Loading` so a refresh/mutation doesn't jump to row 0.
    pub pending_reselect: Option<u64>,
}

impl IssueListView {
    /// Indices into the `Ready` list that pass the current view filter. `blocked`
    /// is the set of blocked issue numbers (empty when unknown). Non-`Ready`
    /// states have no visible rows.
    pub fn visible(&self, blocked: &std::collections::HashSet<u64>) -> Vec<usize> {
        match &self.state {
            IssueListState::Ready(issues) => {
                crate::issue::visible_issues(issues, &self.view_filter, blocked)
            }
            _ => Vec::new(),
        }
    }
}

pub enum IssueListState {
    Loading,
    Ready(Vec<crate::issue::IssueInfo>),
    Error(String),
}

/// State of the issue-detail popup.
pub struct IssueDetailView {
    pub number: u64,
    pub state: IssueDetailState,
    /// Scroll offset in wrapped rows; clamped to `max_scroll` each frame.
    pub scroll: usize,
    pub max_scroll: usize,
}

pub enum IssueDetailState {
    Loading,
    /// Boxed to keep the enum small (`IssueDetail` is large vs the other
    /// variants), which also keeps `IssueDetailView` cheap to move.
    Ready(Box<crate::issue::IssueDetail>),
    Error(String),
}

/// What the issue-compose editor is composing. For `NewIssue` the first editor
/// line is the title and the rest is the body; for `Comment` the whole buffer
/// is the comment body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueComposePurpose {
    NewIssue,
    Comment { number: u64 },
}

/// Live label-picker data (on `App`), so the `IssueLabelPicker` mode variant can
/// stay unit-ish (cursor only). `labels` is the repo's full label set;
/// `original` is which were on the issue when the picker opened; `chosen` is the
/// current toggle state. The apply set-diff compares `chosen` against `original`.
pub struct IssueLabelPicker {
    pub number: u64,
    pub labels: Vec<crate::issue::IssueLabel>,
    pub original: Vec<bool>,
    pub chosen: Vec<bool>,
}

/// Live label-*filter* picker data (on `App`), used by the list's `t` filter.
/// `labels` is the repo's full label set; `chosen` is the in-progress checkbox
/// state. On apply the checked label names become the list's `view_filter`.
pub struct IssueLabelFilter {
    pub labels: Vec<crate::issue::IssueLabel>,
    pub chosen: Vec<bool>,
}

/// Panel rectangles recorded during render, for mouse hit-testing. All are the
/// outer panel rects (including borders); the inner list area is inset by 1.
/// The divider fields are the borders between panels (for drag-resize).
#[derive(Debug, Clone, Copy, Default)]
pub struct MouseLayout {
    pub graph: ratatui::layout::Rect,
    pub files: ratatui::layout::Rect,
    pub commit: ratatui::layout::Rect,
    /// The graph+detail area, for computing divider-drag ratios.
    pub main: ratatui::layout::Rect,
    /// True when graph is on the right and detail on the left (side layout).
    pub side_layout: bool,
}

/// What the `PrCompose` editor is composing. The editor's first line is the
/// PR title (Create); for reviews the whole buffer is the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposePurpose {
    CreatePr,
    ReviewComment { pr: u64 },
    ReviewRequestChanges { pr: u64 },
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
    /// Edit an issue's assignees: the typed comma-separated logins become the
    /// desired final set; the handler diffs it against the issue's current
    /// assignees to compute add/remove.
    EditIssueAssignees { number: u64 },
    /// First step of the HTTPS credential prompt: enter the username. The
    /// pending op + host live on `App.pending_auth`.
    AuthUsername,
    /// Second step of the HTTPS credential prompt: enter the password/token.
    /// Rendered masked.
    AuthPassword,
}

/// A network op that can be re-issued with credentials after an auth-failure
/// prompt. Captures everything needed to rerun the exact same command.
#[derive(Debug, Clone)]
pub enum RetryableOp {
    Fetch { remote: String, show_message: bool, silent: bool },
    FetchAll,
    Push(PushSpec),
    Pull { remote: Option<String>, branch: Option<String>, mode: PullMode },
}

/// A network op currently in flight, kept so its completion handler can drive
/// the credential-prompt retry on an auth failure.
#[derive(Debug, Clone)]
pub struct InFlightOp {
    pub op: RetryableOp,
    /// The op's target host (for credential-cache lookup), if resolvable.
    pub host: Option<String>,
    /// Whether credentials were attached to this attempt (so a fresh auth
    /// failure means the cached creds are stale).
    pub had_creds: bool,
    /// True for a silent background auto-fetch — never prompt on those.
    pub silent: bool,
    /// How many credential prompts this auth episode has already shown.
    pub attempts: u32,
}

/// State of an in-progress HTTPS credential prompt: the op to retry once the
/// user finishes entering their username + password/token.
#[derive(Debug, Clone)]
pub struct PendingAuth {
    pub op: RetryableOp,
    pub host: String,
    /// Filled after the username step; `None` while collecting it.
    pub username: Option<String>,
    /// Prompt count for this episode (capped to avoid infinite re-prompt loops).
    pub attempts: u32,
}

/// Confirmation action kinds
#[derive(Debug, Clone)]
pub enum ConfirmAction {
    /// Check out `name`. `is_remote` is the branch's authoritative
    /// remote/local status, threaded through so the checkout creates/tracks a
    /// local branch off a remote-tracking ref instead of guessing from an
    /// "origin/" prefix (mirrors [`ConfirmAction::Merge`]'s `is_remote`).
    Checkout { name: String, is_remote: bool },
    /// Load the entire commit history (may be a large walk).
    LoadAllCommits,
    /// Apply the newest undo-ledger entry (branch/tag delete, merge, pull, rename).
    Undo,
    DeleteBranch(String),
    /// Delete a local branch that also has a remote counterpart. Plain confirm
    /// (Enter) deletes only the local branch; the secondary confirm
    /// (Ctrl+Enter / `R`, see [`crate::action::Action::ConfirmDeleteBranchAndRemote`])
    /// also deletes `remote/branch` on the remote.
    DeleteBranchWithRemote {
        name: String,
        remote: String,
        branch: String,
    },
    /// Merge `name` into the current branch. `is_remote` is the selected
    /// branch's `BranchInfo::is_remote` at selection time, threaded through
    /// explicitly so the merge resolves `refs/remotes/<name>` for a
    /// remote-tracking branch instead of guessing from the name alone.
    Merge { name: String, is_remote: bool },
    /// Rebase the current branch onto `name`. Same `is_remote` threading as
    /// [`ConfirmAction::Merge`].
    Rebase { name: String, is_remote: bool },
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
    /// Confirm a mutating PR action (create / merge / approve / request-changes)
    /// before it runs asynchronously.
    PrAction(crate::pr_action::PrAction),
    /// Confirm a mutating issue action (close / reopen) before it runs
    /// asynchronously.
    IssueAction(crate::issue_action::IssueAction),
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

/// Commits loaded by the first revwalk, and the chunk size for each extension.
pub const INITIAL_COMMIT_LIMIT: usize = 500;
pub const COMMIT_CHUNK: usize = 500;
/// Auto-load the next chunk when the selection/scroll comes within this many
/// rows of the last loaded commit.
pub const AUTOLOAD_THRESHOLD: usize = 50;

/// Guard message shown when an index-mutating action or pull re-run is
/// blocked by a pull already in flight (both touch `.git/index.lock`).
pub const BUSY_PULL_IN_PROGRESS: &str = "busy: pull in progress";

/// Merged-branch classification subsystem: the async GitHub/local signals that
/// decide whether a branch is already merged (issue #60) plus the base-update
/// back-merge set (issue #55) and the hide-merged toggle. Grouped off `App` so
/// this cluster of derived + worker state lives together (issue #66).
pub struct MergedState {
    /// Local branches classified as already merged into the trunk (merge commit,
    /// fast-forward, or squash-merged via a GitHub PR). Delivered by the
    /// background classifier; drives the dimmed rendering and the hide-merged
    /// toggle. Not persisted — it's derived from the repo + PR state.
    pub branches: std::collections::HashSet<String>,
    /// `branch name → squash landing commit` for the squash-merged subset of
    /// `branches` (issue #81). Delivered alongside `branches` by the classifier;
    /// consumed only to draw the optional grey squash-link line, gated by
    /// `config.ui.squash_link_lines`. Empty for ancestry/fast-forward merges.
    pub squash_targets: std::collections::HashMap<String, git2::Oid>,
    /// Background classifier that computes `branches` off the UI thread, so a
    /// refresh never does per-branch git diffing inline.
    pub classify: crate::merged_branch_fetch::MergedClassifier,
    /// When true, merged branches are removed from the graph entirely rather than
    /// merely dimmed. Composes with `hidden_branches`. Persisted in `UiState`.
    pub hide: bool,
    /// When true, a merged branch shown in the graph (i.e. `hide` is false) is
    /// dimmed: chip color + "merged" badge (issue #106). Independent of `hide` —
    /// off renders a shown merged branch exactly like an unmerged one. Applies
    /// uniformly to every classification in `branches` (ancestry, fast-forward,
    /// or squash all flow through the same set, so there is nothing left for a
    /// squash-specific bypass to hook into). Persisted in `UiState`.
    pub dim: bool,
    /// Squash-merge detection via GitHub: head-branch names of *merged* PRs,
    /// refreshed in the background via `gh pr list --state merged`. The primary
    /// signal for catching squash-merged branches whose local ref survives (the
    /// remote copy having been deleted on merge). Empty when gh is unavailable.
    pub pr_branches: std::collections::HashSet<String>,
    pub pr_branch_fetch:
        crate::interval_fetch::IntervalFetch<std::collections::HashSet<String>>,
    /// OIDs of base-update ("back-merge") commits: merges on an open PR's branch
    /// that pulled the updated base branch in (issue #55). Derived from
    /// `open_prs` + the base branch via `classify_base_update_merges`; drives the
    /// strong-mute rendering when `metadata_columns.mute_base_merges` is on. Not
    /// persisted — recomputed from repo + PR state. The `SignatureGuarded`
    /// wrapper keeps the set and the fingerprint of the inputs it was computed
    /// from together, so a frequent refresh skips redundant ancestry work when
    /// neither the base tip nor the open-PR head set has changed.
    pub base_update:
        crate::signature_guarded::SignatureGuarded<std::collections::HashSet<git2::Oid>>,
    /// OIDs of the loaded commits that would vanish if `hide` were toggled on —
    /// the rows and graph strokes the "dim merged branches" setting (`dim`)
    /// greys when hide is off (issues #108/#111). The complement of the live
    /// refs' first-parent chains (see `graph::merged_lane_oids`), so it covers
    /// classified merged branches AND the side lanes of already-deleted
    /// merged-in branches, exactly like hide's first-parent walk. Empty when
    /// hide already removed them from the graph. Not persisted — derived state.
    pub lane_oids: std::collections::HashSet<git2::Oid>,
}

/// Once-per-episode latches for periodically-retried refresh/poll errors. Each
/// is set when the failure is first reported and re-armed (cleared) on the next
/// success, so a persistent failure doesn't re-flash the status bar every
/// refresh / poll tick (issue #66 grouped these four `bool`s off `App`).
#[derive(Default)]
pub struct RefreshLatches {
    /// Working-tree-status failures during periodic refresh.
    pub wt_status: bool,
    /// Auto-refresh timer failures.
    pub auto_refresh: bool,
    /// Filesystem-watcher-driven refresh failures.
    pub watch_refresh: bool,
    /// Background auto-fetch failures.
    pub auto_fetch: bool,
    /// Repo-handle `reopen()` failures during refresh (see the gated reopen in
    /// `refresh.rs`). Reported once per episode so a persistently-unreadable
    /// `.git` doesn't re-flash the status bar every refresh; a stale handle is
    /// kept as a best-effort fallback rather than aborting the refresh.
    pub reopen: bool,
    /// Background open-PR poll (`gh pr list`) failures (issue #65). Set on the
    /// first failure since the last success — so a gh-missing / no-remote repo
    /// reports once rather than every 5-minute tick — and cleared on success.
    /// The last-good PR map is kept on failure rather than wiped.
    pub pr_fetch: bool,
    /// Background merged-PR poll (`gh pr list --state merged`) failures (issue
    /// #65). Same once-per-episode shape as `pr_fetch`.
    pub merged_fetch: bool,
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
    /// How many commits the revwalk currently loads (grows on demand). The graph
    /// starts at `INITIAL_COMMIT_LIMIT` and extends when the selection nears the
    /// bottom or the user asks for more.
    pub commit_load_limit: usize,
    /// True once the revwalk has yielded fewer commits than the limit — i.e. the
    /// whole history is loaded and there's nothing more to fetch.
    pub all_commits_loaded: bool,
    pub branches: Vec<BranchInfo>,
    /// Configured remote names (e.g. "origin", "upstream"), refreshed alongside
    /// branches. Cached here so the per-frame graph render doesn't hit git2.
    pub remotes: Vec<String>,
    pub graph_layout: GraphLayout,
    /// Bumped every time `graph_layout` is reassigned. Lets the pixel pre-pass
    /// reuse a cached `RowSpec` list without diffing the layout.
    pub graph_generation: u64,

    // UI state
    pub graph_nav: GraphNav,
    pub focused_panel: FocusedPanel,
    /// Files pane subsystem state
    pub files_pane: FilesPaneState,
    pub hidden_branches: std::collections::HashSet<String>,
    /// Remote-tracking refnames (`origin/feature`) whose deletion is in flight.
    /// Filtered out of the graph so an optimistic delete's branch disappears
    /// immediately and stays gone across any refresh that fires before the
    /// async `git push --delete` completes (the on-disk ref lingers until then).
    /// Cleared per refname when the delete op resolves — success leaves the ref
    /// genuinely gone, failure triggers a refresh so it reappears.
    pub pending_remote_deletions: std::collections::HashSet<String>,
    /// Branch name -> author name, shown in the branch-filter picker. Computed
    /// lazily when the picker opens (see `open_branch_filter`), never on every
    /// refresh — attribution runs one revwalk per branch.
    pub branch_authors: std::collections::HashMap<String, String>,
    /// Snapshot of `(branch name, tip OID)` the cached `branch_authors` were
    /// computed from. When the current branch tips no longer match this, the
    /// cache is stale and recomputed on the next picker open.
    pub branch_authors_key: Vec<(String, git2::Oid)>,
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
    /// Soft line-wrapping in the full-screen file-diff viewer (Ctrl+Alt+W).
    /// Off by default: long lines truncate and scroll horizontally. Persisted.
    pub diff_word_wrap: bool,
    /// Source rows for the active FileDiff viewer, kept beside the mode so the
    /// wrap re-layout inputs don't bloat the AppMode enum. `None` when closed.
    pub diff_source: Option<crate::ui::file_diff_view::DiffSource>,

    // Status message with auto-clear
    pub message: Option<String>,
    pub message_time: Option<std::time::Instant>,
    /// Whether the current message is a network-progress message ("Pulling…")
    /// that should stay visible for the whole in-flight op, versus a plain
    /// transient status message that strictly obeys the 5s timeout. Progress
    /// messages are cleared on op completion so they can never be resurrected
    /// by a later, unrelated network op.
    pub message_sticky: bool,

    // Once-per-episode latches for periodically-retried refresh errors (see
    // `RefreshLatches`).
    pub refresh_latches: RefreshLatches,

    // Transient toast notifications for background-op outcomes.
    pub toasts: crate::toast::ToastQueue,
    // Armed after the first open-PR fetch fills the map, so the initial load
    // doesn't toast every PR as "new".
    pub pr_toasts_armed: bool,

    // Network operations (fetch/push/auto-refresh)
    pub network: NetworkManager,

    // ── HTTPS credential prompt (issue #33) ──────────────────────────────
    // Session credential cache, keyed by host — entered once, then supplied
    // transparently to later network ops on the same host.
    pub credentials: std::collections::HashMap<String, Credentials>,
    // The network op currently in flight, so its completion can drive the
    // credential-prompt retry on an auth failure.
    pub in_flight_op: Option<InFlightOp>,
    // An in-progress credential prompt (username → password → retry). `None`
    // when no prompt is open.
    pub pending_auth: Option<PendingAuth>,

    // Open GitHub PRs by head branch name, refreshed in the background via the
    // `gh` CLI. Empty when gh is unavailable or the repo has no GitHub remote.
    pub open_prs: std::collections::HashMap<String, crate::pr::PrInfo>,
    pub pr_fetch: crate::interval_fetch::IntervalFetch<
        std::collections::HashMap<String, crate::pr::PrInfo>,
    >,

    // Merged-branch classification subsystem (see `MergedState`): the async
    // GitHub/local merged signals, the hide-merged toggle, and the base-update
    // back-merge set.
    pub merged: MergedState,

    // The last pull's (remote, branch) so a divergence prompt can rerun it with
    // an explicit merge/rebase strategy.
    pub last_pull: Option<(Option<String>, Option<String>)>,
    // HEAD OID snapshotted when a pull launches (it's async), so its completion
    // can record a reset-to-here undo entry if the pull moved HEAD.
    pub pre_pull_head: Option<Oid>,

    // Session undo ledger for reversible graph ops (branch/tag delete, merge,
    // pull, rename). Separate from the files-pane file-op undo.
    pub undo_ledger: crate::undo::UndoLedger,

    // CI check details popup (AppMode::CiChecks): background fetcher + the
    // current view, `Some` only while the popup is open.
    pub check_fetch: crate::checks::CheckFetch,
    pub ci_checks: Option<CiChecksView>,

    // PR conversation popup (AppMode::PrThread): background fetcher + view.
    pub thread_fetch: crate::pr_thread::PrThreadFetch,
    pub pr_thread: Option<PrThreadView>,

    // Mutating PR actions (create/merge/review): compose editor + async runner.
    pub pr_editor: crate::text_editor::TextEditor,
    pub pr_action_runner: crate::pr_action::PrActionRunner,

    // GitHub Issues: on-demand fetcher + async action runner, the list/detail
    // popup views (`Some` only while open), the compose editor, and the live
    // label-picker data.
    pub issue_fetch: crate::issue::IssueFetch,
    pub issue_action_runner: crate::issue_action::IssueActionRunner,
    pub issue_list: Option<IssueListView>,
    pub issue_detail: Option<IssueDetailView>,
    pub issue_editor: crate::text_editor::TextEditor,
    pub issue_label_picker: Option<IssueLabelPicker>,
    pub issue_label_filter: Option<IssueLabelFilter>,

    // A pending request (set by a compose handler on Ctrl+E) to pop the compose
    // buffer out into the user's $EDITOR. `main.rs` — the sole owner of the
    // terminal — drains this after `handle_action`, suspends the TUI, runs the
    // editor, and restores. Kept off the terminal-owning path so headless/debug
    // runs never try to suspend a real terminal.
    pub pending_external_edit: Option<crate::external_edit::ExternalEditTarget>,

    // Author avatars (pixel mode): background downloader + the graph generation
    // whose author emails have already been enqueued (re-enqueue on reload).
    pub avatar_fetch: crate::avatar_fetch::AvatarFetch,
    pub avatar_enqueued_generation: Option<u64>,

    // Filesystem watcher
    pub watcher: Option<crate::watcher::FsWatcher>,
    // Watcher still being built on a background thread; installed into
    // `watcher` by poll_fs_watcher once ready.
    pub pending_watcher: Option<crate::watcher::PendingFsWatcher>,
    // Set when a previously-running watcher disconnects mid-session (see
    // `poll_fs_watcher`'s `PollResult::Disconnected` arm). Distinguishes that
    // failure from `watcher` simply never having started (construction
    // failed, or disabled by config) — only the former is worth a status-bar
    // warning chip, since the latter isn't a regression the user needs to
    // act on.
    pub watcher_disconnected: bool,
    /// Set when the on-disk `.git` refs/config may have changed under a
    /// long-lived libgit2 handle (a watcher tick that touched `.git`), so the
    /// next `refresh` re-opens the repo to observe them. Gates the reopen so a
    /// working-tree-only watcher tick (or a quiet auto-refresh timer while the
    /// watcher is alive) doesn't pay to re-open every time. Cleared on a
    /// successful reopen; `force` refreshes and a missing watcher reopen
    /// unconditionally, independent of this flag.
    pub repo_dirty: bool,

    // Undo
    pub last_undoable_op: Option<UndoableOperation>,

    // Layout
    pub side_panel_layout: bool,

    /// When true, remote-only branches (remote refs with no matching local
    /// branch) are hidden from the graph — their labels and their exclusive
    /// commits. Composes with `hidden_branches`. Persisted in `UiState`.
    pub hide_remote_branches: bool,

    /// When true, stash entries (and any commits reachable only as stash
    /// parents) are hidden from the graph. Persisted in `UiState`.
    pub hide_stashes: bool,

    // Which metadata columns (author/hash/date) render on each commit row.
    pub metadata_columns: crate::config::MetadataColumns,

    // User cap on the graph column width, in cells. None = uncapped (fit all
    // lanes). Trims wasted padding from a wide region far back in history.
    pub graph_width_cap: Option<usize>,

    // Debug mode
    pub debug_keys: bool,

    /// When the CapsLock hint toast last fired (#106), for rate-limiting —
    /// not persisted, purely a within-session cooldown.
    pub last_capslock_hint: Option<std::time::Instant>,

    // Performance counters. Recorded on the render/refresh paths; a summary is
    // logged on exit (only visible with --log-file).
    pub perf: crate::perf::PerfStats,

    // Mouse: panel rectangles recorded each frame for hit-testing, plus the
    // last left click (for double-click detection).
    pub mouse_layout: MouseLayout,
    pub last_click: Option<crate::mouse::LastClick>,
    /// Files-pane scroll offset from the last render, for mouse hit-testing.
    pub files_view_offset: usize,
    /// When the commit menu was opened by right-click, its screen anchor;
    /// `None` = keyboard-opened (centered).
    pub menu_anchor: Option<(u16, u16)>,
    /// Rect of the currently open popup, recorded each frame for click-outside
    /// detection and in-popup row hit-testing.
    pub popup_rect: Option<ratatui::layout::Rect>,
    /// Clickable chip regions per graph row (indexed by filtered row position),
    /// recorded each frame for badge/branch-chip clicks.
    pub graph_chip_hits: Vec<Vec<crate::mouse::ChipHit>>,
    /// Clickable status-bar key hints: each hint's absolute cell rect paired with
    /// the `Action` pressing its key would dispatch. Rebuilt every frame by the
    /// status bar so a click reflects the current mode/panel's hints.
    pub status_hints: Vec<(ratatui::layout::Rect, Action)>,
    /// Graph-pane share of the graph/detail split, as a percentage (20–80).
    pub graph_split_ratio: u16,
    /// Whether the divider between graph and detail is being dragged.
    pub dragging_divider: bool,
    /// Branch tracing: when on (and the graph is branchy), the selected commit's
    /// lineage renders at full strength and other lanes are dimmed.
    pub trace_enabled: bool,

    // Config
    pub config: Config,

    // Terminal background color (r, g, b), detected once at startup.
    // Used to derive theme-adaptive structural colors. `None` when the
    // terminal doesn't report it (e.g. headless tests).
    pub terminal_bg: Option<(u8, u8, u8)>,

    // Pixel-rendered graph state. `Some` only when a graphics protocol was
    // detected at startup and pixel rendering is enabled; `None` in tests and
    // when falling back to Unicode glyphs.
    pub pixel_graph: Option<crate::ui::graph_pixels::PixelGraphState>,

    // Cached UNDIMMED pixel-graph base row specs, valid while (graph_generation,
    // commit_filter, panel_available, graph_width) are unchanged. Theme is stable
    // at runtime, so it's not part of the key; the graph_width component captures
    // the resize cap. Branch-trace dimming is layered on lazily per frame over
    // just the on-screen window (see `dim_pixel_specs_window`), so the traced
    // selection is deliberately NOT part of this key — moving the selection with
    // tracing on reuses this cache instead of rebuilding every row's geometry.
    // Rebuilt lazily by the render pre-pass.
    pub pixel_specs_cache: Option<PixelSpecsCache>,

    // Branch-trace frame cache: lineage, lit edges, and resolved lane colors
    // for the traced selection. Valid while (graph_generation, selected index)
    // are unchanged; rebuilding these is O(total loaded commits), so doing it
    // per draw (the old behavior) made every keypress scale with history size
    // while tracing was on. Refreshed by `ensure_trace_cache` at the top of
    // each draw; cleared by the theme setter (lane colors bake the theme in).
    pub trace_cache: Option<TraceCache>,
}

/// Frame-cached branch-trace state (see `App::trace_cache`). `lineage` empty
/// means "tracing is on but the selection has no lineage" — consumers treat
/// that as trace-off for rendering, and the cache still prevents a per-draw
/// recompute.
pub struct TraceCache {
    generation: u64,
    selected: usize,
    /// Selected commit's lineage (ancestors ∪ descendants), the full-strength set.
    pub lineage: std::collections::HashSet<Oid>,
    /// Lit cell edges derived from `lineage` (see `trace_lit_edges`).
    pub lit: std::collections::HashMap<crate::git::graph::CellEdge, Oid>,
    /// Lineage commit → its lane color resolved through the active theme, for
    /// recoloring lit strokes without a per-frame full-graph scan.
    pub lane_rgb: std::collections::HashMap<Oid, [u8; 3]>,
}

impl TraceCache {
    /// The cache as an active trace: `Some` only when the lineage is non-empty.
    pub fn active(&self) -> Option<&TraceCache> {
        (!self.lineage.is_empty()).then_some(self)
    }
}

/// Cached undimmed pixel base row specs plus the key they were built for:
/// `(graph_generation, commit_filter, panel_available, graph_width, specs)`.
pub type PixelSpecsCache = (
    u64,
    String,
    u16,
    u16,
    Vec<crate::ui::graph_pixels::RowSpec>,
);

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
    /// The compose text a pending external edit should hand to the editor.
    pub fn external_edit_source_text(
        &self,
        target: crate::external_edit::ExternalEditTarget,
    ) -> String {
        match target {
            crate::external_edit::ExternalEditTarget::Pr => self.pr_editor.text.clone(),
            crate::external_edit::ExternalEditTarget::Issue => self.issue_editor.text.clone(),
        }
    }

    /// Replace the targeted compose buffer with the editor's output, cursor at
    /// end. Called by `main.rs` after the external editor returns successfully.
    pub fn apply_external_edit(
        &mut self,
        target: crate::external_edit::ExternalEditTarget,
        text: String,
    ) {
        let mut editor = crate::text_editor::TextEditor::from_text(&text);
        editor.move_text_end(false);
        match target {
            crate::external_edit::ExternalEditTarget::Pr => self.pr_editor = editor,
            crate::external_edit::ExternalEditTarget::Issue => self.issue_editor = editor,
        }
    }

    pub fn handle_action(&mut self, action: Action) -> Result<()> {
        // Ctrl+Q always quits
        if matches!(action, Action::ForceQuit) {
            self.should_quit = true;
            return Ok(());
        }
        if matches!(action, Action::ToggleLayout) {
            self.side_panel_layout = !self.side_panel_layout;
            self.save_ui_state();
            return Ok(());
        }
        if matches!(action, Action::ToggleDebugKeys) {
            self.debug_keys = !self.debug_keys;
            self.toast(crate::toast::ToastKind::Info, if self.debug_keys {
                "Debug keys ON"
            } else {
                "Debug keys OFF"
            });
            return Ok(());
        }
        // F5 full update works from any panel/sub-state; the keybinding layer
        // only emits it in Normal mode.
        if matches!(action, Action::FullUpdate) {
            self.full_update();
            return Ok(());
        }
        // The command palette opens from any panel in Normal mode.
        if matches!(action, Action::OpenCommandPalette) {
            self.open_command_palette();
            return Ok(());
        }
        // The settings menu opens from any panel in Normal mode.
        if matches!(action, Action::OpenSettings) {
            self.open_settings();
            return Ok(());
        }
        // Mouse actions hit-test the recorded layout regardless of mode.
        if matches!(
            action,
            Action::MouseClick { .. }
                | Action::MouseRightClick { .. }
                | Action::MouseScroll { .. }
                | Action::MouseDrag { .. }
                | Action::MouseUp { .. }
        ) {
            self.handle_mouse_action(action);
            return Ok(());
        }

        match &self.mode {
            AppMode::Normal => self.handle_normal_action(action)?,
            AppMode::Help => self.handle_help_action(action),
            AppMode::Input { .. } => self.handle_input_action(action)?,
            AppMode::Confirm { .. } => self.handle_confirm_action(action)?,
            AppMode::CommitMenu { .. } => self.handle_commit_menu_action(action)?,
            AppMode::MetadataMenu { .. } => self.handle_metadata_menu_action(action),
            AppMode::Settings { .. } => self.handle_settings_action(action)?,
            AppMode::PullDivergence { .. } => self.handle_pull_divergence_action(action),
            AppMode::CiChecks => self.handle_ci_checks_action(action),
            AppMode::PrThread => self.handle_pr_thread_action(action),
            AppMode::PrCompose { .. } => self.handle_pr_compose_action(action),
            AppMode::PrMergePicker { .. } => self.handle_pr_merge_picker_action(action),
            AppMode::PrReviewPicker { .. } => self.handle_pr_review_picker_action(action),
            AppMode::IssueList => self.handle_issue_list_action(action),
            AppMode::IssueDetail => self.handle_issue_detail_action(action),
            AppMode::IssueCompose { .. } => self.handle_issue_compose_action(action),
            AppMode::IssueLabelPicker { .. } => self.handle_issue_label_picker_action(action),
            AppMode::IssueLabelFilter { .. } => self.handle_issue_label_filter_action(action),
            AppMode::BranchPicker { .. } => self.handle_branch_picker_action(action)?,
            AppMode::BranchDeletePicker { .. } => self.handle_branch_delete_picker_action(action)?,
            AppMode::TagPicker { .. } => self.handle_tag_picker_action(action)?,
            AppMode::RemotePicker { .. } => self.handle_remote_picker_action(action)?,
            AppMode::BranchFilter { .. } => self.handle_branch_filter_action(action)?,
            AppMode::FileDiff { .. } => self.handle_file_diff_action(action)?,
            AppMode::FileHistory { .. } => self.handle_file_history_action(action)?,
            AppMode::CommandPalette { .. } => self.handle_command_palette_action(action)?,
        }
        Ok(())
    }

    /// Report a one-shot error as a red toast (#116): visible without blocking
    /// input, long-TTL so it isn't missed, dismissible early with Esc. Never a
    /// modal — the status bar stays reserved for sticky state (conflicts,
    /// latched background-check errors), and no error may lock the UI.
    pub fn show_error(&mut self, message: String) {
        self.toast(crate::toast::ToastKind::Error, message);
    }

    /// Report a failed post-op refresh with a consistent message and severity.
    /// Used after ops (fetch/push/pull/prune/PR actions/…) whose own outcome
    /// already succeeded but whose follow-up `refresh()` failed.
    pub fn report_refresh_error(&mut self, e: impl std::fmt::Display) {
        self.show_error(format!("Refresh failed: {e}"));
    }

    /// Enqueue this graph's author emails for avatar download (once per graph
    /// load) and drain any finished downloads. Returns whether a new avatar
    /// arrived, so the caller can trigger a redraw. Cheap no-op when avatars are
    /// toggled off.
    pub fn update_avatars(&mut self) -> bool {
        if !self.metadata_columns.avatars {
            return false;
        }
        if self.avatar_enqueued_generation != Some(self.graph_generation) {
            self.avatar_enqueued_generation = Some(self.graph_generation);
            // Collect first so the immutable borrow of `graph_layout` doesn't
            // overlap the mutable `avatar_fetch` calls.
            let emails: Vec<String> = self
                .graph_layout
                .nodes
                .iter()
                .filter_map(|n| n.commit.as_ref().map(|c| c.author_email.clone()))
                .collect();
            for email in &emails {
                self.avatar_fetch.request(email);
            }
        }
        self.avatar_fetch.poll()
    }

    /// Whether branch tracing is currently in effect: enabled by the user and
    /// the graph is branchy enough (> 2 lanes) to benefit.
    pub(crate) fn trace_active(&self) -> bool {
        self.trace_enabled && crate::git::graph::graph_has_enough_lanes(&self.graph_layout)
    }

    /// Bring `trace_cache` up to date for this frame: `Some` iff tracing is
    /// active with a selection, keyed by `(graph_generation, selected index)`.
    /// A hit is free; a miss does the O(graph) lineage / lit-edge / lane-color
    /// builds exactly once per selection move instead of once per draw. Called
    /// at the top of `ui::draw`, before any renderer reads the cache.
    pub(crate) fn ensure_trace_cache(&mut self) {
        if !self.trace_active() {
            self.trace_cache = None;
            return;
        }
        let Some(sel) = self.graph_nav.graph_list_state.selected() else {
            self.trace_cache = None;
            return;
        };
        if self
            .trace_cache
            .as_ref()
            .is_some_and(|c| c.generation == self.graph_generation && c.selected == sel)
        {
            return;
        }
        let lineage = crate::git::graph::lineage_oids(&self.graph_layout, sel);
        let lit = crate::git::graph::trace_lit_edges(&self.graph_layout, &lineage);
        let theme = self.theme();
        let lane_rgb = self
            .graph_layout
            .nodes
            .iter()
            .filter_map(|n| {
                n.commit
                    .as_ref()
                    .filter(|c| lineage.contains(&c.oid))
                    .map(|c| {
                        let rgb = crate::ui::graph_pixels::color_to_rgb(
                            theme.lane_color(n.color_index),
                        );
                        (c.oid, rgb)
                    })
            })
            .collect();
        self.trace_cache = Some(TraceCache {
            generation: self.graph_generation,
            selected: sel,
            lineage,
            lit,
            lane_rgb,
        });
    }

    /// The active trace for rendering: the frame cache when tracing is on and
    /// the selection has a non-empty lineage. Read-only view over
    /// `ensure_trace_cache`'s result.
    pub(crate) fn active_trace(&self) -> Option<&TraceCache> {
        self.trace_cache.as_ref().and_then(TraceCache::active)
    }

    /// Toggle branch tracing (persisted). A no-op visually on linear graphs,
    /// which never trace, but the preference is still saved.
    pub(crate) fn toggle_trace(&mut self) {
        self.trace_enabled = !self.trace_enabled;
        self.save_ui_state();
        let state = if self.trace_enabled { "on" } else { "off" };
        self.toast(crate::toast::ToastKind::Info, format!("Branch tracing {state}"));
    }

    /// Toggle whether remote-only branches are shown in the graph (persisted).
    /// Rebuilds the graph so their exclusive commits appear/disappear, not just
    /// their labels. Composes with the per-branch filter.
    pub(crate) fn toggle_remote_branches(&mut self) -> Result<()> {
        self.hide_remote_branches = !self.hide_remote_branches;
        self.save_ui_state();
        self.refresh(true)?;
        let state = if self.hide_remote_branches {
            "hidden"
        } else {
            "shown"
        };
        self.toast(crate::toast::ToastKind::Info, format!("Remote branches {state}"));
        Ok(())
    }

    /// Toggle whether merged branches are hidden from the graph (persisted).
    /// Rebuilds the graph so a squash-merged branch's now-dangling commits appear
    /// or disappear, not just its label. When shown, merged branches stay dimmed.
    pub(crate) fn toggle_merged_branches(&mut self) -> Result<()> {
        self.merged.hide = !self.merged.hide;
        self.save_ui_state();
        self.refresh(true)?;
        let state = if self.merged.hide {
            "hidden"
        } else {
            "dimmed"
        };
        self.set_message(format!("Merged branches {state}"));
        Ok(())
    }

    /// Hand the current branch/base/PR state to the background classifier, which
    /// recomputes `merged_branches` off the UI thread (ancestry + bounded patch-id
    /// scans per branch). Idempotent when inputs are unchanged — the classifier's
    /// signature guard skips redundant work — so it's safe to call every refresh.
    /// The result is applied later by [`Self::update_merged_classification`].
    pub(crate) fn kick_merged_classification(&mut self) {
        let Some(base) = crate::git::merged::base_branch(&self.branches) else {
            // No base branch to measure against → nothing can be merged.
            self.merged.branches.clear();
            return;
        };
        let input = crate::merged_branch_fetch::ClassifyInput {
            repo_path: self.repo_path.clone(),
            branches: self.branches.clone(),
            base_name: base.name.clone(),
            base_tip: base.tip_oid,
            gh_merged: self.merged.pr_branches.clone(),
        };
        self.merged.classify.maybe_start(input);
    }

    /// The squash-link edges to feed [`build_graph`]: `(branch_tip, squash_commit)`
    /// for each squash-merged branch, but only when the `squash_link_lines` option
    /// is on. Empty (option off) leaves the layout byte-identical. Each branch
    /// name is resolved to its current tip OID; `build_graph` further guards that
    /// both endpoints are loaded before drawing anything (issue #81).
    pub(crate) fn squash_link_edges(&self) -> Vec<(git2::Oid, git2::Oid)> {
        if !self.config.ui.squash_link_lines {
            return Vec::new();
        }
        self.merged
            .squash_targets
            .iter()
            .filter_map(|(name, &target)| {
                let tip = self.branches.iter().find(|b| &b.name == name)?.tip_oid;
                Some((tip, target))
            })
            .collect()
    }

    /// Recompute the set of base-update ("back-merge") commits (issue #55) from
    /// the current open PRs and base branch, storing it in `base_update_merges`.
    /// Cheap (bounded per-PR walks on the UI thread's repo handle) and guarded by
    /// an input signature so a frequent refresh does no work when neither the
    /// base tip nor the open-PR head set has changed. Safe to call every refresh
    /// and whenever `open_prs` updates; the render path only *reads* the set.
    pub(crate) fn recompute_base_update_merges(&mut self) {
        let Some(base) = crate::git::merged::base_branch(&self.branches) else {
            self.merged.base_update.reset(std::collections::HashSet::new());
            return;
        };
        // Sorted PR head OIDs so the signature is order-independent.
        let mut pr_heads: Vec<git2::Oid> =
            self.open_prs.values().filter_map(|p| p.head_oid()).collect();
        pr_heads.sort();
        // Signature: base tip + the (sorted) head set. Unchanged ⇒ result is
        // identical, so the guard skips the ancestry walks below.
        let repo = self.repo.repo();
        self.merged.base_update
            .recompute_if_changed((base.tip_oid, &pr_heads), || {
                if pr_heads.is_empty() {
                    std::collections::HashSet::new()
                } else {
                    crate::git::merged::classify_base_update_merges(repo, &pr_heads, base.tip_oid)
                }
            });
    }

    /// Toggle soft line-wrapping in the file-diff viewer (persisted). The next
    /// render re-lays-out the diff via `ensure_diff_layout`, so the toggle only
    /// flips the flag and reports the new state.
    pub(crate) fn toggle_diff_word_wrap(&mut self) {
        self.diff_word_wrap = !self.diff_word_wrap;
        self.save_ui_state();
        let state = if self.diff_word_wrap { "on" } else { "off" };
        self.toast(crate::toast::ToastKind::Info, format!("Line wrap {state}"));
    }

    /// Re-lay-out the file-diff viewer's rendered lines when the wrap toggle or
    /// the pane width has changed since the last layout. Cheap no-op otherwise.
    /// Called from the renderer before it borrows the diff state, so scrolling,
    /// the scrollbar, and hunk navigation all see wrapped-row coordinates.
    pub(crate) fn ensure_diff_layout(&mut self) {
        let wrap = self.diff_word_wrap;
        let width = self.diff_viewport_width as usize;
        let viewport = self.diff_viewport_height as usize;

        let Some(source) = self.diff_source.as_mut() else {
            return;
        };
        // Width only affects the wrapped layout; when wrap is off a width change
        // is irrelevant, so skip re-laying-out on every resize.
        let width_matters = wrap && source.layout_width != width;
        if source.layout_wrap == wrap && !width_matters {
            return;
        }
        let (lines, hunks) = crate::ui::file_diff_view::layout_diff_rows(
            &source.rows,
            &source.hunk_positions,
            wrap,
            width,
        );
        source.layout_wrap = wrap;
        source.layout_width = width;

        if let AppMode::FileDiff {
            rendered_lines,
            hunk_positions,
            scroll_offset,
            horizontal_offset,
            max_line_width,
            total_lines,
            ..
        } = &mut self.mode
        {
            *max_line_width = lines.iter().map(|l| l.width()).max().unwrap_or(0);
            *total_lines = lines.len();
            *rendered_lines = lines;
            *hunk_positions = hunks;
            // Wrapping changes the row count, so keep the viewport in range and
            // reset horizontal pan (wrapped lines never overflow the width).
            *scroll_offset = (*scroll_offset).min(total_lines.saturating_sub(viewport));
            if wrap {
                *horizontal_offset = 0;
            }
        }
    }

    /// Persist the state.toml settings. Built generically from the settings
    /// registry: every descriptor whose store is `State` writes its live value
    /// into the `UiState` field its lens points at, so this stays in lockstep
    /// with the menu with no hand-copied field list.
    pub(crate) fn save_ui_state(&self) {
        use crate::settings::{descriptors, SettingStore};
        let mut ui = UiState::default();
        for d in descriptors() {
            if let SettingStore::State { write, .. } = d.store {
                write(&mut ui, d.get(self));
            }
        }
        ui.save();
    }

    /// Metadata-columns toggle menu: navigate, toggle (persisting), close.
    fn handle_metadata_menu_action(&mut self, action: Action) {
        use crate::config::MetadataColumn;
        let n = MetadataColumn::ALL.len();
        match action {
            Action::MoveUp => {
                if let AppMode::MetadataMenu { selected } = &mut self.mode {
                    *selected = (*selected + n - 1) % n;
                }
            }
            Action::MoveDown => {
                if let AppMode::MetadataMenu { selected } = &mut self.mode {
                    *selected = (*selected + 1) % n;
                }
            }
            Action::MenuSelect => {
                let idx = match &self.mode {
                    AppMode::MetadataMenu { selected } => *selected,
                    _ => return,
                };
                self.metadata_columns.toggle(MetadataColumn::ALL[idx]);
                self.save_ui_state();
            }
            Action::Cancel => self.mode = AppMode::Normal,
            _ => {}
        }
    }

    /// Snapshot every setting's current value, ordered to match
    /// `settings::descriptors()`, for the settings menu to render. The menu
    /// edits the live app state directly (see `commit_setting`), so this is a
    /// read-only projection with no inverse to keep in sync.
    pub(crate) fn settings_snapshot(&self) -> Vec<crate::settings::SettingValue> {
        crate::settings::descriptors()
            .iter()
            .map(|d| d.get(self))
            .collect()
    }

    /// Persist both stores the settings menu writes to: UI-state settings to
    /// state.toml, config settings to config.toml (comments/unknown keys kept).
    pub(crate) fn persist_settings(&self) {
        self.save_ui_state();
        self.config.save();
    }

    /// Open the settings menu (Ctrl+,).
    pub(crate) fn open_settings(&mut self) {
        self.mode = AppMode::Settings {
            selected: 0,
            editing: None,
            query: String::new(),
        };
    }

    /// Write one setting's new value to the live app state, persist it, and
    /// rebuild the graph if the change was one a bare field write can't realize
    /// on its own — the branch/stash visibility toggles (`hide_remote_branches` /
    /// `hide_merged_branches` / `hide_stashes`), which change which commits are
    /// in the graph.
    fn commit_setting(
        &mut self,
        descriptor: &crate::settings::SettingDescriptor,
        value: crate::settings::SettingValue,
    ) -> Result<()> {
        let old_hide_remote = self.hide_remote_branches;
        let old_hide_merged = self.merged.hide;
        let old_hide_stashes = self.hide_stashes;
        let old_squash_links = self.config.ui.squash_link_lines;
        descriptor.set(self, value);
        self.persist_settings();
        if self.hide_remote_branches != old_hide_remote
            || self.merged.hide != old_hide_merged
            || self.hide_stashes != old_hide_stashes
            || self.config.ui.squash_link_lines != old_squash_links
        {
            // These change which commits (or synthetic link lines) the graph
            // contains, which a bare field write can't realize — rebuild it.
            self.refresh(true)?;
        }
        Ok(())
    }

    /// Settings menu: navigate, toggle/cycle, type numeric values, fuzzy-filter
    /// by label, close. Edits apply live and persist immediately.
    ///
    /// `selected` is always a real (absolute) index into `descriptors()`, even
    /// while a filter is active — navigation and typing keep it synced to the
    /// currently-visible subset (`crate::settings::filter_descriptors`), the
    /// same "position lookup within a visible-index list" pattern the graph
    /// panel's commit filter uses.
    fn handle_settings_action(&mut self, action: Action) -> Result<()> {
        use crate::settings::{
            clamp_int, cycle_value, descriptors, filter_descriptors, SettingKind, SettingValue,
        };

        let ds = descriptors();
        let (selected, editing, query) = match &self.mode {
            AppMode::Settings {
                selected,
                editing,
                query,
            } => (*selected, editing.clone(), query.clone()),
            _ => return Ok(()),
        };
        let visible = filter_descriptors(&ds, &query);

        match action {
            Action::MoveUp => {
                if !visible.is_empty() {
                    let pos = visible.iter().position(|&i| i == selected).unwrap_or(0);
                    let new_pos = (pos + visible.len() - 1) % visible.len();
                    if let AppMode::Settings {
                        selected, editing, ..
                    } = &mut self.mode
                    {
                        *selected = visible[new_pos];
                        *editing = None;
                    }
                }
            }
            Action::MoveDown => {
                if !visible.is_empty() {
                    let pos = visible.iter().position(|&i| i == selected).unwrap_or(0);
                    let new_pos = (pos + 1) % visible.len();
                    if let AppMode::Settings {
                        selected, editing, ..
                    } = &mut self.mode
                    {
                        *selected = visible[new_pos];
                        *editing = None;
                    }
                }
            }
            Action::MenuSelect => {
                // Only act on a setting that's actually shown under the
                // current filter (guards against a stale selection).
                if visible.contains(&selected) {
                    let d = &ds[selected];
                    match (&editing, d.kind) {
                        // Commit a typed numeric value.
                        (Some(buf), SettingKind::Int { .. }) => {
                            let parsed = buf.parse::<u64>().ok();
                            if let AppMode::Settings { editing, .. } = &mut self.mode {
                                *editing = None;
                            }
                            if let Some(parsed) = parsed {
                                let value = SettingValue::Int(clamp_int(&d.kind, parsed));
                                self.commit_setting(d, value)?;
                            }
                        }
                        // Otherwise cycle/toggle the current value.
                        _ => {
                            let next = cycle_value(&d.kind, d.get(self));
                            self.commit_setting(d, next)?;
                        }
                    }
                }
            }
            // A digit starts (or continues) an inline numeric edit only when
            // no filter is active yet and the selected setting takes a
            // number — the pre-existing shortcut. Once a filter is active
            // (or the setting isn't numeric), typing always wins: digits
            // fall through to the catch-all arm below and filter like any
            // other character.
            Action::InputChar(c)
                if c.is_ascii_digit()
                    && (editing.is_some()
                        || (query.is_empty()
                            && matches!(ds[selected].kind, SettingKind::Int { .. }))) =>
            {
                if let AppMode::Settings { editing, .. } = &mut self.mode {
                    let buf = editing.get_or_insert_with(String::new);
                    // Cap length so absurd input can't overflow u64.
                    if buf.len() < 7 {
                        buf.push(c);
                    }
                }
            }
            // Everything else (letters, and digits once filtering or off a
            // numeric setting) types into the filter query. This also cancels
            // an in-progress numeric edit — typing a non-digit mid-edit means
            // the user's switched to searching, not finishing the number.
            Action::InputChar(c) => {
                let mut new_query = query;
                new_query.push(c);
                self.apply_settings_query(new_query);
            }
            Action::InputBackspace => {
                if let AppMode::Settings {
                    editing: Some(buf), ..
                } = &mut self.mode
                {
                    buf.pop();
                    if buf.is_empty() {
                        if let AppMode::Settings { editing, .. } = &mut self.mode {
                            *editing = None;
                        }
                    }
                } else if !query.is_empty() {
                    let mut new_query = query;
                    new_query.pop();
                    self.apply_settings_query(new_query);
                }
            }
            Action::InputBackspaceWord if editing.is_none() && !query.is_empty() => {
                let mut new_query = query;
                crate::text_editor::pop_word(&mut new_query);
                self.apply_settings_query(new_query);
            }
            Action::InputClearLine if editing.is_none() && !query.is_empty() => {
                self.apply_settings_query(String::new());
            }
            Action::Cancel => {
                // Esc unwinds one layer at a time: cancel an in-progress
                // numeric edit first, else clear an active filter, else
                // close the menu.
                if editing.is_some() {
                    if let AppMode::Settings { editing, .. } = &mut self.mode {
                        *editing = None;
                    }
                } else if !query.is_empty() {
                    self.apply_settings_query(String::new());
                } else {
                    self.mode = AppMode::Normal;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Apply a new settings-menu filter query: if the current selection fell
    /// out of the now-visible subset, jump to the first visible item (mirrors
    /// `BranchFilter`'s "reset selection on filter change"). Also cancels any
    /// in-progress numeric edit, since filtering and editing are exclusive.
    fn apply_settings_query(&mut self, new_query: String) {
        use crate::settings::{descriptors, filter_descriptors};
        let ds = descriptors();
        let visible = filter_descriptors(&ds, &new_query);
        if let AppMode::Settings {
            selected,
            editing,
            query,
        } = &mut self.mode
        {
            *editing = None;
            if !visible.contains(selected) {
                if let Some(&first) = visible.first() {
                    *selected = first;
                }
            }
            *query = new_query;
        }
    }

    /// Pull-divergence prompt: pick merge (0) or rebase (1), or cancel. The
    /// choice reruns the pull with that strategy through the normal async path.
    fn handle_pull_divergence_action(&mut self, action: Action) {
        const N: usize = 2; // Merge, Rebase
        match action {
            Action::MoveUp => {
                if let AppMode::PullDivergence { selected } = &mut self.mode {
                    *selected = (*selected + N - 1) % N;
                }
            }
            Action::MoveDown => {
                if let AppMode::PullDivergence { selected } = &mut self.mode {
                    *selected = (*selected + 1) % N;
                }
            }
            Action::MenuSelect => {
                let idx = match &self.mode {
                    AppMode::PullDivergence { selected } => *selected,
                    _ => return,
                };
                self.mode = AppMode::Normal;
                let mode = if idx == 0 {
                    PullMode::Merge
                } else {
                    PullMode::Rebase
                };
                self.rerun_pull_with_mode(mode);
            }
            Action::Cancel => self.mode = AppMode::Normal,
            _ => {}
        }
    }

    fn handle_help_action(&mut self, action: Action) {
        if matches!(action, Action::ToggleHelp | Action::Quit | Action::Cancel) {
            self.mode = AppMode::Normal;
        }
    }

}

#[cfg(test)]
mod settings_menu_filter_tests {
    use super::*;
    use crate::test_support::git;

    fn test_app() -> (tempfile::TempDir, App) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("f.txt"), "base\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "initial"]);
        let repo = GitRepository::open(dir).expect("open repo");
        let app = App::from_repo(repo).expect("build app");
        (tmp, app)
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_action(Action::InputChar(c)).expect("type char");
        }
    }

    fn settings_query(app: &App) -> &str {
        match &app.mode {
            AppMode::Settings { query, .. } => query,
            other => panic!("expected AppMode::Settings, got {other:?}"),
        }
    }

    fn settings_selected(app: &App) -> usize {
        match &app.mode {
            AppMode::Settings { selected, .. } => *selected,
            other => panic!("expected AppMode::Settings, got {other:?}"),
        }
    }

    #[test]
    fn typing_narrows_selection_to_the_fuzzy_matching_subset() {
        let (_tmp, mut app) = test_app();
        app.open_settings();
        type_str(&mut app, "dim");

        assert_eq!(settings_query(&app), "dim");
        let ds = crate::settings::descriptors();
        let visible = crate::settings::filter_descriptors(&ds, "dim");
        assert!(!visible.is_empty(), "test fixture: 'dim' must match something");
        assert!(
            visible.contains(&settings_selected(&app)),
            "selection must land inside the filtered subset"
        );
        assert_eq!(ds[settings_selected(&app)].label, "Dim merged branches");
    }

    #[test]
    fn empty_query_leaves_every_descriptor_reachable() {
        let (_tmp, mut app) = test_app();
        app.open_settings();
        let ds = crate::settings::descriptors();
        assert_eq!(settings_query(&app), "");
        // Walking MoveDown the full length must visit every descriptor
        // exactly once before wrapping — i.e. nothing is pre-filtered.
        let mut seen = std::collections::HashSet::new();
        seen.insert(settings_selected(&app));
        for _ in 1..ds.len() {
            app.handle_action(Action::MoveDown).unwrap();
            seen.insert(settings_selected(&app));
        }
        assert_eq!(seen.len(), ds.len());
    }

    #[test]
    fn no_match_yields_an_empty_list_and_navigation_is_a_no_op() {
        let (_tmp, mut app) = test_app();
        app.open_settings();
        type_str(&mut app, "zzzqqqxxx_no_such_setting");
        let before = settings_selected(&app);
        app.handle_action(Action::MoveDown).unwrap();
        app.handle_action(Action::MoveUp).unwrap();
        assert_eq!(settings_selected(&app), before, "no visible rows to move between");
    }

    #[test]
    fn navigation_clamps_within_the_filtered_subset() {
        let (_tmp, mut app) = test_app();
        app.open_settings();
        // "e" matches many settings; the point is just that every step must
        // stay inside the filtered set — never park on a filtered-out row.
        type_str(&mut app, "e");
        let ds = crate::settings::descriptors();
        let visible = crate::settings::filter_descriptors(&ds, "e");
        assert!(visible.len() > 1, "test fixture needs >1 match to prove clamping");
        for _ in 0..(visible.len() * 2 + 1) {
            app.handle_action(Action::MoveDown).unwrap();
            assert!(visible.contains(&settings_selected(&app)));
        }
        for _ in 0..(visible.len() * 2 + 1) {
            app.handle_action(Action::MoveUp).unwrap();
            assert!(visible.contains(&settings_selected(&app)));
        }
    }

    #[test]
    fn menu_select_toggles_the_currently_filtered_setting() {
        let (_tmp, mut app) = test_app();
        let before = app.merged.dim;
        app.open_settings();
        type_str(&mut app, "Dim merged"); // unique match: "Dim merged branches"
        assert_eq!(
            crate::settings::descriptors()[settings_selected(&app)].label,
            "Dim merged branches"
        );
        app.handle_action(Action::MenuSelect).unwrap();
        assert_eq!(app.merged.dim, !before, "MenuSelect must act on the filtered selection");
    }

    #[test]
    fn esc_first_clears_the_query_then_closes_the_menu() {
        let (_tmp, mut app) = test_app();
        app.open_settings();
        type_str(&mut app, "dim");
        assert_eq!(settings_query(&app), "dim");

        app.handle_action(Action::Cancel).unwrap();
        assert_eq!(
            settings_query(&app),
            "",
            "first Esc clears the query but stays open"
        );
        assert!(matches!(app.mode, AppMode::Settings { .. }));

        app.handle_action(Action::Cancel).unwrap();
        assert!(
            matches!(app.mode, AppMode::Normal),
            "second Esc (now with an empty query) closes the menu"
        );
    }

    #[test]
    fn esc_with_no_query_closes_immediately() {
        let (_tmp, mut app) = test_app();
        app.open_settings();
        app.handle_action(Action::Cancel).unwrap();
        assert!(matches!(app.mode, AppMode::Normal));
    }

    #[test]
    fn backspace_pops_the_query_one_character_at_a_time() {
        let (_tmp, mut app) = test_app();
        app.open_settings();
        type_str(&mut app, "dim");
        app.handle_action(Action::InputBackspace).unwrap();
        assert_eq!(settings_query(&app), "di");
        app.handle_action(Action::InputBackspace).unwrap();
        app.handle_action(Action::InputBackspace).unwrap();
        assert_eq!(settings_query(&app), "");
        // Backspacing an already-empty query is a no-op, not a close.
        app.handle_action(Action::InputBackspace).unwrap();
        assert_eq!(settings_query(&app), "");
        assert!(matches!(app.mode, AppMode::Settings { .. }));
    }

    #[test]
    fn digit_starts_a_numeric_edit_only_while_the_query_is_empty() {
        let (_tmp, mut app) = test_app();
        app.open_settings();
        // Move onto an Int-kind setting ("Graph split ratio %").
        let ds = crate::settings::descriptors();
        let int_idx = ds
            .iter()
            .position(|d| matches!(d.kind, crate::settings::SettingKind::Int { .. }))
            .expect("fixture needs an Int setting");
        while settings_selected(&app) != int_idx {
            app.handle_action(Action::MoveDown).unwrap();
        }
        // With an empty query, a digit starts the legacy numeric-edit buffer.
        app.handle_action(Action::InputChar('4')).unwrap();
        match &app.mode {
            AppMode::Settings { editing, query, .. } => {
                assert_eq!(editing.as_deref(), Some("4"));
                assert_eq!(query, "");
            }
            other => panic!("expected Settings mode, got {other:?}"),
        }
        // A non-digit character mid-edit cancels the edit and starts filtering.
        app.handle_action(Action::InputChar('x')).unwrap();
        match &app.mode {
            AppMode::Settings { editing, query, .. } => {
                assert_eq!(*editing, None, "typing a letter must cancel the numeric edit");
                assert_eq!(query, "x");
            }
            other => panic!("expected Settings mode, got {other:?}"),
        }
    }

    #[test]
    fn digits_filter_once_a_query_is_already_active() {
        let (_tmp, mut app) = test_app();
        app.open_settings();
        type_str(&mut app, "e"); // start a non-empty text filter first
        app.handle_action(Action::InputChar('5')).unwrap();
        match &app.mode {
            AppMode::Settings { editing, query, .. } => {
                assert_eq!(*editing, None, "digit must not start a numeric edit mid-filter");
                assert_eq!(query, "e5");
            }
            other => panic!("expected Settings mode, got {other:?}"),
        }
    }
}
