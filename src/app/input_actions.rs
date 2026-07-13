//! Text-input mode (create branch, tag, search).

use super::*;

impl App {
    pub(crate) fn handle_input_action(&mut self, action: Action) -> Result<()> {
        let AppMode::Input {
            title,
            input,
            action: input_action,
        } = &self.mode
        else {
            return Ok(());
        };
        let (title, mut input, input_action) = (title.clone(), input.clone(), input_action.clone());

        match action {
            Action::Confirm => {
                match input_action {
                    InputAction::CreateBranch => {
                        if !input.is_empty() {
                            if let Some(node) = self.selected_commit_node() {
                                if let Some(commit) = &node.commit {
                                    create_branch(self.repo.repo(), &input, commit.oid)?;
                                    self.refresh(true)?;
                                }
                            }
                        }
                    }
                    InputAction::AddTag => {
                        if !input.is_empty() {
                            if let Some(node) = self.selected_commit_node() {
                                if let Some(commit) = &node.commit {
                                    add_tag(self.repo.repo(), &input, commit.oid)?;
                                    self.refresh(true)?;
                                    self.set_message(format!("Tag '{}' created", input));
                                }
                            }
                        }
                    }
                    InputAction::Search => {
                        self.jump_to_search_result();
                    }
                }
                // Clear search state after confirming
                self.search_state = SearchState::default();
                self.mode = AppMode::Normal;
            }
            Action::Cancel => {
                // Restore position when canceling search
                if matches!(input_action, InputAction::Search) {
                    self.restore_search_position();
                }
                self.search_state = SearchState::default();
                self.mode = AppMode::Normal;
            }
            Action::InputChar(c) => {
                input.push(c);

                // Incremental fuzzy search with live preview
                if matches!(input_action, InputAction::Search) {
                    self.update_fuzzy_search(&input);
                    self.jump_to_search_result();
                }

                self.mode = AppMode::Input {
                    title,
                    input,
                    action: input_action,
                };
            }
            Action::InputBackspace => {
                // Empty input + backspace = cancel (like Esc)
                if input.is_empty() {
                    if matches!(input_action, InputAction::Search) {
                        self.restore_search_position();
                    }
                    self.search_state = SearchState::default();
                    self.mode = AppMode::Normal;
                    return Ok(());
                }

                input.pop();

                // Update fuzzy search on backspace with live preview
                if matches!(input_action, InputAction::Search) {
                    self.update_fuzzy_search(&input);
                    self.jump_to_search_result();
                }

                self.mode = AppMode::Input {
                    title,
                    input,
                    action: input_action,
                };
            }
            Action::InputBackspaceWord => {
                crate::text_editor::pop_word(&mut input);

                if matches!(input_action, InputAction::Search) {
                    self.update_fuzzy_search(&input);
                    self.jump_to_search_result();
                }

                self.mode = AppMode::Input {
                    title,
                    input,
                    action: input_action,
                };
            }
            Action::InputClearLine => {
                input.clear();

                if matches!(input_action, InputAction::Search) {
                    self.update_fuzzy_search(&input);
                    self.jump_to_search_result();
                }

                self.mode = AppMode::Input {
                    title,
                    input,
                    action: input_action,
                };
            }
            Action::SearchSelectUp => {
                self.search_state.select_up();
                self.jump_to_search_result();
            }
            Action::SearchSelectDown => {
                self.search_state.select_down();
                self.jump_to_search_result();
            }
            Action::SearchSelectUpQuiet => {
                self.search_state.select_up();
                // No graph jump - just move in dropdown
            }
            Action::SearchSelectDownQuiet => {
                self.search_state.select_down();
                // No graph jump - just move in dropdown
            }
            _ => {}
        }
        Ok(())
    }
}
