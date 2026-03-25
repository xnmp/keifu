//! Keybindings

#[cfg(windows)]
use crossterm::event::KeyEventKind;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;
use crate::app::{AppMode, FocusedPanel};

pub fn map_key_to_action(
    key: KeyEvent,
    mode: &AppMode,
    focused_panel: FocusedPanel,
    editing_commit: bool,
) -> Option<Action> {
    #[cfg(windows)]
    if key.kind != KeyEventKind::Press {
        return None;
    }

    // Ctrl+Q always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
        return Some(Action::ForceQuit);
    }

    match mode {
        AppMode::Normal => map_normal_mode(key, focused_panel, editing_commit),
        AppMode::Help => map_help_mode(key),
        AppMode::Input { action, .. } => {
            if *action == crate::app::InputAction::Search {
                map_search_mode(key)
            } else {
                map_input_mode(key)
            }
        }
        AppMode::Confirm { .. } => map_confirm_mode(key),
        AppMode::Error { .. } => map_error_mode(key),
        AppMode::CommitMenu { .. } => map_commit_menu_mode(key),
        AppMode::FileSelect { .. } => map_file_select_mode(key),
        AppMode::FileDiff { .. } => map_file_diff_mode(key),
    }
}

fn map_normal_mode(
    key: KeyEvent,
    panel: FocusedPanel,
    editing_commit: bool,
) -> Option<Action> {
    // If editing commit message, route to editor keybindings
    if editing_commit && panel == FocusedPanel::CommitDetail {
        return map_editor_mode(key);
    }

    // Panel navigation with left/right arrows (from any panel)
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Left) => return Some(Action::PanelLeft),
        (KeyModifiers::NONE, KeyCode::Right) => return Some(Action::PanelRight),
        _ => {}
    }

    match panel {
        FocusedPanel::Graph => map_graph_mode(key),
        FocusedPanel::Files => map_files_mode(key),
        FocusedPanel::CommitDetail => map_commit_detail_mode(key),
    }
}

fn map_graph_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        // Movement (arrow keys only, j/k removed per requirements)
        (KeyModifiers::NONE, KeyCode::Down) => Some(Action::MoveDown),
        (KeyModifiers::NONE, KeyCode::Up) => Some(Action::MoveUp),

        // Page scroll
        (KeyModifiers::CONTROL, KeyCode::Char('d')) => Some(Action::PageDown),
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => Some(Action::PageUp),

        // Top/bottom
        (KeyModifiers::NONE, KeyCode::Char('g')) | (KeyModifiers::NONE, KeyCode::Home) => {
            Some(Action::GoToTop)
        }
        (KeyModifiers::SHIFT, KeyCode::Char('G')) | (KeyModifiers::NONE, KeyCode::End) => {
            Some(Action::GoToBottom)
        }

        // Jump to HEAD
        (_, KeyCode::Char('@')) => Some(Action::JumpToHead),

        // Branch jump
        (_, KeyCode::Char(']')) | (KeyModifiers::NONE, KeyCode::Tab) => Some(Action::NextBranch),
        (_, KeyCode::Char('[')) | (KeyModifiers::SHIFT, KeyCode::BackTab) => {
            Some(Action::PrevBranch)
        }

        // Branch selection within same commit (h/l only, left/right now switch panels)
        (KeyModifiers::NONE, KeyCode::Char('h')) => Some(Action::BranchLeft),
        (KeyModifiers::NONE, KeyCode::Char('l')) => Some(Action::BranchRight),

        // Enter opens commit menu (or goes to files for uncommitted)
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::OpenCommitMenu),

        // Quick actions
        (KeyModifiers::NONE, KeyCode::Char('b')) => Some(Action::CreateBranch),
        (KeyModifiers::NONE, KeyCode::Char('d')) => Some(Action::DeleteBranch),
        (KeyModifiers::NONE, KeyCode::Char('f')) => Some(Action::Fetch),

        // Space still opens file select for quick access
        (KeyModifiers::NONE, KeyCode::Char(' ')) => Some(Action::EnterFileSelect),

        // UI
        (_, KeyCode::Char('/')) => Some(Action::Search),
        (KeyModifiers::SHIFT, KeyCode::Char('R')) => Some(Action::Refresh),
        (_, KeyCode::Char('?')) => Some(Action::ToggleHelp),
        (KeyModifiers::NONE, KeyCode::Char('q')) | (KeyModifiers::NONE, KeyCode::Esc) => {
            Some(Action::Quit)
        }

        _ => None,
    }
}

