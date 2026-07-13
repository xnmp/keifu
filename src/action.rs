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

    // Files
    OpenWithDefault,

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
