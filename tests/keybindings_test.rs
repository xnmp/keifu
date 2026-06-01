//! Pre-refactor regression tests for keybinding mappings.
//! Captures the complete key→action contract so refactoring cannot silently change bindings.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use keifu::action::Action;
use keifu::app::{AppMode, FocusedPanel, InputAction};
use keifu::keybindings::map_key_to_action;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn key_mod(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

fn map_normal_graph(key_event: KeyEvent) -> Option<Action> {
    map_key_to_action(key_event, &AppMode::Normal, FocusedPanel::Graph, false, false)
}

fn map_normal_files(key_event: KeyEvent) -> Option<Action> {
    map_key_to_action(key_event, &AppMode::Normal, FocusedPanel::Files, false, false)
}

fn map_normal_detail(key_event: KeyEvent) -> Option<Action> {
    map_key_to_action(
        key_event,
        &AppMode::Normal,
        FocusedPanel::CommitDetail,
        false,
        false,
    )
}

fn map_editor(key_event: KeyEvent) -> Option<Action> {
    map_key_to_action(
        key_event,
        &AppMode::Normal,
        FocusedPanel::CommitDetail,
        true,
        false,
    )
}

// ── Global hotkeys ──────────────────────────────────────────────────

#[test]
fn ctrl_q_always_force_quits() {
    let k = key_mod(KeyCode::Char('q'), KeyModifiers::CONTROL);
    // Works regardless of mode/panel
    assert_eq!(map_normal_graph(k), Some(Action::ForceQuit));
    assert_eq!(map_normal_files(k), Some(Action::ForceQuit));
    assert_eq!(
        map_key_to_action(k, &AppMode::Help, FocusedPanel::Graph, false, false),
        Some(Action::ForceQuit)
    );
}

#[test]
fn f12_toggles_debug() {
    assert_eq!(
        map_normal_graph(key(KeyCode::F(12))),
        Some(Action::ToggleDebugKeys)
    );
}

#[test]
fn alt_slash_toggles_layout() {
    let k = key_mod(KeyCode::Char('/'), KeyModifiers::ALT);
    assert_eq!(map_normal_graph(k), Some(Action::ToggleLayout));
}

// ── Panel navigation ────────────────────────────────────────────────

#[test]
fn left_arrow_cycles_panel_left() {
    assert_eq!(map_normal_graph(key(KeyCode::Left)), Some(Action::PanelLeft));
    assert_eq!(map_normal_files(key(KeyCode::Left)), Some(Action::PanelLeft));
    assert_eq!(map_normal_detail(key(KeyCode::Left)), Some(Action::PanelLeft));
}

#[test]
fn right_arrow_cycles_panel_right() {
    assert_eq!(
        map_normal_graph(key(KeyCode::Right)),
        Some(Action::PanelRight)
    );
}

#[test]
fn tab_cycles_panel_right() {
    assert_eq!(map_normal_graph(key(KeyCode::Tab)), Some(Action::PanelRight));
}

#[test]
fn shift_backtab_cycles_panel_left() {
    assert_eq!(
        map_normal_graph(key_mod(KeyCode::BackTab, KeyModifiers::SHIFT)),
        Some(Action::PanelLeft)
    );
}

// ── Graph mode ──────────────────────────────────────────────────────

#[test]
fn graph_mode_navigation() {
    assert_eq!(map_normal_graph(key(KeyCode::Up)), Some(Action::MoveUp));
    assert_eq!(map_normal_graph(key(KeyCode::Down)), Some(Action::MoveDown));
    assert_eq!(
        map_normal_graph(key_mod(KeyCode::Char('d'), KeyModifiers::CONTROL)),
        Some(Action::PageDown)
    );
    assert_eq!(
        map_normal_graph(key_mod(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        Some(Action::PageUp)
    );
    assert_eq!(map_normal_graph(key(KeyCode::PageDown)), Some(Action::PageDown));
    assert_eq!(map_normal_graph(key(KeyCode::PageUp)), Some(Action::PageUp));
    assert_eq!(map_normal_graph(key(KeyCode::Char('g'))), Some(Action::GoToTop));
    assert_eq!(map_normal_graph(key(KeyCode::Home)), Some(Action::GoToTop));
    assert_eq!(
        map_normal_graph(key_mod(KeyCode::Char('G'), KeyModifiers::SHIFT)),
        Some(Action::GoToBottom)
    );
    assert_eq!(map_normal_graph(key(KeyCode::End)), Some(Action::GoToBottom));
}

#[test]
fn graph_mode_branch_navigation() {
    assert_eq!(
        map_normal_graph(key(KeyCode::Char('@'))),
        Some(Action::JumpToHead)
    );
    assert_eq!(
        map_normal_graph(key(KeyCode::Char(']'))),
        Some(Action::NextBranch)
    );
    assert_eq!(
        map_normal_graph(key(KeyCode::Char('['))),
        Some(Action::PrevBranch)
    );
    assert_eq!(map_normal_graph(key(KeyCode::Char('h'))), None);
    assert_eq!(map_normal_graph(key(KeyCode::Char('l'))), None);
}

#[test]
fn graph_mode_actions() {
    assert_eq!(
        map_normal_graph(key(KeyCode::Enter)),
        Some(Action::OpenCommitMenu)
    );
    assert_eq!(
        map_normal_graph(key(KeyCode::Char('b'))),
        Some(Action::CreateBranch)
    );
    assert_eq!(
        map_normal_graph(key(KeyCode::Char('d'))),
        Some(Action::DeleteBranch)
    );
    assert_eq!(map_normal_graph(key(KeyCode::Char('f'))), Some(Action::Fetch));
    assert_eq!(
        map_normal_graph(key(KeyCode::Char(' '))),
        Some(Action::OpenFileDiff)
    );
    assert_eq!(
        map_normal_graph(key_mod(KeyCode::Char('B'), KeyModifiers::SHIFT)),
        Some(Action::OpenBranchFilter)
    );
    assert_eq!(
        map_normal_graph(key(KeyCode::Char('/'))),
        Some(Action::Search)
    );
    assert_eq!(
        map_normal_graph(key_mod(KeyCode::Char('R'), KeyModifiers::SHIFT)),
        Some(Action::Refresh)
    );
    assert_eq!(
        map_normal_graph(key(KeyCode::Char('?'))),
        Some(Action::ToggleHelp)
    );
    assert_eq!(map_normal_graph(key(KeyCode::Esc)), Some(Action::Quit));
}

// ── Files mode ──────────────────────────────────────────────────────

#[test]
fn files_mode_navigation() {
    assert_eq!(map_normal_files(key(KeyCode::Up)), Some(Action::MoveUp));
    assert_eq!(map_normal_files(key(KeyCode::Down)), Some(Action::MoveDown));
    assert_eq!(map_normal_files(key(KeyCode::Home)), Some(Action::GoToTop));
    assert_eq!(map_normal_files(key(KeyCode::End)), Some(Action::GoToBottom));
}

#[test]
fn files_mode_operations() {
    assert_eq!(
        map_normal_files(key(KeyCode::Char('s'))),
        Some(Action::ToggleStage)
    );
    assert_eq!(
        map_normal_files(key(KeyCode::Char('a'))),
        Some(Action::ToggleStage)
    );
    assert_eq!(
        map_normal_files(key(KeyCode::Char('i'))),
        Some(Action::AddToGitignore)
    );
    assert_eq!(
        map_normal_files(key(KeyCode::Char('v'))),
        Some(Action::ArchiveFile)
    );
    assert_eq!(
        map_normal_files(key(KeyCode::Delete)),
        Some(Action::TrashFile)
    );
    assert_eq!(
        map_normal_files(key_mod(KeyCode::Char('z'), KeyModifiers::CONTROL)),
        Some(Action::UndoLastFileOp)
    );
    assert_eq!(
        map_normal_files(key(KeyCode::Char('f'))),
        Some(Action::ToggleFolderView)
    );
    assert_eq!(
        map_normal_files(key(KeyCode::Char('r'))),
        Some(Action::RestoreFile)
    );
    assert_eq!(
        map_normal_files(key(KeyCode::Char(' '))),
        Some(Action::OpenWithDefault)
    );
    assert_eq!(
        map_normal_files(key(KeyCode::Enter)),
        Some(Action::OpenFileDiff)
    );
    assert_eq!(
        map_normal_files(key_mod(KeyCode::Char('f'), KeyModifiers::CONTROL)),
        Some(Action::StartFilesFilter)
    );
    assert_eq!(map_normal_files(key(KeyCode::Esc)), Some(Action::FocusGraph));
}

// ── Files filter mode ───────────────────────────────────────────────

#[test]
fn files_filter_mode_input() {
    let map = |k: KeyEvent| {
        map_key_to_action(k, &AppMode::Normal, FocusedPanel::Files, false, true)
    };
    assert_eq!(map(key(KeyCode::Char('a'))), Some(Action::FilesFilterChar('a')));
    assert_eq!(
        map(key_mod(KeyCode::Char('A'), KeyModifiers::SHIFT)),
        Some(Action::FilesFilterChar('A'))
    );
    assert_eq!(map(key(KeyCode::Backspace)), Some(Action::FilesFilterBackspace));
    assert_eq!(map(key(KeyCode::Enter)), Some(Action::Confirm));
    assert_eq!(map(key(KeyCode::Esc)), Some(Action::Cancel));
}

// ── Commit detail mode ──────────────────────────────────────────────

#[test]
fn commit_detail_navigation() {
    assert_eq!(map_normal_detail(key(KeyCode::Up)), Some(Action::MoveUp));
    assert_eq!(map_normal_detail(key(KeyCode::Down)), Some(Action::MoveDown));
    assert_eq!(
        map_normal_detail(key(KeyCode::Enter)),
        Some(Action::StartEditing)
    );
    assert_eq!(map_normal_detail(key(KeyCode::Esc)), Some(Action::FocusGraph));
}

#[test]
fn commit_detail_char_starts_editing() {
    assert_eq!(
        map_normal_detail(key(KeyCode::Char('x'))),
        Some(Action::EditorChar('x'))
    );
}

#[test]
fn commit_detail_ctrl_enter_amends() {
    assert_eq!(
        map_normal_detail(key_mod(KeyCode::Enter, KeyModifiers::CONTROL)),
        Some(Action::AmendCommit)
    );
}

// ── Editor mode ─────────────────────────────────────────────────────

#[test]
fn editor_commit_controls() {
    assert_eq!(map_editor(key(KeyCode::Enter)), Some(Action::CommitChanges));
    assert_eq!(
        map_editor(key_mod(KeyCode::Enter, KeyModifiers::CONTROL)),
        Some(Action::AmendCommit)
    );
    assert_eq!(map_editor(key(KeyCode::Esc)), Some(Action::StopEditing));
}

#[test]
fn editor_newline_via_shift_enter() {
    assert_eq!(
        map_editor(key_mod(KeyCode::Enter, KeyModifiers::SHIFT)),
        Some(Action::EditorNewline)
    );
    assert_eq!(
        map_editor(key_mod(KeyCode::Enter, KeyModifiers::ALT)),
        Some(Action::EditorNewline)
    );
}

#[test]
fn editor_basic_editing() {
    assert_eq!(
        map_editor(key(KeyCode::Char('a'))),
        Some(Action::EditorChar('a'))
    );
    assert_eq!(
        map_editor(key(KeyCode::Backspace)),
        Some(Action::EditorBackspace)
    );
    assert_eq!(map_editor(key(KeyCode::Delete)), Some(Action::EditorDelete));
}

#[test]
fn editor_cursor_movement() {
    assert_eq!(
        map_editor(key(KeyCode::Left)),
        Some(Action::EditorLeft(false))
    );
    assert_eq!(
        map_editor(key_mod(KeyCode::Left, KeyModifiers::SHIFT)),
        Some(Action::EditorLeft(true))
    );
    assert_eq!(
        map_editor(key(KeyCode::Right)),
        Some(Action::EditorRight(false))
    );
    assert_eq!(map_editor(key(KeyCode::Up)), Some(Action::EditorUp(false)));
    assert_eq!(
        map_editor(key(KeyCode::Down)),
        Some(Action::EditorDown(false))
    );
    assert_eq!(
        map_editor(key(KeyCode::Home)),
        Some(Action::EditorHome(false))
    );
    assert_eq!(
        map_editor(key(KeyCode::End)),
        Some(Action::EditorEnd(false))
    );
}

#[test]
fn editor_word_operations() {
    assert_eq!(
        map_editor(key_mod(KeyCode::Left, KeyModifiers::ALT)),
        Some(Action::EditorWordLeft(false))
    );
    assert_eq!(
        map_editor(key_mod(KeyCode::Right, KeyModifiers::ALT)),
        Some(Action::EditorWordRight(false))
    );
    assert_eq!(
        map_editor(key_mod(KeyCode::Backspace, KeyModifiers::ALT)),
        Some(Action::EditorBackspaceWord)
    );
    assert_eq!(
        map_editor(key_mod(KeyCode::Delete, KeyModifiers::ALT)),
        Some(Action::EditorDeleteWord)
    );
}

#[test]
fn editor_text_start_end() {
    assert_eq!(
        map_editor(key_mod(KeyCode::Home, KeyModifiers::CONTROL)),
        Some(Action::EditorTextStart(false))
    );
    assert_eq!(
        map_editor(key_mod(KeyCode::End, KeyModifiers::CONTROL)),
        Some(Action::EditorTextEnd(false))
    );
}

// ── Commit menu mode ────────────────────────────────────────────────

#[test]
fn commit_menu_navigation() {
    let mode = AppMode::CommitMenu {
        items: vec![],
        selected: 0,
        filter: String::new(),
    };
    let map = |k: KeyEvent| map_key_to_action(k, &mode, FocusedPanel::Graph, false, false);

    assert_eq!(map(key(KeyCode::Up)), Some(Action::MoveUp));
    assert_eq!(map(key(KeyCode::Down)), Some(Action::MoveDown));
    assert_eq!(map(key(KeyCode::Enter)), Some(Action::MenuSelect));
    assert_eq!(map(key(KeyCode::Esc)), Some(Action::Cancel));
    assert_eq!(map(key(KeyCode::Char('q'))), Some(Action::Cancel));
}

// ── Branch filter mode ──────────────────────────────────────────────

#[test]
fn branch_filter_controls() {
    let mode = AppMode::BranchFilter {
        filter: String::new(),
        selected: 0,
        all_branches: vec![],
    };
    let map = |k: KeyEvent| map_key_to_action(k, &mode, FocusedPanel::Graph, false, false);

    assert_eq!(map(key(KeyCode::Up)), Some(Action::MoveUp));
    assert_eq!(map(key(KeyCode::Down)), Some(Action::MoveDown));
    assert_eq!(map(key(KeyCode::Char(' '))), Some(Action::MenuSelect));
    assert_eq!(
        map(key_mod(KeyCode::Char('a'), KeyModifiers::CONTROL)),
        Some(Action::SelectAll)
    );
    assert_eq!(
        map(key_mod(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        Some(Action::SelectNone)
    );
    assert_eq!(map(key(KeyCode::Enter)), Some(Action::Confirm));
    assert_eq!(map(key(KeyCode::Esc)), Some(Action::Cancel));
    assert_eq!(map(key(KeyCode::Char('x'))), Some(Action::InputChar('x')));
    assert_eq!(map(key(KeyCode::Backspace)), Some(Action::InputBackspace));
}

// ── Help mode ───────────────────────────────────────────────────────

#[test]
fn help_mode_dismiss() {
    let map =
        |k: KeyEvent| map_key_to_action(k, &AppMode::Help, FocusedPanel::Graph, false, false);

    assert_eq!(map(key(KeyCode::Esc)), Some(Action::ToggleHelp));
    assert_eq!(map(key(KeyCode::Char('q'))), Some(Action::ToggleHelp));
    assert_eq!(map(key(KeyCode::Char('?'))), Some(Action::ToggleHelp));
    assert_eq!(map(key(KeyCode::Char('x'))), None);
}

// ── Input mode ──────────────────────────────────────────────────────

#[test]
fn input_mode_basic() {
    let mode = AppMode::Input {
        title: String::new(),
        input: String::new(),
        action: InputAction::CreateBranch,
    };
    let map = |k: KeyEvent| map_key_to_action(k, &mode, FocusedPanel::Graph, false, false);

    assert_eq!(map(key(KeyCode::Enter)), Some(Action::Confirm));
    assert_eq!(map(key(KeyCode::Esc)), Some(Action::Cancel));
    assert_eq!(map(key(KeyCode::Backspace)), Some(Action::InputBackspace));
    assert_eq!(map(key(KeyCode::Char('a'))), Some(Action::InputChar('a')));
}

// ── Search mode ─────────────────────────────────────────────────────

#[test]
fn search_mode_controls() {
    let mode = AppMode::Input {
        title: String::new(),
        input: String::new(),
        action: InputAction::Search,
    };
    let map = |k: KeyEvent| map_key_to_action(k, &mode, FocusedPanel::Graph, false, false);

    assert_eq!(map(key(KeyCode::Up)), Some(Action::SearchSelectUp));
    assert_eq!(map(key(KeyCode::Down)), Some(Action::SearchSelectDown));
    assert_eq!(
        map(key_mod(KeyCode::Char('k'), KeyModifiers::CONTROL)),
        Some(Action::SearchSelectUp)
    );
    assert_eq!(
        map(key_mod(KeyCode::Char('j'), KeyModifiers::CONTROL)),
        Some(Action::SearchSelectDown)
    );
    assert_eq!(map(key(KeyCode::Tab)), Some(Action::SearchSelectDownQuiet));
    assert_eq!(
        map(key_mod(KeyCode::BackTab, KeyModifiers::SHIFT)),
        Some(Action::SearchSelectUpQuiet)
    );
    assert_eq!(map(key(KeyCode::Enter)), Some(Action::Confirm));
    assert_eq!(map(key(KeyCode::Esc)), Some(Action::Cancel));
    assert_eq!(map(key(KeyCode::Char('x'))), Some(Action::InputChar('x')));
}

// ── Confirm mode ────────────────────────────────────────────────────

#[test]
fn confirm_mode_yes_no() {
    let mode = AppMode::Confirm {
        message: String::new(),
        action: keifu::app::ConfirmAction::Push,
    };
    let map = |k: KeyEvent| map_key_to_action(k, &mode, FocusedPanel::Graph, false, false);

    assert_eq!(map(key(KeyCode::Char('y'))), Some(Action::Confirm));
    assert_eq!(map(key(KeyCode::Enter)), Some(Action::Confirm));
    assert_eq!(map(key(KeyCode::Char('n'))), Some(Action::Cancel));
    assert_eq!(map(key(KeyCode::Esc)), Some(Action::Cancel));
    assert_eq!(map(key(KeyCode::Char('x'))), None);
}

// ── Error mode ──────────────────────────────────────────────────────

#[test]
fn error_mode_dismiss() {
    let mode = AppMode::Error {
        message: String::new(),
    };
    let map = |k: KeyEvent| map_key_to_action(k, &mode, FocusedPanel::Graph, false, false);

    assert_eq!(map(key(KeyCode::Esc)), Some(Action::Cancel));
    assert_eq!(map(key(KeyCode::Enter)), Some(Action::Cancel));
    assert_eq!(map(key(KeyCode::Char('q'))), Some(Action::Cancel));
    assert_eq!(map(key(KeyCode::Char('x'))), None);
}

// ── File diff mode ──────────────────────────────────────────────────

#[test]
fn file_diff_mode_scrolling() {
    let mode = AppMode::FileDiff {
        file_index: 0,
        file_list: vec![],
        content: keifu::git::FileDiffContent {
            path: std::path::PathBuf::new(),
            kind: keifu::git::FileChangeKind::Modified,
            is_binary: false,
            hunks: vec![],
            total_additions: 0,
            total_deletions: 0,
        },
        rendered_lines: vec![],
        hunk_positions: vec![],
        scroll_offset: 0,
        horizontal_offset: 0,
        max_line_width: 0,
        total_lines: 0,
    };
    let map = |k: KeyEvent| map_key_to_action(k, &mode, FocusedPanel::Graph, false, false);

    assert_eq!(map(key(KeyCode::Down)), Some(Action::ScrollDown));
    assert_eq!(map(key(KeyCode::Up)), Some(Action::ScrollUp));
    assert_eq!(
        map(key_mod(KeyCode::Char('d'), KeyModifiers::CONTROL)),
        Some(Action::ScrollPageDown)
    );
    assert_eq!(
        map(key_mod(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        Some(Action::ScrollPageUp)
    );
    assert_eq!(map(key(KeyCode::Char('g'))), Some(Action::ScrollToTop));
    assert_eq!(map(key(KeyCode::Home)), Some(Action::ScrollToTop));
    assert_eq!(
        map(key_mod(KeyCode::Char('G'), KeyModifiers::SHIFT)),
        Some(Action::ScrollToBottom)
    );
    assert_eq!(map(key(KeyCode::End)), Some(Action::ScrollToBottom));
    assert_eq!(map(key(KeyCode::Char('h'))), Some(Action::ScrollLeft));
    assert_eq!(map(key(KeyCode::Left)), Some(Action::ScrollLeft));
    assert_eq!(map(key(KeyCode::Char('l'))), Some(Action::ScrollRight));
    assert_eq!(map(key(KeyCode::Right)), Some(Action::ScrollRight));
    assert_eq!(map(key(KeyCode::Char('0'))), Some(Action::ScrollToLineStart));
}

#[test]
fn file_diff_mode_file_navigation() {
    let mode = AppMode::FileDiff {
        file_index: 0,
        file_list: vec![],
        content: keifu::git::FileDiffContent {
            path: std::path::PathBuf::new(),
            kind: keifu::git::FileChangeKind::Modified,
            is_binary: false,
            hunks: vec![],
            total_additions: 0,
            total_deletions: 0,
        },
        rendered_lines: vec![],
        hunk_positions: vec![],
        scroll_offset: 0,
        horizontal_offset: 0,
        max_line_width: 0,
        total_lines: 0,
    };
    let map = |k: KeyEvent| map_key_to_action(k, &mode, FocusedPanel::Graph, false, false);

    assert_eq!(map(key(KeyCode::Char(']'))), Some(Action::NextHunk));
    assert_eq!(map(key(KeyCode::Char('['))), Some(Action::PrevHunk));
    assert_eq!(map(key(KeyCode::Char('n'))), Some(Action::NextFile));
    assert_eq!(
        map(key_mod(KeyCode::Char('N'), KeyModifiers::SHIFT)),
        Some(Action::PrevFile)
    );
    assert_eq!(map(key(KeyCode::Esc)), Some(Action::Cancel));
    assert_eq!(map(key(KeyCode::Char('q'))), Some(Action::Cancel));
}
