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
}
