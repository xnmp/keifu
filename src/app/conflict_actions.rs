//! Merge-conflict resolution: accept ours/theirs, continue, abort, and the
//! shared handling of operation outcomes (conflict vs completion).

use super::*;

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
                self.set_message(format!("Stash {verb}"));
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
            OpOutcome::Completed => self.set_message(op_completed_message(op)),
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
        self.focused_panel = FocusedPanel::Files;
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
            self.set_message("Not a conflicted file (use o/t on a Merge Changes entry)");
            return Ok(());
        }
        let path = file.path.to_string_lossy().to_string();
        if ours {
            accept_ours(&self.repo_path, &path)?;
        } else {
            accept_theirs(&self.repo_path, &path)?;
        }
        self.set_message(format!(
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
            self.set_message("No operation in progress");
            return Ok(());
        }
        match continue_operation(&self.repo_path, op) {
            Ok(OpOutcome::Completed) => {
                self.refresh(true)?;
                self.set_message(op_completed_message(op));
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
            self.set_message("No operation in progress");
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
