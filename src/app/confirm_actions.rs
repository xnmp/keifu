//! Confirmation mode for destructive operations.

use super::*;

impl App {
    pub(crate) fn handle_confirm_action(&mut self, action: Action) -> Result<()> {
        let AppMode::Confirm {
            action: confirm_action,
            ..
        } = &self.mode
        else {
            return Ok(());
        };
        let confirm_action = confirm_action.clone();

        match action {
            Action::Confirm => {
                match confirm_action {
                    ConfirmAction::DeleteBranch(name) => {
                        delete_branch(self.repo.repo(), &name)?;
                    }
                    ConfirmAction::Merge(name) => {
                        merge_branch(self.repo.repo(), &name)?;
                    }
                    ConfirmAction::Rebase(name) => {
                        rebase_branch(self.repo.repo(), &name)?;
                    }
                    ConfirmAction::CherryPick(oid) => {
                        cherry_pick(&self.repo_path, oid)?;
                    }
                    ConfirmAction::Revert(oid) => {
                        revert_commit(&self.repo_path, oid)?;
                    }
                    ConfirmAction::ResetSoft(oid) => {
                        reset_to_commit(&self.repo_path, oid, ResetMode::Soft)?;
                    }
                    ConfirmAction::ResetMixed(oid) => {
                        reset_to_commit(&self.repo_path, oid, ResetMode::Mixed)?;
                    }
                    ConfirmAction::ResetHard(oid) => {
                        reset_to_commit(&self.repo_path, oid, ResetMode::Hard)?;
                    }
                    ConfirmAction::Push => {
                        self.start_push();
                    }
                    ConfirmAction::RestoreFile(paths) => {
                        restore_files(&self.repo_path, &paths)?;
                        let label = file_count_label(&paths);
                        self.set_message(format!("Restored {}", label));
                        self.mode = AppMode::Normal;
                        self.refresh_after_file_op()?;
                        return Ok(());
                    }
                    ConfirmAction::TrashFile(paths) => {
                        let mut errors = Vec::new();
                        for path in &paths {
                            let full = std::path::Path::new(&self.repo_path).join(path);
                            if let Err(e) = trash::delete(&full) {
                                errors.push(format!("{}: {}", path, e));
                            }
                        }
                        if errors.is_empty() {
                            let label = file_count_label(&paths);
                            self.set_message(format!("Moved {} to recycle bin", label));
                        } else {
                            self.set_message(format!("Trash errors: {}", errors.join("; ")));
                        }
                        self.mode = AppMode::Normal;
                        self.refresh_after_file_op()?;
                        return Ok(());
                    }
                    ConfirmAction::StashDrop(index) => {
                        stash_drop(&self.repo_path, index)?;
                        self.set_message(format!("Dropped stash@{{{}}}", index));
                    }
                    ConfirmAction::DiscardHunk {
                        patch,
                        file_path,
                        scroll_offset,
                    } => {
                        apply_patch_worktree_reverse(&self.repo_path, &patch)?;
                        self.set_message("Discarded hunk");
                        // Reopen the diff viewer where we left off instead of
                        // falling through to Normal mode.
                        self.reload_file_diff_for_path(&file_path, scroll_offset)?;
                        return Ok(());
                    }
                }
                self.refresh(true)?;
                self.mode = AppMode::Normal;
            }
            Action::Cancel => {
                // A discard-hunk prompt was launched from the diff viewer;
                // dismissing it should return there, not drop to Normal.
                if let ConfirmAction::DiscardHunk {
                    file_path,
                    scroll_offset,
                    ..
                } = confirm_action
                {
                    self.reopen_file_diff_for_path(&file_path, scroll_offset)?;
                } else {
                    self.mode = AppMode::Normal;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
