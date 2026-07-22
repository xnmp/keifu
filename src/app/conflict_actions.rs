//! Merge-conflict resolution: accept ours/theirs, continue, abort, and the
//! shared handling of operation outcomes (conflict vs completion).

use super::*;

/// Pure guard for mutating operations (checkout, a new merge/rebase/…, pull,
/// stash pop/apply) that must not run while a history-integrating operation is
/// mid-flight or unmerged paths linger.
///
/// `attempted` is a user-facing verb ("checkout", "pull", …). Returns the
/// status message to show when the action must be blocked, or `None` when it
/// may proceed.
pub(crate) fn op_guard_message(
    op: OperationState,
    conflict_count: usize,
    attempted: &str,
) -> Option<String> {
    if op.is_in_progress() {
        Some(format!(
            "Cannot {attempted} during a {} — resolve conflicts, then Continue (c) or Abort (A)",
            op.verb()
        ))
    } else if conflict_count > 0 {
        // Conflicts remain without a tracked operation (e.g. a conflicting
        // stash pop): there is nothing to Continue/Abort, only resolve.
        Some(format!(
            "Cannot {attempted} with unresolved conflicts — resolve them (o/t) first"
        ))
    } else {
        None
    }
}

/// Pure guard for committing: blocked only while unmerged (conflicted) paths
/// remain. Once every path is resolved, committing (and Continue) is allowed —
/// even mid-merge, where a plain commit legitimately finishes the merge.
pub(crate) fn commit_guard_message(op: OperationState, conflict_count: usize) -> Option<String> {
    if conflict_count == 0 {
        return None;
    }
    if op.is_in_progress() {
        Some("Unresolved conflicts — resolve them (o/t), then Continue (c)".to_string())
    } else {
        Some("Unresolved conflicts — resolve them (o/t) before committing".to_string())
    }
}

/// "<Op> completed" message for a finished history-integrating operation.
fn op_completed_message(op: OperationState) -> String {
    let name = match op {
        OperationState::Merge => "Merge",
        OperationState::Rebase => "Rebase",
        OperationState::CherryPick => "Cherry-pick",
        OperationState::Revert => "Revert",
        OperationState::Clean => "Operation",
    };
    format!("{name} completed")
}

impl App {
    /// Block a mutating operation (`attempted` names it, e.g. "checkout") while
    /// a merge/rebase/cherry-pick/revert is in progress or unmerged paths
    /// remain. Sets a guiding status message and returns `true` when the caller
    /// must abort the action. Conflict-resolution paths (o/t, continue, abort,
    /// staging, refresh) intentionally do not call this.
    pub(crate) fn block_if_op_in_progress(&mut self, attempted: &str) -> bool {
        match op_guard_message(self.op_state, self.conflict_count, attempted) {
            Some(msg) => {
                self.toast(crate::toast::ToastKind::Info, msg);
                true
            }
            None => false,
        }
    }

    /// Block committing while unmerged (conflicted) paths remain. Sets a guiding
    /// status message and returns `true` when the commit must be blocked.
    pub(crate) fn block_commit_if_unmerged(&mut self) -> bool {
        match commit_guard_message(self.op_state, self.conflict_count) {
            Some(msg) => {
                self.toast(crate::toast::ToastKind::Info, msg);
                true
            }
            None => false,
        }
    }

    /// Guided message shown while conflicts remain, naming the resolution keys.
    pub(crate) fn conflict_guidance(count: usize) -> String {
        format!(
            "Conflicts in {} file{} — resolve then Continue (c), or Abort (A)",
            count,
            if count == 1 { "" } else { "s" }
        )
    }

    /// Guidance for a conflicting stash pop/apply. Unlike merge/rebase there is
    /// no MERGE_HEAD and so no Continue/Abort step: the user resolves in place
    /// and drops the (kept) stash from the stash menu when done.
    pub(crate) fn stash_conflict_guidance(verb: &str, count: usize) -> String {
        format!(
            "Stash {} hit conflicts in {} file{} — stash kept; resolve (o/t accept a side), then Drop it from the stash menu (Enter) when done",
            verb,
            count,
            if count == 1 { "" } else { "s" }
        )
    }

    /// React to a stash pop/apply outcome. A stash conflict leaves no operation
    /// in progress (no MERGE_HEAD), so `op_state` stays Clean and the merge-style
    /// Continue/Abort guidance does not apply; on conflicts we only surface the
    /// conflicted files and point the user at stash-specific resolution. `verb`
    /// is the past-tense action for the success message ("applied"/"popped");
    /// `gerund` names the operation in the conflict guidance ("apply"/"pop").
    pub(crate) fn handle_stash_outcome(
        &mut self,
        outcome: OpOutcome,
        verb: &str,
        gerund: &str,
    ) -> Result<()> {
        match outcome {
            OpOutcome::Completed => {
                self.refresh(true)?;
                self.toast(crate::toast::ToastKind::Success, format!("Stash {verb}"));
            }
            OpOutcome::Conflicts { count } => {
                self.refresh(true)?;
                self.focus_conflict_files();
                self.set_message(Self::stash_conflict_guidance(gerund, count));
            }
        }
        Ok(())
    }

