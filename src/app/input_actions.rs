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
                                    self.toast(crate::toast::ToastKind::Success, format!("Tag '{}' created", input));
                                }
                            }
                        }
                    }
                    InputAction::RenameBranch { old_name } => {
                        if !input.is_empty() && input != old_name {
                            rename_branch(&self.repo_path, &old_name, &input)?;
                            // Inverse: rename the new name back to the old.
                            self.record_undo(crate::undo::UndoEntry {
                                description: format!("Rename '{old_name}' → '{input}'"),
                                confirm: format!("Undo: rename → back to '{old_name}'?"),
                                plan: crate::undo::UndoPlan::RenameBranch {
                                    from: input.clone(),
                                    to: old_name.clone(),
                                },
                                check: crate::undo::UndoCheck::RenameConsistent {
                                    exists: input.clone(),
                                    absent: old_name.clone(),
                                },
                            });
                            self.refresh(true)?;
                            self.toast(crate::toast::ToastKind::Success, format!("Renamed '{}' -> '{}'", old_name, input));
                        }
                    }
                    InputAction::BranchFromStash { index } => {
                        if !input.is_empty() {
                            stash_branch(&self.repo_path, &input, index)?;
                            self.refresh(true)?;
                            self.toast(crate::toast::ToastKind::Success, format!("Created branch '{}' from stash", input));
                        }
                    }
                    InputAction::StashPush { scope } => {
                        self.do_stash_push(scope, input.trim())?;
                    }
                    InputAction::EditIssueAssignees { number } => {
                        // The runner is async; return to the detail popup rather
                        // than falling through to Normal below. If the runner was
                        // busy the edit was rejected — keep the Input open (mode
                        // untouched) so the typed logins aren't lost.
                        if self.submit_issue_assignees(number, &input) {
                            self.search_state = SearchState::default();
                            self.mode = AppMode::IssueDetail;
                        }
                        return Ok(());
                    }
                    InputAction::Search => {
                        self.jump_to_search_result();
                    }
                    // Credential prompt: username step advances to the masked
                    // password step; password step caches + retries. Both manage
                    // their own mode transition and return early.
                    InputAction::AuthUsername => {
                        self.auth_advance_to_password(input.clone());
                        return Ok(());
                    }
                    InputAction::AuthPassword => {
                        self.auth_submit_password(input.clone());
                        return Ok(());
                    }
                }
                // Clear search state after confirming
                self.search_state = SearchState::default();
                self.mode = AppMode::Normal;
            }
            Action::Cancel => {
                // A credential prompt cancels cleanly back to Normal, discarding
                // the pending op.
                if matches!(
                    input_action,
                    InputAction::AuthUsername | InputAction::AuthPassword
                ) {
                    self.auth_cancel();
                    return Ok(());
                }
                // Restore position when canceling search
                if matches!(input_action, InputAction::Search) {
                    self.restore_search_position();
                }
                self.search_state = SearchState::default();
                // Cancelling the assignee edit returns to the issue detail it was
                // launched from, not all the way out to Normal.
                self.mode = if matches!(input_action, InputAction::EditIssueAssignees { .. }) {
                    AppMode::IssueDetail
                } else {
                    AppMode::Normal
                };
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
            Action::InputPaste(pasted) => {
                // Append a (pre-sanitized) paste chunk atomically.
                input.push_str(&pasted);

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
                    self.mode = if matches!(input_action, InputAction::EditIssueAssignees { .. }) {
                        AppMode::IssueDetail
                    } else {
                        AppMode::Normal
                    };
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

    /// Push the working tree to a stash for the chosen scope, then clear the
    /// commit-message editor and return focus to the graph.
    fn do_stash_push(&mut self, scope: StashScope, message: &str) -> Result<()> {
        match scope {
            StashScope::Staged => stash_staged(&self.repo_path, message)?,
            StashScope::All => stash_all(&self.repo_path, message, false)?,
            StashScope::AllUntracked => stash_all(&self.repo_path, message, true)?,
        }
        self.commit_editor = crate::text_editor::TextEditor::new();
        self.editing_commit_message = false;
        self.refresh(true)?;
        self.toast(crate::toast::ToastKind::Success, "Stashed changes");
        self.focused_panel = FocusedPanel::Graph;
        Ok(())
    }
}
