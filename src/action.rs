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
    MenuSelect,
    SelectAll,
    SelectNone,

    // Commit comparison (graph)
    MarkForCompare,

    // Open the selected commit's PR in the browser (graph)
    OpenPr,

    // Open the metadata-columns toggle menu (graph)
    OpenMetadataMenu,

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
}
