//! Files panel: stage, gitignore, archive, trash, restore, undo.

use super::*;

impl App {
    pub(crate) fn handle_files_action(&mut self, action: Action) -> Result<()> {
        self.sync_file_list_cache();
        let item_count = self.files_pane.display_items().len();

        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::MoveUp => {
                self.move_file_selection(-1);
            }
            Action::MoveDown => {
                self.move_file_selection(1);
            }
            Action::PageUp => {
                self.move_file_selection(-10);
            }
            Action::PageDown => {
                self.move_file_selection(10);
            }
            Action::GoToTop => {
                self.move_file_selection(-(item_count as i32));
            }
            Action::GoToBottom => {
                self.move_file_selection(item_count as i32);
            }
            Action::OpenFileDiff => {
                if let Some(file) = self.selected_file().cloned() {
                    let file_list = self.files_pane.display_file_list();
                    let flat_idx = self.display_index_to_flat_index(self.file_selected_index());
                    if let Err(e) = self.enter_file_diff(flat_idx, file_list, &file.path) {
                        self.set_message(format!("Cannot open diff: {e}"));
                    }
                }
            }
            Action::OpenWithDefault => {
                if let Some(file) = self.selected_file() {
                    let path = file.path.clone();
                    let full_path = if self.is_uncommitted_selected() {
                        // Working tree file — open directly
                        std::path::Path::new(&self.repo_path).join(&path)
                    } else if let Some(node) = self.selected_commit_node() {
                        // Committed file — extract blob to temp file
                        if let Some(commit) = &node.commit {
                            match self.extract_blob_to_temp(commit.oid, &path) {
                                Ok(tmp) => tmp,
                                Err(e) => {
                                    self.set_message(format!("Cannot extract file: {e}"));
                                    return Ok(());
                                }
                            }
                        } else {
                            return Ok(());
                        }
                    } else {
                        std::path::Path::new(&self.repo_path).join(&path)
                    };
                    self.open_with_default(&full_path, &path);
                }
            }
            Action::ToggleStage => {
                self.toggle_stage_selected_file()?;
            }
            Action::StageAll => {
                self.stage_all_files()?;
            }
            Action::UnstageAll => {
                self.unstage_all_files()?;
            }
            Action::AddToGitignore => {
                self.add_selected_to_gitignore()?;
            }
            Action::ArchiveFile => {
                if self.is_in_archived_section() {
                    self.unarchive_selected_file()?;
                } else {
                    self.archive_selected_file()?;
                }
            }
            Action::TrashFile => {
                self.trash_selected_file()?;
            }
            Action::RestoreFile => {
                self.restore_selected_file()?;
            }
            Action::AcceptOurs => {
                self.accept_conflict_side(true)?;
            }
            Action::AcceptTheirs => {
                self.accept_conflict_side(false)?;
            }
            Action::ContinueOperation => {
                self.continue_in_progress_operation()?;
            }
            Action::AbortOperation => {
                self.prompt_abort_operation();
            }
            Action::UndoLastFileOp => {
                self.undo_last_file_op()?;
            }
            Action::ToggleFolderView => {
                self.files_pane.files_group_by_folder = !self.files_pane.files_group_by_folder;
            }
            Action::StartFilesFilter => {
                self.files_pane.files_filter_active = true;
                self.files_pane.files_filter.clear();
            }
            Action::FilesFilterChar(c) => {
                self.files_pane.files_filter.push(c);
            }
            Action::FilesFilterBackspace => {
                if !self.files_pane.files_filter.is_empty() {
                    self.files_pane.files_filter.pop();
                } else {
                    // Empty filter + backspace exits filter mode
                    self.files_pane.files_filter_active = false;
                }
            }
            Action::InputBackspaceWord => {
                crate::text_editor::pop_word(&mut self.files_pane.files_filter);
            }
            Action::InputClearLine => {
                self.files_pane.files_filter.clear();
            }
            Action::Confirm => {
                // Enter: keep filter, exit filter mode
                self.files_pane.files_filter_active = false;
            }
            Action::Cancel => {
                // Esc: clear filter, exit filter mode
                self.files_pane.files_filter.clear();
                self.files_pane.files_filter_active = false;
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

    fn move_file_selection(&mut self, delta: i32) {
        self.files_pane.move_file_selection(delta);
    }

    fn toggle_stage_selected_file(&mut self) -> Result<()> {
        if !self.is_uncommitted_selected() {
            return Ok(());
        }

        let files = self
            .selected_files()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        if files.is_empty() {
            return Ok(());
        }

        // Determine direction: if any file is unstaged, we stage; otherwise unstage all
        let any_unstaged = files
            .iter()
            .any(|f| !matches!(f.stage_status, Some(StageStatus::Staged)));
        let staging = any_unstaged;

        for file in &files {
            let path_str = file.path.to_string_lossy().to_string();
            if staging {
                stage_file(&self.repo_path, &path_str)?;
            } else {
                unstage_file(&self.repo_path, &path_str)?;
            }
        }

        // Record undo for single file; for multiple, record the first
        if files.len() == 1 {
            self.last_undoable_op = Some(UndoableOperation::Stage {
                path: files[0].path.to_string_lossy().to_string(),
                was_staged: !staging,
            });
        }

        self.refresh_after_file_op()?;
        Ok(())
    }

    fn stage_all_files(&mut self) -> Result<()> {
        if !self.is_uncommitted_selected() {
            return Ok(());
        }
        stage_all(&self.repo_path)?;
        self.set_message("Staged all changes");
        self.refresh_after_file_op()?;
        Ok(())
    }

    fn unstage_all_files(&mut self) -> Result<()> {
        if !self.is_uncommitted_selected() {
            return Ok(());
        }
        unstage_all(&self.repo_path)?;
        self.set_message("Unstaged all changes");
        self.refresh_after_file_op()?;
        Ok(())
    }

    fn add_selected_to_gitignore(&mut self) -> Result<()> {
        if !self.is_uncommitted_selected() {
            return Ok(());
        }

        // Resolve the pattern: header → folder path, file → file path
        let pattern = match self.selected_display_item() {
            Some(FilesPaneItem::FolderHeader(text)) => {
                text.clone()
            }
            Some(FilesPaneItem::File(file)) => {
                file.path.to_string_lossy().to_string()
            }
            Some(FilesPaneItem::SectionHeader(_)) | None => return Ok(()),
        };

        match add_to_gitignore(&self.repo_path, &pattern)? {
            true => {
                self.last_undoable_op = Some(UndoableOperation::Gitignore {
                    pattern: pattern.clone(),
                });
                self.set_message(format!("Added '{}' to .gitignore", pattern));
                self.refresh_after_file_op()?;
            }
            false => {
                self.set_message(format!("'{}' already in .gitignore", pattern));
            }
        }

        Ok(())
    }

    fn archive_selected_file(&mut self) -> Result<()> {
        if !self.is_uncommitted_selected() {
            return Ok(());
        }

        // Resolve target: header → folder path (without trailing /), file → file path
        let target = match self.selected_display_item() {
            Some(FilesPaneItem::FolderHeader(text)) => {
                text.trim_end_matches('/').to_string()
            }
            Some(FilesPaneItem::File(file)) => {
                file.path.to_string_lossy().to_string()
            }
            Some(FilesPaneItem::SectionHeader(_)) | None => return Ok(()),
        };

        archive_path(&self.repo_path, &target)?;
        // Ensure .archive is in .gitignore
        let _ = add_to_gitignore(&self.repo_path, ".archive");
        self.last_undoable_op = Some(UndoableOperation::Archive {
            relative_path: target.clone(),
        });
        self.set_message(format!("Archived '{}'", target));
        self.refresh_after_file_op()?;

        Ok(())
    }

    fn unarchive_selected_file(&mut self) -> Result<()> {
        let Some(FilesPaneItem::File(file)) = self.selected_display_item().cloned() else {
            return Ok(());
        };
        let target = file.path.to_string_lossy().to_string();
        unarchive_path(&self.repo_path, &target)?;
        self.set_message(format!("Unarchived '{}'", target));
        self.refresh_after_file_op()?;
        Ok(())
    }

    fn trash_selected_file(&mut self) -> Result<()> {
        if !self.is_uncommitted_selected() {
            return Ok(());
        }

        // Only allow trashing untracked files; tracked files should use restore (r)
        let paths: Vec<String> = self
            .selected_files()
            .into_iter()
            .filter(|f| matches!(f.stage_status, Some(StageStatus::Untracked)))
            .map(|f| f.path.to_string_lossy().to_string())
            .collect();
        if paths.is_empty() {
            return Ok(());
        }

        let label = file_count_label(&paths);
        self.mode = AppMode::Confirm {
            message: format!("Move {} to recycle bin?", label),
            action: ConfirmAction::TrashFile(paths),
        };
        Ok(())
    }

    fn restore_selected_file(&mut self) -> Result<()> {
        if !self.is_uncommitted_selected() {
            return Ok(());
        }

        let files: Vec<_> = self.selected_files().into_iter().cloned().collect();
        if files.is_empty() {
            return Ok(());
        }

        let all_new = files.iter().all(|f| {
            matches!(f.kind, FileChangeKind::Added)
                || matches!(f.stage_status, Some(StageStatus::Untracked))
        });

        let paths: Vec<String> = files
            .iter()
            .map(|f| f.path.to_string_lossy().to_string())
            .collect();

        let label = file_count_label(&paths);
        let message = if all_new {
            format!(
                "Delete {}? This file is untracked and will be permanently removed.",
                label
            )
        } else {
            format!("Discard changes to {}?", label)
        };
        self.mode = AppMode::Confirm {
            message,
            action: ConfirmAction::RestoreFile(paths),
        };
        Ok(())
    }

    fn undo_last_file_op(&mut self) -> Result<()> {
        let Some(op) = self.last_undoable_op.take() else {
            self.set_message("Nothing to undo");
            return Ok(());
        };

        match op {
            UndoableOperation::Stage { path, was_staged } => {
                // Reverse: if it was_staged before the toggle, we unstaged it, so re-stage.
                // If it wasn't staged, we staged it, so unstage.
                if was_staged {
                    stage_file(&self.repo_path, &path)?;
                } else {
                    unstage_file(&self.repo_path, &path)?;
                }
                self.set_message(format!("Undid stage/unstage '{}'", path));
            }
            UndoableOperation::Gitignore { pattern } => {
                match remove_from_gitignore(&self.repo_path, &pattern)? {
                    true => self.set_message(format!(
                        "Removed '{}' from .gitignore",
                        pattern
                    )),
                    false => {
                        self.set_message(format!(
                            "'{}' not found in .gitignore",
                            pattern
                        ));
                        return Ok(());
                    }
                }
            }
            UndoableOperation::Archive { relative_path } => {
                unarchive_path(&self.repo_path, &relative_path)?;
                self.set_message(format!("Restored '{}' from archive", relative_path));
            }
        }

        self.refresh_after_file_op()?;
        Ok(())
    }

    pub(crate) fn selected_stash_index(&self) -> Option<usize> {
        let node = self.selected_commit_node()?;
        let label = node.stash_label.as_ref()?;
        label.strip_prefix("stash@{")?.strip_suffix('}')?.parse().ok()
    }
}