fn map_files_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        // Movement
        (KeyModifiers::NONE, KeyCode::Down) => Some(Action::MoveDown),
        (KeyModifiers::NONE, KeyCode::Up) => Some(Action::MoveUp),
        (KeyModifiers::CONTROL, KeyCode::Char('d')) => Some(Action::PageDown),
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => Some(Action::PageUp),
        (KeyModifiers::NONE, KeyCode::Home) => Some(Action::GoToTop),
        (KeyModifiers::NONE, KeyCode::End) => Some(Action::GoToBottom),

        // Stage/unstage
        (KeyModifiers::NONE, KeyCode::Char('s')) => Some(Action::ToggleStage),

        // Enter file select for diff viewing
        (KeyModifiers::NONE, KeyCode::Enter) | (KeyModifiers::NONE, KeyCode::Char(' ')) => {
            Some(Action::EnterFileSelect)
        }

        // Esc returns to graph
        (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::FocusGraph),

        // Help / quit
        (_, KeyCode::Char('?')) => Some(Action::ToggleHelp),
        (KeyModifiers::NONE, KeyCode::Char('q')) => Some(Action::Quit),

        _ => None,
    }
}

fn map_commit_detail_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        // Enter starts editing (for uncommitted changes)
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::StartEditing),

        // Esc returns to graph
        (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::FocusGraph),

        // Help / quit
        (_, KeyCode::Char('?')) => Some(Action::ToggleHelp),
        (KeyModifiers::NONE, KeyCode::Char('q')) => Some(Action::Quit),

        _ => None,
    }
}

fn map_editor_mode(key: KeyEvent) -> Option<Action> {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match (key.modifiers, key.code) {
        // Alt+Enter commits
        (m, KeyCode::Enter) if m.contains(KeyModifiers::ALT) => Some(Action::CommitChanges),

        // Esc exits edit mode
        (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::StopEditing),

        // Enter inserts newline
        (KeyModifiers::NONE, KeyCode::Enter) | (KeyModifiers::SHIFT, KeyCode::Enter) => {
            Some(Action::EditorNewline)
        }

        // Alt+Backspace: delete word backward
        (m, KeyCode::Backspace) if m.contains(KeyModifiers::ALT) => {
            Some(Action::EditorBackspaceWord)
        }

        // Alt+Delete / Alt+d: delete word forward
        (m, KeyCode::Delete) if m.contains(KeyModifiers::ALT) => {
            Some(Action::EditorDeleteWord)
        }
        (m, KeyCode::Char('d')) if m.contains(KeyModifiers::ALT) => {
            Some(Action::EditorDeleteWord)
        }

        // Alt+Left / Alt+b: word left
        (m, KeyCode::Left) if m.contains(KeyModifiers::ALT) => {
            Some(Action::EditorWordLeft(shift))
        }
        (m, KeyCode::Char('b')) if m.contains(KeyModifiers::ALT) && !shift => {
            Some(Action::EditorWordLeft(false))
        }

        // Alt+Right / Alt+f: word right
        (m, KeyCode::Right) if m.contains(KeyModifiers::ALT) => {
            Some(Action::EditorWordRight(shift))
        }
        (m, KeyCode::Char('f')) if m.contains(KeyModifiers::ALT) && !shift => {
            Some(Action::EditorWordRight(false))
        }

        // Ctrl+Home / Alt+Home: text start
        (m, KeyCode::Home) if m.contains(KeyModifiers::CONTROL) || m.contains(KeyModifiers::ALT) => {
            Some(Action::EditorTextStart(shift))
        }
        // Ctrl+End / Alt+End: text end
        (m, KeyCode::End) if m.contains(KeyModifiers::CONTROL) || m.contains(KeyModifiers::ALT) => {
            Some(Action::EditorTextEnd(shift))
        }

        // Basic cursor movement
        (_, KeyCode::Left) if !alt && !ctrl => Some(Action::EditorLeft(shift)),
        (_, KeyCode::Right) if !alt && !ctrl => Some(Action::EditorRight(shift)),
        (_, KeyCode::Up) if !alt && !ctrl => Some(Action::EditorUp(shift)),
        (_, KeyCode::Down) if !alt && !ctrl => Some(Action::EditorDown(shift)),
        (_, KeyCode::Home) if !alt && !ctrl => Some(Action::EditorHome(shift)),
        (_, KeyCode::End) if !alt && !ctrl => Some(Action::EditorEnd(shift)),

        // Backspace/Delete
        (KeyModifiers::NONE, KeyCode::Backspace) => Some(Action::EditorBackspace),
        (KeyModifiers::NONE, KeyCode::Delete) => Some(Action::EditorDelete),

        // Character input (no ctrl, no alt)
        (m, KeyCode::Char(c))
            if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
        {
            Some(Action::EditorChar(c))
        }

        _ => None,
    }
}