    /// React to a merge/rebase/cherry-pick/revert outcome. On conflicts, move
    /// to the uncommitted node's files pane and guide the user; on completion,
    /// confirm success. `op` is the operation that ran (state may already be
    /// clean again on completion).
    pub(crate) fn handle_op_outcome(&mut self, outcome: OpOutcome, op: OperationState) {
        match outcome {
            OpOutcome::Completed => self.toast(crate::toast::ToastKind::Success, op_completed_message(op)),
            OpOutcome::Conflicts { count } => {
                self.focus_conflict_files();
                self.set_message(Self::conflict_guidance(count));
            }
        }
    }

    /// Select the uncommitted node, focus the files pane, and prime its diff so
    /// the Merge Changes section is visible immediately.
    pub(crate) fn focus_conflict_files(&mut self) {
        let has_uncommitted = self
            .graph_layout
            .nodes
            .first()
            .is_some_and(|node| node.is_uncommitted);
        if has_uncommitted {
            self.graph_nav.graph_list_state.select(Some(0));
            self.graph_nav.selected_branch_position = None;
        }
        self.focus_files_pane();
        // The async uncommitted diff may not have run yet; compute the quick
        // file list synchronously so conflicts show without a frame delay.
        self.diff_cache.set_quick_uncommitted(self.repo.repo());
        self.sync_file_list_cache();
    }

    /// Accept our side (`ours = true`) or their side of the selected conflicted
    /// file, then stage it. Only acts on a conflicted file.
    pub(crate) fn accept_conflict_side(&mut self, ours: bool) -> Result<()> {
        if !self.is_uncommitted_selected() {
            return Ok(());
        }
        let Some(file) = self.selected_file().cloned() else {
            return Ok(());
        };
        if file.stage_status != Some(StageStatus::Conflicted) {
            self.toast(crate::toast::ToastKind::Info, "Not a conflicted file (use o/t on a Merge Changes entry)");
            return Ok(());
        }
        let path = file.path.to_string_lossy().to_string();
        if ours {
            accept_ours(&self.repo_path, &path)?;
        } else {
            accept_theirs(&self.repo_path, &path)?;
        }
        self.toast(crate::toast::ToastKind::Success, format!(
            "Accepted {} for '{}'",
            if ours { "ours" } else { "theirs" },
            path
        ));
        self.refresh_after_file_op()?;
        Ok(())
    }

    /// Continue the in-progress operation after conflicts are resolved.
    pub(crate) fn continue_in_progress_operation(&mut self) -> Result<()> {
        let op = self.op_state;
        if !op.is_in_progress() {
            self.toast(crate::toast::ToastKind::Info, "No operation in progress");
            return Ok(());
        }
        match continue_operation(&self.repo_path, op) {
            Ok(OpOutcome::Completed) => {
                self.refresh(true)?;
                self.toast(crate::toast::ToastKind::Success, op_completed_message(op));
            }
            Ok(OpOutcome::Conflicts { count }) => {
                self.refresh(true)?;
                self.focus_conflict_files();
                self.set_message(Self::conflict_guidance(count));
            }
            Err(e) => {
                self.refresh(true)?;
                self.show_error(format!("Continue failed: {e}"));
            }
        }
        Ok(())
    }

    /// Prompt (behind the destructive-op Confirm dialog) to abort the operation.
    pub(crate) fn prompt_abort_operation(&mut self) {
        let op = self.op_state;
        if !op.is_in_progress() {
            self.toast(crate::toast::ToastKind::Info, "No operation in progress");
            return;
        }
        self.mode = AppMode::Confirm {
            message: format!(
                "Abort {}? This discards the in-progress {} and any resolutions.",
                op.verb(),
                op.verb()
            ),
            action: ConfirmAction::AbortOperation(op),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::{commit_guard_message, op_guard_message};
    use crate::app::App;
    use crate::git::{GitRepository, OperationState};
    use crate::test_support::git;

    /// #116: conflict guidance routes the user to the files pane; if that pane
    /// is hidden, it must be un-hidden — focus landing on an invisible pane
    /// would strand input with no visible cue.
    #[test]
    fn conflict_focus_unhides_a_hidden_files_pane() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("f.txt"), "x\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-q", "-m", "init"]);
        let repo = GitRepository::open(tmp.path()).unwrap();
        let mut app = App::from_repo(repo).unwrap();

