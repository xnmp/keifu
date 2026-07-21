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
                        self.delete_local_branch_with_undo(&name)?;
                    }
                    // Enter on a local+remote branch deletes the local branch
                    // only; Ctrl+Enter/R (ConfirmDeleteBranchAndRemote, below)
                    // deletes both.
                    ConfirmAction::DeleteBranchWithRemote { name, .. } => {
                        self.delete_local_branch_with_undo(&name)?;
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
                        // Optimistic + async: hide the branch now, delete on the
                        // remote in the background, restore on failure.
                        self.mode = AppMode::Normal;
                        self.start_remote_branch_deletion(remote, branch);
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
            // Secondary confirm (Ctrl+Enter / R): only meaningful for a
            // local+remote delete offer — delete the local branch AND start the
            // optimistic remote deletion. A no-op for any other confirm dialog,
            // leaving it open.
            Action::ConfirmDeleteBranchAndRemote => {
                if let ConfirmAction::DeleteBranchWithRemote { name, remote, branch } =
                    confirm_action
                {
                    self.delete_local_branch_with_undo(&name)?;
                    self.mode = AppMode::Normal;
                    self.refresh(true)?;
                    self.start_remote_branch_deletion(remote, branch);
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

    /// Delete local branch `name`, recording a recreate-at-tip undo entry (the
    /// tip OID is captured before deletion). Shared by the plain delete-branch
    /// confirm and the local+remote delete offer.
    fn delete_local_branch_with_undo(&mut self, name: &str) -> Result<()> {
        // Capture the tip OID before deleting, for a recreate undo.
        let tip = self
            .repo
            .repo()
            .find_branch(name, git2::BranchType::Local)
            .ok()
            .and_then(|b| b.get().target());
        delete_branch(self.repo.repo(), name)?;
        if let Some(oid) = tip {
            self.record_undo(crate::undo::UndoEntry {
                description: format!("Delete branch '{name}'"),
                confirm: format!(
                    "Undo: delete branch '{name}' → recreate at {}?",
                    crate::undo::short_oid(oid)
                ),
                plan: crate::undo::UndoPlan::RecreateBranch {
                    name: name.to_string(),
                    oid,
                },
                check: crate::undo::UndoCheck::BranchAbsent(name.to_string()),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Action;
    use crate::git::{BranchInfo, GitRepository};
    use crate::test_support::git;
    use std::process::Command;

    /// A repo with a committed local `feature` branch published to a bare
    /// `origin` (so `origin/feature` exists as a real remote-tracking ref), plus
    /// a local-only `solo` branch. Returns the tempdir (kept alive for the repo)
    /// and a refreshed App.
    fn app_local_and_remote_feature() -> (tempfile::TempDir, App) {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        let local = root.join("local");
        let origin = root.join("origin.git");
        Command::new("git").args(["init", "-q", "--bare"]).arg(&origin).status().unwrap();
        Command::new("git").args(["init", "-q", "-b", "main"]).arg(&local).status().unwrap();
        git(&local, &["config", "user.email", "t@t.com"]);
        git(&local, &["config", "user.name", "t"]);
        git(&local, &["remote", "add", "origin", origin.to_str().unwrap()]);
        std::fs::write(local.join("a.txt"), "a").unwrap();
        git(&local, &["add", "a.txt"]);
        git(&local, &["commit", "-qm", "init"]);
        git(&local, &["branch", "feature"]);
        git(&local, &["branch", "solo"]);
        git(&local, &["push", "-q", "-u", "origin", "feature"]);
        git(&local, &["fetch", "-q", "origin"]);
        let grepo = GitRepository::open(&local).unwrap();
        let mut app = App::from_repo(grepo).unwrap();
        app.refresh(true).unwrap();
        (td, app)
    }

    fn branch(name: &str, is_remote: bool, upstream: Option<&str>) -> BranchInfo {
        BranchInfo {
            name: name.to_string(),
            is_head: false,
            is_remote,
            upstream: upstream.map(str::to_string),
            tip_oid: git2::Oid::zero(),
            ahead: 0,
            behind: 0,
        }
    }

    // ── #88: dialog routing + text contract ─────────────────────────────

    #[test]
    fn confirm_delete_offers_both_when_branch_exists_on_remote() {
        let (_td, mut app) = app_local_and_remote_feature();
        app.confirm_delete_branch("feature".to_string());
        match &app.mode {
            AppMode::Confirm { message, action } => {
                assert!(
                    matches!(action, ConfirmAction::DeleteBranchWithRemote { remote, branch, .. }
                        if remote == "origin" && branch == "feature"),
                    "expected DeleteBranchWithRemote, got {action:?}"
                );
                // Dialog text advertises the secondary key + its fallback and the
                // remote it targets.
                assert!(message.contains("Ctrl+Enter"), "message: {message}");
                assert!(message.contains('R'), "message: {message}");
                assert!(message.contains("origin/feature"), "message: {message}");
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    #[test]
    fn confirm_delete_local_only_branch_has_no_remote_advertisement() {
        let (_td, mut app) = app_local_and_remote_feature();
        app.confirm_delete_branch("solo".to_string());
        match &app.mode {
            AppMode::Confirm { message, action } => {
                assert!(
                    matches!(action, ConfirmAction::DeleteBranch(name) if name == "solo"),
                    "expected plain DeleteBranch, got {action:?}"
                );
                assert!(!message.contains("Ctrl+Enter"), "message: {message}");
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    // ── #88: remote_counterpart detection ───────────────────────────────

    #[test]
    fn remote_counterpart_finds_upstream_tracked_ref() {
        let (_td, app) = app_local_and_remote_feature();
        assert_eq!(
            app.remote_counterpart("feature"),
            Some(("origin".to_string(), "feature".to_string()))
        );
    }

    #[test]
    fn remote_counterpart_none_for_local_only_branch() {
        let (_td, app) = app_local_and_remote_feature();
        assert_eq!(app.remote_counterpart("solo"), None);
    }

    #[test]
    fn remote_counterpart_falls_back_to_name_match_without_upstream() {
        let (_td, mut app) = app_local_and_remote_feature();
        // A local branch with no configured upstream but a same-named remote ref
        // still counts as having a remote counterpart.
        app.branches = vec![
            branch("x", false, None),
            branch("origin/x", true, None),
        ];
        assert_eq!(
            app.remote_counterpart("x"),
            Some(("origin".to_string(), "x".to_string()))
        );
    }

    // ── #88: secondary-confirm action routing ───────────────────────────

    #[test]
    fn secondary_confirm_is_noop_on_plain_delete_branch() {
        let (_td, mut app) = app_local_and_remote_feature();
        app.mode = AppMode::Confirm {
            message: "Delete branch 'solo'?".to_string(),
            action: ConfirmAction::DeleteBranch("solo".to_string()),
        };
        app.handle_confirm_action(Action::ConfirmDeleteBranchAndRemote).unwrap();
        // No remote to delete: the dialog stays open and nothing is scheduled.
        assert!(matches!(app.mode, AppMode::Confirm { .. }));
        assert!(app.pending_remote_deletions.is_empty());
        assert!(!app.is_pushing());
    }

    #[test]
    fn secondary_confirm_deletes_local_and_optimistically_removes_remote() {
        let (_td, mut app) = app_local_and_remote_feature();
        app.confirm_delete_branch("feature".to_string());
        app.handle_confirm_action(Action::ConfirmDeleteBranchAndRemote).unwrap();

        // Local branch deleted synchronously.
        assert!(
            app.repo.repo().find_branch("feature", git2::BranchType::Local).is_err(),
            "local feature should be gone"
        );
        // Optimistic: remote hidden now, deletion dispatched (in flight).
        assert!(app.pending_remote_deletions.contains("origin/feature"));
        assert!(app.is_pushing());
        assert!(matches!(app.mode, AppMode::Normal));

        // Drain the (local, file://) push to completion.
        for _ in 0..1000 {
            if app.update_push_status() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!app.is_pushing());
        assert!(app.pending_remote_deletions.is_empty());
        assert!(
            app.repo.repo().find_branch("origin/feature", git2::BranchType::Remote).is_err(),
            "origin/feature should be gone after a successful delete"
        );
        assert!(
            app.toasts.visible().iter().any(|t| t.text.contains("Deleted origin/feature")),
            "expected success toast"
        );
    }

    // ── #89: optimistic delete — failure restores the branch ────────────

    #[test]
    fn failed_remote_delete_restores_branch_and_toasts_error() {
        let (_td, mut app) = app_local_and_remote_feature();
        // Simulate an in-flight optimistic delete of origin/feature…
        app.pending_remote_deletions.insert("origin/feature".to_string());
        app.in_flight_op = Some(InFlightOp {
            op: RetryableOp::Push(PushSpec::Delete {
                remote: "origin".to_string(),
                branch: "feature".to_string(),
            }),
            host: None,
            had_creds: false,
            silent: false,
            attempts: 0,
        });
        // …that fails on the remote (deterministic, no real push).
        app.network.complete_push_for_test(Err("remote rejected".to_string()));
        assert!(app.update_push_status());

        // The optimistic hide is undone and the branch is back in the graph.
        assert!(app.pending_remote_deletions.is_empty());
        assert!(
            app.branches.iter().any(|b| b.is_remote && b.name == "origin/feature"),
            "origin/feature should reappear after a failed delete"
        );
        assert!(
            app.toasts.visible().iter().any(|t| {
                matches!(t.kind, crate::toast::ToastKind::Error)
                    && t.text.contains("origin/feature")
            }),
            "expected an error toast naming the branch"
        );
    }

    #[test]
    fn successful_remote_delete_clears_pending_and_toasts_success() {
        let (_td, mut app) = app_local_and_remote_feature();
        app.pending_remote_deletions.insert("origin/feature".to_string());
        app.in_flight_op = Some(InFlightOp {
            op: RetryableOp::Push(PushSpec::Delete {
                remote: "origin".to_string(),
                branch: "feature".to_string(),
            }),
            host: None,
            had_creds: false,
            silent: false,
            attempts: 0,
        });
        app.network.complete_push_for_test(Ok(()));
        assert!(app.update_push_status());

        assert!(app.pending_remote_deletions.is_empty());
        assert!(
            app.toasts.visible().iter().any(|t| {
                matches!(t.kind, crate::toast::ToastKind::Success)
                    && t.text.contains("Deleted origin/feature")
            }),
            "expected a success toast"
        );
    }

    // ── Edge case: current branch stays undeletable ─────────────────────

    #[test]
    fn deleting_current_branch_is_blocked() {
        let (_td, mut app) = app_local_and_remote_feature();
        // HEAD is on `main`. Even if a delete-current confirm is forced (the
        // picker normally filters HEAD out), the operation must refuse.
        assert_eq!(
            app.branches.iter().find(|b| b.is_head).map(|b| b.name.as_str()),
            Some("main")
        );
        app.mode = AppMode::Confirm {
            message: "Delete branch 'main'?".to_string(),
            action: ConfirmAction::DeleteBranch("main".to_string()),
        };
        assert!(
            app.handle_confirm_action(Action::Confirm).is_err(),
            "deleting the current branch must fail"
        );
        assert!(
            app.repo.repo().find_branch("main", git2::BranchType::Local).is_ok(),
            "current branch must still exist after a blocked delete"
        );
    }
}