fn map_commit_menu_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
            Some(Action::MoveUp)
        }
        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
            Some(Action::MoveDown)
        }
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::MenuSelect),
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
            Some(Action::Cancel)
        }
        _ => None,
    }
}

fn map_help_mode(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => Some(Action::ToggleHelp),
        _ => None,
    }
}

fn map_input_mode(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Enter => Some(Action::Confirm),
        KeyCode::Esc => Some(Action::Cancel),
        KeyCode::Backspace => Some(Action::InputBackspace),
        KeyCode::Char(c) => Some(Action::InputChar(c)),
        _ => None,
    }
}

fn map_search_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) => Some(Action::SearchSelectUp),
        (KeyModifiers::NONE, KeyCode::Down) => Some(Action::SearchSelectDown),
        (KeyModifiers::CONTROL, KeyCode::Char('k')) => Some(Action::SearchSelectUp),
        (KeyModifiers::CONTROL, KeyCode::Char('j')) => Some(Action::SearchSelectDown),
        (KeyModifiers::NONE, KeyCode::Tab) => Some(Action::SearchSelectDownQuiet),
        (KeyModifiers::SHIFT, KeyCode::BackTab) => Some(Action::SearchSelectUpQuiet),
        (_, KeyCode::Enter) => Some(Action::Confirm),
        (_, KeyCode::Esc) => Some(Action::Cancel),
        (_, KeyCode::Backspace) | (_, KeyCode::Delete) => Some(Action::InputBackspace),
        (_, KeyCode::Char(c)) => Some(Action::InputChar(c)),
        _ => None,
    }
}

fn map_confirm_mode(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => Some(Action::Confirm),
        KeyCode::Char('n') | KeyCode::Esc => Some(Action::Cancel),
        _ => None,
    }
}

fn map_error_mode(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => Some(Action::Cancel),
        _ => None,
    }
}

fn map_file_select_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Char('j')) | (KeyModifiers::NONE, KeyCode::Down) => {
            Some(Action::FileSelectDown)
        }
        (KeyModifiers::NONE, KeyCode::Char('k')) | (KeyModifiers::NONE, KeyCode::Up) => {
            Some(Action::FileSelectUp)
        }
        (KeyModifiers::NONE, KeyCode::Char('s')) => Some(Action::ToggleStage),
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::OpenFileDiff),
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
            Some(Action::Cancel)
        }
        _ => None,
    }
}

fn map_file_diff_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Char('j')) | (KeyModifiers::NONE, KeyCode::Down) => {
            Some(Action::ScrollDown)
        }
        (KeyModifiers::NONE, KeyCode::Char('k')) | (KeyModifiers::NONE, KeyCode::Up) => {
            Some(Action::ScrollUp)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('d')) => Some(Action::ScrollPageDown),
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => Some(Action::ScrollPageUp),
        (KeyModifiers::CONTROL, KeyCode::Char('f')) | (KeyModifiers::NONE, KeyCode::PageDown) => {
            Some(Action::PageDown)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('b')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
            Some(Action::PageUp)
        }
        (KeyModifiers::NONE, KeyCode::Char('g')) | (KeyModifiers::NONE, KeyCode::Home) => {
            Some(Action::ScrollToTop)
        }
        (KeyModifiers::SHIFT, KeyCode::Char('G')) | (KeyModifiers::NONE, KeyCode::End) => {
            Some(Action::ScrollToBottom)
        }
        (KeyModifiers::NONE, KeyCode::Char('h')) | (KeyModifiers::NONE, KeyCode::Left) => {
            Some(Action::ScrollLeft)
        }
        (KeyModifiers::NONE, KeyCode::Char('l')) | (KeyModifiers::NONE, KeyCode::Right) => {
            Some(Action::ScrollRight)
        }
        (KeyModifiers::NONE, KeyCode::Char('0')) => Some(Action::ScrollToLineStart),
        (_, KeyCode::Char(']')) => Some(Action::NextHunk),
        (_, KeyCode::Char('[')) => Some(Action::PrevHunk),
        (KeyModifiers::NONE, KeyCode::Char('n')) => Some(Action::NextFile),
        (KeyModifiers::SHIFT, KeyCode::Char('N')) => Some(Action::PrevFile),
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
            Some(Action::Cancel)
        }
        _ => None,
    }
}
