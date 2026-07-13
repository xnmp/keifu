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
                // Set when a history-integrating op runs, so we can guide the
                // user through conflicts (or confirm success) after refresh.
                let mut op_outcome: Option<(OpOutcome, OperationState)> = None;
                match confirm_action {
                    ConfirmAction::DeleteBranch(name) => {
                        delete_branch(self.repo.repo(), &name)?;
                    }
                    ConfirmAction::Merge(name) => {
                        let outcome = merge_branch(self.repo.repo(), &name)?;
                        op_outcome = Some((outcome, OperationState::Merge));
                    }
                    ConfirmAction::Rebase(name) => {
                        let outcome = rebase_branch(self.repo.repo(), &name)?;
                        op_outcome = Some((outcome, OperationState::Rebase));
                    }
                    ConfirmAction::CherryPick(oid) => {
                        let outcome = cherry_pick(&self.repo_path, oid)?;
                        op_outcome = Some((outcome, OperationState::CherryPick));
                    }
                    ConfirmAction::Revert(oid) => {
                        let outcome = revert_commit(&self.repo_path, oid)?;
                        op_outcome = Some((outcome, OperationState::Revert));
                    }
                    ConfirmAction::AbortOperation(op) => {
                        abort_operation(&self.repo_path, op)?;
                        self.refresh(true)?;
                        self.set_message(format!("{} aborted", op.verb()));
                        self.mode = AppMode::Normal;
                        return Ok(());
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
                }
                self.refresh(true)?;
                self.mode = AppMode::Normal;
                if let Some((outcome, op)) = op_outcome {
                    self.handle_op_outcome(outcome, op);
                }
            }
            Action::Cancel => {
                self.mode = AppMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }
}
