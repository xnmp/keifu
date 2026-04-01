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
    BranchLeft,
    BranchRight,

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
    AddToGitignore,
    ArchiveFile,
    TrashFile,
    UndoLastFileOp,
    ToggleFolderView,
    StartFilesFilter,
    FilesFilterChar(char),
    FilesFilterBackspace,

    // Commit editor
    StartEditing,
    StopEditing,
    CommitChanges,
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

    // Search dropdown
    SearchSelectUp,
    SearchSelectDown,
    SearchSelectUpQuiet,   // Tab navigation (no graph jump)
    SearchSelectDownQuiet, // Tab navigation (no graph jump)

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
}
