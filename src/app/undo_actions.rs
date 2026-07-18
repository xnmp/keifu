//! Graph-scope undo: verify a recorded entry against the live repo, confirm,
//! and run the inverse. Kept entirely separate from the files-pane file-op undo
//! (`undo_last_file_op` / `last_undoable_op`), which is untouched.

use super::*;
use crate::git::operations::is_working_tree_clean;
use crate::undo::{UndoCheck, UndoEntry, UndoPlan};

impl App {
    /// Record a reversible operation. Called right after an op succeeds.
    pub(crate) fn record_undo(&mut self, entry: UndoEntry) {
        self.undo_ledger.record(entry);
    }

    /// Ctrl+Z (graph scope): verify the newest entry still matches the repo and,
    /// if so, raise the undo confirmation; otherwise drop it — never guess.
    pub(crate) fn request_undo(&mut self) {
        let Some(entry) = self.undo_ledger.peek() else {
            self.toast(crate::toast::ToastKind::Info, "Nothing to undo");
            return;
        };
        let check = entry.check.clone();
        let confirm = entry.confirm.clone();
        match self.verify_undo(&check) {
            Ok(()) => {
                self.mode = AppMode::Confirm {
                    message: confirm,
                    action: ConfirmAction::Undo,
                };
            }
            Err(reason) => {
                self.undo_ledger.pop();
                self.show_error(format!("Cannot undo — {reason}. Entry discarded."));
            }
        }
    }

    /// Confirmed undo: re-verify (authoritative), consume the entry, run the
    /// inverse, refresh, and toast. Any failure drops the entry and reports.
    pub(crate) fn confirm_undo(&mut self) -> Result<()> {
        let Some(entry) = self.undo_ledger.peek() else {
            self.mode = AppMode::Normal;
            return Ok(());
        };
        let check = entry.check.clone();
        let plan = entry.plan.clone();
        let description = entry.description.clone();
        self.mode = AppMode::Normal;

        // Re-check at execution time: state may have shifted between the Ctrl+Z
        // preview and this confirmation.
        if let Err(reason) = self.verify_undo(&check) {
            self.undo_ledger.pop();
            self.show_error(format!("Cannot undo — {reason}. Entry discarded."));
            return Ok(());
        }

        // Consume the entry before executing (no retry loop on failure).
        self.undo_ledger.pop();
        match self.execute_undo_plan(&plan) {
            Ok(()) => {
                self.refresh(true)?;
                self.toast(crate::toast::ToastKind::Success, format!("Undone: {description}"));
            }
            Err(e) => self.show_error(format!("Undo failed: {e}")),
        }
        Ok(())
    }

    /// Verify the repo still matches what the recorded op left behind. Returns a
    /// human-readable reason on mismatch.
    fn verify_undo(&self, check: &UndoCheck) -> std::result::Result<(), String> {
        let repo = self.repo.repo();
        match check {
            UndoCheck::BranchAbsent(name) => {
                if repo.find_branch(name, git2::BranchType::Local).is_ok() {
                    return Err(format!("branch '{name}' exists again"));
                }
            }
            UndoCheck::TagAbsent(name) => {
                if repo
                    .find_reference(&format!("refs/tags/{name}"))
                    .is_ok()
                {
                    return Err(format!("tag '{name}' exists again"));
                }
            }
            UndoCheck::HeadAtCleanTree(oid) => {
                if self.repo.head_oid() != Some(*oid) {
                    return Err("HEAD has moved since".to_string());
                }
                if !is_working_tree_clean(repo).unwrap_or(false) {
                    return Err("the working tree has uncommitted changes".to_string());
                }
            }
            UndoCheck::RenameConsistent { exists, absent } => {
                if repo.find_branch(exists, git2::BranchType::Local).is_err() {
                    return Err(format!("branch '{exists}' no longer exists"));
                }
                if repo.find_branch(absent, git2::BranchType::Local).is_ok() {
                    return Err(format!("branch '{absent}' exists again"));
                }
            }
        }
        Ok(())
    }

    /// Execute the inverse operation via the existing git ops.
    fn execute_undo_plan(&mut self, plan: &UndoPlan) -> Result<()> {
        match plan {
            UndoPlan::RecreateBranch { name, oid } => {
                create_branch(self.repo.repo(), name, *oid)
            }
            UndoPlan::RecreateTag { name, oid, .. } => {
                create_lightweight_tag(self.repo.repo(), name, *oid)
            }
            UndoPlan::ResetHard { to } => reset_hard_checked(self.repo.repo(), *to),
            UndoPlan::RenameBranch { from, to } => {
                rename_branch(&self.repo_path, from, to)
            }
        }
    }
}
