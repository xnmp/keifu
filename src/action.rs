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
    FocusLeftPane,
    FocusRightPane,
    FocusUpPane,
    FocusDownPane,

    // Git operations
    Checkout,
    FocusFiles,
    FilesSelect,
    FilesOpenModal,
    ToggleStage,
    ModalScrollUp,
    ModalScrollDown,
    ModalPageUp,
    ModalPageDown,
    CreateBranch,
    DeleteBranch,
    Fetch,
    Merge,
    Rebase,

    // UI
    ToggleHelp,
    Search,
    Refresh,
    QuitAll,
    Quit,
    ToggleKeyDebug,
    CommitMessageToggleEdit,
    CommitMessageStopEdit,

    // Dialogs
    Confirm,
    Cancel,
    InputChar(char),
    InputBackspace,
    CommitMessageInsertNewline,
    CommitMessageMoveLeft,
    CommitMessageMoveRight,
    CommitMessageMoveHome,
    CommitMessageMoveEnd,
    CommitMessageMoveStart,
    CommitMessageMoveFinish,
    CommitMessageSelectLeft,
    CommitMessageSelectRight,
    CommitMessageSelectHome,
    CommitMessageSelectEnd,
    CommitMessageSelectStart,
    CommitMessageSelectFinish,
    CommitMessageMoveWordLeft,
    CommitMessageMoveWordRight,
    CommitMessageDeleteWordBack,
    CommitMessageDeleteWordForward,
    CommitMessageDeleteForward,
    CommitMessageCommit,

    // Search dropdown
    SearchSelectUp,
    SearchSelectDown,
    SearchSelectUpQuiet,   // Tab navigation (no graph jump)
    SearchSelectDownQuiet, // Tab navigation (no graph jump)
}