        app.hide_files_pane = true;
        app.focus_conflict_files();
        assert!(!app.hide_files_pane, "conflict flow must reveal the files pane");
        assert_eq!(app.focused_panel, crate::app::FocusedPanel::Files);
    }

    // ── Pure predicate: op_guard_message ────────────────────────────────

    #[test]
    fn clean_state_blocks_nothing() {
        for attempted in ["checkout", "merge", "pull", "pop a stash"] {
            assert_eq!(
                op_guard_message(OperationState::Clean, 0, attempted),
                None,
                "clean state must not block {attempted}"
            );
        }
    }

    #[test]
    fn in_progress_op_blocks_and_names_the_operation() {
        let cases = [
            (OperationState::Merge, "checkout", "merge"),
            (OperationState::Rebase, "merge", "rebase"),
            (OperationState::CherryPick, "pull", "cherry-pick"),
            (OperationState::Revert, "pop a stash", "revert"),
        ];
        for (op, attempted, op_name) in cases {
            let msg = op_guard_message(op, 1, attempted)
                .unwrap_or_else(|| panic!("{op:?} must block {attempted}"));
            assert!(
                msg.contains(&format!("Cannot {attempted}")),
                "names the attempted action: {msg}"
            );
            assert!(
                msg.contains(op_name),
                "names the in-progress operation ({op_name}): {msg}"
            );
            assert!(
                msg.contains("Continue (c)") && msg.contains("Abort (A)"),
                "points to the way out: {msg}"
            );
        }
    }

    #[test]
    fn conflicts_without_op_still_block_new_operations() {
        // Stash-pop-conflict case: op_state is Clean but unmerged paths exist.
        let msg = op_guard_message(OperationState::Clean, 2, "checkout")
            .expect("lingering conflicts must block a checkout");
        assert!(msg.contains("Cannot checkout"), "names the action: {msg}");
        assert!(
            msg.contains("resolve"),
            "guides toward resolution: {msg}"
        );
    }

    // ── Pure predicate: commit_guard_message ────────────────────────────

    #[test]
    fn commit_allowed_once_conflicts_resolved() {
        // No conflicts -> never blocked, even mid-merge (a plain commit finishes
        // the merge).
        assert_eq!(commit_guard_message(OperationState::Merge, 0), None);
        assert_eq!(commit_guard_message(OperationState::Clean, 0), None);
    }

    #[test]
    fn commit_blocked_while_conflicts_remain() {
        // Mid-merge with conflicts: guide toward Continue.
        let msg = commit_guard_message(OperationState::Merge, 1)
            .expect("commit blocked while unmerged");
        assert!(msg.contains("Unresolved conflicts"), "{msg}");
        assert!(msg.contains("Continue (c)"), "{msg}");

        // Stash-conflict (no op): still blocked, but no Continue to offer.
        let msg = commit_guard_message(OperationState::Clean, 1)
            .expect("commit blocked while unmerged");
        assert!(msg.contains("Unresolved conflicts"), "{msg}");
    }

    // ── Integration: real mid-merge repo ────────────────────────────────
    // `git` (below) deliberately doesn't assert exit codes: the `merge` call
    // in `conflicted_merge_repo` is expected to exit non-zero on conflict.

    /// Build a repo where merging `feature` into `main` conflicts, leaving the
    /// repo mid-merge with an unmerged path.
    fn conflicted_merge_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("f.txt"), "base\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "base"]);
        git(dir, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(dir.join("f.txt"), "feature\n").unwrap();
        git(dir, &["commit", "-q", "-am", "feature change"]);
        git(dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("f.txt"), "main\n").unwrap();
        git(dir, &["commit", "-q", "-am", "main change"]);
        git(dir, &["merge", "feature"]); // conflicts, leaves MERGE_HEAD
        tmp
    }

    #[test]
    fn checkout_is_blocked_mid_merge_and_head_stays_put() {
        let tmp = conflicted_merge_repo();
        let repo = GitRepository::open(tmp.path()).expect("open repo");
        let mut app = App::from_repo(repo).expect("build app");

        // Sanity: the app sees the mid-merge conflicted state.
        assert_eq!(app.op_state, OperationState::Merge);
        assert!(app.conflict_count > 0, "expected unmerged paths");

        let head_before = app.repo.head_oid();
        app.checkout_branch_by_name("feature", false)
            .expect("guarded checkout returns Ok");

        // HEAD did not move, and the user was told why (via a toast).
        assert_eq!(app.repo.head_oid(), head_before, "HEAD must not move");
        let toast = app.toasts.visible().last().expect("guard toast shown");
        assert!(
            toast.text.contains("Cannot checkout during a merge"),
            "guard toast shown: {}",
            toast.text
        );
    }
}
