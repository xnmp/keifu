//! User action definitions

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    // Navigation
    MoveUp,
    MoveDown,
    PageUp,
    PageDown,
    GoToTop,
    GoToBottom,
    JumpToHead,
    NextBranch,
    PrevBranch,

    // Panel navigation
    PanelLeft,
    PanelRight,
    FocusGraph,
    ForceQuit,

    // Git operations
    Checkout,
    CreateBranch,
    DeleteBranch,
    Fetch,
    Pull,
    Push,
    Merge,
    Rebase,

    // Commit menu
    OpenCommitMenu,
    OpenBranchFilter,
    /// Show/hide all remote-only branches in the graph.
    ToggleRemoteBranches,
    MenuSelect,
    SelectAll,
    SelectNone,

    // Commit comparison (graph)
    MarkForCompare,

    // Open the command palette (Ctrl+P / ':') from Normal mode
    OpenCommandPalette,

    // Create / merge a pull request directly (palette shortcuts to the commit
    // menu's PR actions)
    CreatePullRequest,
    MergePullRequest,

    // Open the selected commit's PR in the browser (graph)
    OpenPr,

    // Open the CI checks detail popup for the selected commit's PR (graph)
    OpenCiChecks,

    // Open the PR conversation thread popup for the selected commit's PR (graph)
    OpenPrThread,

    // Open the review-disposition picker from the PR thread popup
    OpenReviewPicker,

    // Submit the PR-compose editor (create PR / review body)
    SubmitCompose,

    // Pop the current compose buffer out into the user's $EDITOR (Ctrl+E),
    // shared by the PR- and issue-compose modes.
    ExternalEdit,

    // GitHub Issues
    /// Open the issue list popup (Normal mode, any panel).
    OpenIssueList,
    /// Re-fetch the current issue list / detail.
    RefreshIssues,
    /// Cycle the list filter Open → Closed → All.
    CycleIssueFilter,
    /// Open the selected issue's detail popup.
    OpenIssueDetail,
    /// Start composing a new issue.
    NewIssue,
    /// Start composing a comment on the current issue.
    CommentOnIssue,
    /// Close an open issue / reopen a closed one (via Confirm).
    ToggleIssueState,
    /// Open the label picker for the current issue.
    EditIssueLabels,
    /// Edit the current issue's assignees (via Input).
    EditIssueAssignees,
    /// Toggle the label under the cursor in the label picker.
    ToggleIssueLabel,
    /// Open the current issue in the browser.
    OpenIssueInBrowser,

    // Open the metadata-columns toggle menu (graph)
    OpenMetadataMenu,

    // Jump to the merge base / fork point of the selection vs main (or HEAD) (graph)
    JumpToMergeBase,

    // Undo the last reversible graph operation (branch/tag delete, merge, pull,
    // rename) — graph scope. Distinct from the files-pane UndoLastFileOp.
    UndoLastOp,

    // Load the next chunk / all remaining commits (graph)
    LoadMoreCommits,
    LoadAllCommits,

    // Toggle branch tracing (highlight selected commit's lineage) (graph)
    ToggleTrace,

    // Shrink / widen the graph column width cap (graph)
    ShrinkGraphWidth,
    WidenGraphWidth,

    // Per-file history (files pane)
    FileHistory,

    // Files
    OpenWithDefault,
    CopyPath,

    // File operations
    RestoreFile,
    ToggleStage,
    StageAll,
    UnstageAll,
    AddToGitignore,
    ArchiveFile,
    TrashFile,
    UndoLastFileOp,
    ToggleFolderView,
    StartFilesFilter,
    FilesFilterChar(char),
    FilesFilterBackspace,

    // Merge-conflict resolution (files pane, when an operation is in progress)
    AcceptOurs,
    AcceptTheirs,
    ContinueOperation,
    AbortOperation,

    // Jump to next / previous conflicted file in the files pane (wrap-around)
    NextConflict,
    PrevConflict,

    // Commit filter (graph panel)
    StartCommitFilter,
    CommitFilterChar(char),
    CommitFilterBackspace,

    // Commit editor
    StartEditing,
    StopEditing,
    CommitChanges,
    AmendCommit,
    StashStaged,
    EditorChar(char),
    EditorNewline,
    EditorBackspace,
    EditorDelete,
    EditorLeft(bool),
    EditorRight(bool),
    EditorUp(bool),
    EditorDown(bool),
    EditorHome(bool),
    EditorEnd(bool),
    EditorWordLeft(bool),
    EditorWordRight(bool),
    EditorBackspaceWord,
    EditorDeleteWord,
    EditorKillLine,
    EditorTextStart(bool),
    EditorTextEnd(bool),

    // UI
    ToggleHelp,
    Search,
    Refresh,
    /// F5: fetch all remotes + force PR refetch + refresh (full update).
    FullUpdate,
    Quit,

    // Dialogs
    Confirm,
    Cancel,
    InputChar(char),
    InputBackspace,
    InputBackspaceWord,
    InputClearLine,

    // Search dropdown
    SearchSelectUp,
    SearchSelectDown,
    SearchSelectUpQuiet,   // Tab navigation (no graph jump)
    SearchSelectDownQuiet, // Tab navigation (no graph jump)

    // Mouse (coordinates in terminal cells; hit-testing lives on App)
    MouseClick { col: u16, row: u16 },
    MouseRightClick { col: u16, row: u16 },
    MouseScroll { col: u16, row: u16, down: bool },
    MouseDrag { col: u16, row: u16 },
    MouseUp { col: u16, row: u16 },

    // Layout
    ToggleLayout,

    // Debug
    ToggleDebugKeys,

    // File diff
    OpenFileDiff,
    ScrollUp,
    ScrollDown,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToTop,
    ScrollToBottom,
    ScrollLeft,
    ScrollRight,
    ScrollToLineStart,
    NextFile,
    PrevFile,
    NextHunk,
    PrevHunk,
    StageHunk,
    UnstageHunk,
    DiscardHunk,
    /// Toggle soft line-wrapping in the file-diff viewer (Ctrl+Alt+W).
    ToggleDiffWrap,
}
