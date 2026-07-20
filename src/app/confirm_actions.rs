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
                    ConfirmAction::Checkout { name, is_remote } => {
                        self.mode = AppMode::Normal;
                        self.checkout_branch_by_name(&name, is_remote)?;
                        return Ok(());
                    }
                    ConfirmAction::LoadAllCommits => {
                        self.mode = AppMode::Normal;
                        self.load_more_commits(true);
                        return Ok(());
                    }
                    ConfirmAction::Undo => {
                        // Re-verifies, executes the inverse, refreshes, toasts.
                        self.confirm_undo()?;
                        return Ok(());
                    }
                    ConfirmAction::DeleteBranch(name) => {
                        // Capture the tip OID before deleting, for a recreate undo.
                        let tip = self
                            .repo
                            .repo()
                            .find_branch(&name, git2::BranchType::Local)
                            .ok()
                            .and_then(|b| b.get().target());
                        delete_branch(self.repo.repo(), &name)?;
                        if let Some(oid) = tip {
                            self.record_undo(crate::undo::UndoEntry {
                                description: format!("Delete branch '{name}'"),
                                confirm: format!(
                                    "Undo: delete branch '{name}' → recreate at {}?",
                                    crate::undo::short_oid(oid)
                                ),
                                plan: crate::undo::UndoPlan::RecreateBranch {
                                    name: name.clone(),
                                    oid,
                                },
                                check: crate::undo::UndoCheck::BranchAbsent(name.clone()),
                            });
                        }
                    }
                    ConfirmAction::Merge { name, is_remote } => {
                        // Snapshot HEAD so a clean merge can be reset away.
                        let pre_head = self.repo.head_oid();
                        let branch_type = if is_remote {
                            git2::BranchType::Remote
                        } else {
                            git2::BranchType::Local
                        };
                        let outcome = merge_branch(self.repo.repo(), &name, branch_type)?;
                        op_outcome = Some((outcome, OperationState::Merge));
                        if outcome == OpOutcome::Completed {
                            if let (Some(pre), Some(post)) = (pre_head, self.repo.head_oid()) {
                                if pre != post {
                                    self.record_undo(crate::undo::UndoEntry {
                                        description: format!("Merge '{name}'"),
                                        confirm: format!(
                                            "Undo: merge '{name}' → reset to {}?",
                                            crate::undo::short_oid(pre)
                                        ),
                                        plan: crate::undo::UndoPlan::ResetHard { to: pre },
                                        check: crate::undo::UndoCheck::HeadAtCleanTree(post),
                                    });
                                }
                            }
                        }
                    }
                    ConfirmAction::Rebase { name, is_remote } => {
                        let branch_type = if is_remote {
                            git2::BranchType::Remote
                        } else {
                            git2::BranchType::Local
                        };
                        let outcome = rebase_branch(self.repo.repo(), &name, branch_type)?;
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
                        self.toast(crate::toast::ToastKind::Success, format!("{} aborted", op.verb()));
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
                        self.mode = AppMode::Normal;
                        self.initiate_push();
                        return Ok(());
                    }
                    ConfirmAction::DeleteRemoteBranch { remote, branch } => {
                        delete_remote_branch(&self.repo_path, &remote, &branch)?;
                        self.refresh(true)?;
                        self.toast(crate::toast::ToastKind::Success, format!("Deleted {remote}/{branch}"));
                        self.mode = AppMode::Normal;
                        return Ok(());
                    }
                    ConfirmAction::RestoreFile(paths) => {
                        restore_files(&self.repo_path, &paths)?;
                        let label = file_count_label(&paths);
                        self.toast(crate::toast::ToastKind::Success, format!("Restored {}", label));
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
                            self.toast(crate::toast::ToastKind::Success, format!("Moved {} to recycle bin", label));
                        } else {
                            self.toast(crate::toast::ToastKind::Error, format!("Trash errors: {}", errors.join("; ")));
                        }
                        self.mode = AppMode::Normal;
                        self.refresh_after_file_op()?;
                        return Ok(());
                    }
                    ConfirmAction::StashDrop(index) => {
                        stash_drop(&self.repo_path, index)?;
                        self.toast(crate::toast::ToastKind::Success, format!("Dropped stash@{{{}}}", index));
                    }
                    ConfirmAction::DeleteTag(name) => {
                        // Capture the target commit + whether it's annotated,
                        // before deletion, for a lightweight-recreate undo.
                        let (target, annotated) = {
                            let repo = self.repo.repo();
                            let target = repo
                                .find_reference(&format!("refs/tags/{name}"))
                                .ok()
                                .and_then(|r| r.peel_to_commit().ok())
                                .map(|c| c.id());
                            (target, is_annotated_tag(repo, &name))
                        };
                        delete_tag(&self.repo_path, &name)?;
                        self.toast(crate::toast::ToastKind::Success, format!("Deleted tag '{}'", name));
                        if let Some(oid) = target {
                            let suffix = if annotated { " as a lightweight tag" } else { "" };
                            self.record_undo(crate::undo::UndoEntry {
                                description: format!("Delete tag '{name}'"),
                                confirm: format!(
                                    "Undo: delete tag '{name}' → recreate at {}{suffix}?",
                                    crate::undo::short_oid(oid)
                                ),
                                plan: crate::undo::UndoPlan::RecreateTag {
                                    name: name.clone(),
                                    oid,
                                    was_annotated: annotated,
                                },
                                check: crate::undo::UndoCheck::TagAbsent(name.clone()),
                            });
                        }
                    }
                    ConfirmAction::DiscardHunk {
                        patch,
                        file_path,
                        scroll_offset,
                    } => {
                        apply_patch_worktree_reverse(&self.repo_path, &patch)?;
                        self.toast(crate::toast::ToastKind::Success, "Discarded hunk");
                        // Reopen the diff viewer where we left off instead of
                        // falling through to Normal mode.
                        self.reload_file_diff_for_path(&file_path, scroll_offset)?;
                        return Ok(());
                    }
                    ConfirmAction::PrAction(action) => {
                        // Runs asynchronously; skip the synchronous refresh below.
                        self.run_pr_action(action);
                        return Ok(());
                    }
                    ConfirmAction::IssueAction(action) => {
                        // Runs asynchronously; skip the synchronous refresh below.
                        self.run_issue_action(action);
                        return Ok(());
                    }
                }
                self.refresh(true)?;
                self.mode = AppMode::Normal;
                if let Some((outcome, op)) = op_outcome {
                    self.handle_op_outcome(outcome, op);
                }
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
