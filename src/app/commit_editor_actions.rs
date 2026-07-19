//! Commit detail pane and commit-message editor.

use super::*;
use crate::text_editor::TextEditor;

/// Apply a text-editing action to `editor`. Returns true if `action` was an
/// editor edit/movement (so callers know it was consumed). Shared by the
/// commit-message editor and the PR-compose editor.
pub(crate) fn apply_editor_edit(editor: &mut TextEditor, action: &Action) -> bool {
    match action {
        Action::EditorChar(c) => editor.insert_char(*c),
        Action::EditorNewline => editor.insert_newline(),
        Action::EditorBackspace => editor.backspace(),
        Action::EditorDelete => editor.delete(),
        Action::EditorLeft(s) => editor.move_left(*s),
        Action::EditorRight(s) => editor.move_right(*s),
        Action::EditorUp(s) => editor.move_up(*s),
        Action::EditorDown(s) => editor.move_down(*s),
        Action::EditorHome(s) => editor.move_home(*s),
        Action::EditorEnd(s) => editor.move_end(*s),
        Action::EditorWordLeft(s) => editor.move_word_left(*s),
        Action::EditorWordRight(s) => editor.move_word_right(*s),
        Action::EditorBackspaceWord => editor.backspace_word(),
        Action::EditorDeleteWord => editor.delete_word(),
        Action::EditorKillLine => editor.kill_line(),
        Action::EditorTextStart(s) => editor.move_text_start(*s),
        Action::EditorTextEnd(s) => editor.move_text_end(*s),
        _ => return false,
    }
    true
}

impl App {
    pub(crate) fn handle_commit_detail_action(&mut self, action: Action) -> Result<()> {
        // Ctrl+S opens the stash options menu whether or not the commit-message
        // editor is active; any typed message is carried through as the default.
        if matches!(action, Action::StashStaged) {
            self.open_stash_menu();
            return Ok(());
        }

        if self.editing_commit_message {
            return self.handle_editor_action(action);
        }

        // Auto-start editing on character input
        if let Action::EditorChar(c) = action {
            if self.is_uncommitted_selected() {
                self.editing_commit_message = true;
                self.amending_commit = false;
                self.commit_editor.insert_char(c);
                self.scroll_to_editor_cursor();
                return Ok(());
            } else if self.is_head_commit_selected() {
                if let Ok(msg) = get_last_commit_message(&self.repo_path) {
                    self.commit_editor = crate::text_editor::TextEditor::from_text(&msg);
                    self.editing_commit_message = true;
                    self.amending_commit = true;
                    self.commit_editor.insert_char(c);
                    self.scroll_to_editor_cursor();
                    return Ok(());
                }
            }
            return Ok(());
        }

        if matches!(action, Action::AmendCommit) {
            if self.block_commit_if_unmerged() {
                return Ok(());
            }
            if self.is_uncommitted_selected() {
                // Ctrl+Enter with no message: amend --no-edit
                commit_amend_no_edit(&self.repo_path)?;
                self.refresh(true)?;
                self.set_message("Commit amended (--no-edit)");
                self.focused_panel = FocusedPanel::Graph;
            }
            return Ok(());
        }

        match action {
            Action::StartEditing => {
                if self.is_uncommitted_selected() {
                    self.editing_commit_message = true;
                    self.amending_commit = false;
                } else if self.is_head_commit_selected() {
                    // Edit HEAD commit message for amending
                    if let Ok(msg) = get_last_commit_message(&self.repo_path) {
                        self.commit_editor = crate::text_editor::TextEditor::from_text(&msg);
                        self.editing_commit_message = true;
                        self.amending_commit = true;
                    }
                }
            }
            Action::MoveUp => {
                self.commit_detail_scroll = self.commit_detail_scroll.saturating_sub(1);
            }
            Action::MoveDown => {
                self.commit_detail_scroll =
                    (self.commit_detail_scroll + 1).min(self.commit_detail_max_scroll);
            }
            Action::PageUp => {
                self.commit_detail_scroll = self.commit_detail_scroll.saturating_sub(10);
            }
            Action::PageDown => {
                self.commit_detail_scroll =
                    (self.commit_detail_scroll + 10).min(self.commit_detail_max_scroll);
            }
            Action::GoToTop => {
                self.commit_detail_scroll = 0;
            }
            Action::ToggleHelp => {
                self.mode = AppMode::Help;
            }
            Action::Refresh => {
                self.refresh(true)?;
                self.reset_timers();
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_editor_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::StopEditing => {
                self.editing_commit_message = false;
                self.amending_commit = false;
            }
            Action::CommitChanges => {
                if self.block_commit_if_unmerged() {
                    return Ok(());
                }
                let msg = self.commit_editor.text.trim().to_string();
                if self.amending_commit {
                    // Amending: use the edited message
                    if !msg.is_empty() {
                        commit_amend(&self.repo_path, &msg)?;
                        self.commit_editor = crate::text_editor::TextEditor::new();
                        self.editing_commit_message = false;
                        self.amending_commit = false;
                        self.refresh(true)?;
                        self.set_message("Commit amended");
                        self.focused_panel = FocusedPanel::Graph;
                    }
                } else if !msg.is_empty() {
                    commit_with_message(&self.repo_path, &msg)?;
                    self.commit_editor = crate::text_editor::TextEditor::new();
                    self.editing_commit_message = false;
                    self.refresh(true)?;
                    self.set_message("Changes committed");
                    self.focused_panel = FocusedPanel::Graph;
                }
            }
            Action::AmendCommit => {
                if self.block_commit_if_unmerged() {
                    return Ok(());
                }
                if self.amending_commit {
                    // Already editing HEAD commit — Ctrl+Enter acts same as Enter (save amend)
                    let msg = self.commit_editor.text.trim().to_string();
                    if !msg.is_empty() {
                        commit_amend(&self.repo_path, &msg)?;
                        self.commit_editor = crate::text_editor::TextEditor::new();
                        self.editing_commit_message = false;
                        self.amending_commit = false;
                        self.refresh(true)?;
                        self.set_message("Commit amended");
                        self.focused_panel = FocusedPanel::Graph;
                    }
                } else {
                    // On uncommitted node — amend last commit
                    let msg = self.commit_editor.text.trim().to_string();
                    if msg.is_empty() {
                        commit_amend_no_edit(&self.repo_path)?;
                    } else {
                        commit_amend(&self.repo_path, &msg)?;
                    }
                    self.commit_editor = crate::text_editor::TextEditor::new();
                    self.editing_commit_message = false;
                    self.refresh(true)?;
                    self.set_message("Commit amended");
                    self.focused_panel = FocusedPanel::Graph;
                }
            }
            other => {
                apply_editor_edit(&mut self.commit_editor, &other);
            }
        }
        self.scroll_to_editor_cursor();
        Ok(())
    }

    /// Auto-scroll the commit detail pane to keep the editor cursor visible.
    fn scroll_to_editor_cursor(&mut self) {
        let (cursor_row, _) = self.commit_editor.cursor_position();
        let absolute_row = self.commit_editor_line_offset as usize + cursor_row;
        let scroll = self.commit_detail_scroll as usize;
        let visible = self.commit_detail_visible_rows as usize;
        if visible == 0 {
            return;
        }
        if absolute_row < scroll {
            self.commit_detail_scroll = absolute_row as u16;
        } else if absolute_row >= scroll + visible {
            self.commit_detail_scroll = (absolute_row - visible + 1) as u16;
        }
        // Don't clamp to max_scroll here — the editor may have added lines
        // that haven't been rendered yet, so max_scroll is stale. The next
        // render will recompute the correct max.
    }
}
