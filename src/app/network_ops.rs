//! Network operations: fetch/pull/push, auto-refresh, fs-watcher polling.

use super::*;
use crate::toast::ToastKind;

impl App {
    pub fn update_fetch_status(&mut self) -> bool {
        let Some((result, silent)) = self.network.poll_fetch() else {
            return false;
        };
        let flight = self.in_flight_op.take();
        match result {
            Ok(()) => {
                self.network.reset_timers();
                if matches!(self.mode, AppMode::FileDiff { .. }) {
                    self.pending_refresh = true;
                } else {
                    let prev_head = self.repo.head_oid();
                    let prev_branch_count = self.branches.len();
                    match self.refresh(true) {
                        Ok(()) => {
                            let changed = self.repo.head_oid() != prev_head
                                || self.branches.len() != prev_branch_count;
                            // Only user-initiated fetches (manual `f` / F5) toast;
                            // silent auto-fetch stays quiet on success.
                            if !silent {
                                if changed {
                                    self.toast(ToastKind::Success, "Fetched — graph updated");
                                } else {
                                    self.toast(ToastKind::Info, "Fetched — up to date");
                                }
                            }
                        }
                        Err(e) => self.show_error(format!("Refresh failed: {e}")),
                    }
                }
            }
            // A silent auto-fetch error was previously swallowed; surface it as a
            // toast. A user-initiated fetch keeps the full error dialog. An HTTPS
            // auth failure on a user-initiated fetch opens the credential prompt.
            Err(e) => {
                if self.try_prompt_credentials(&e, flight) {
                    // Prompt opened.
                } else if silent {
                    self.toast(ToastKind::Error, format!("Auto-fetch failed: {e}"));
                } else {
                    self.show_git_error(e);
                }
            }
        }
        true
    }

    pub fn update_push_status(&mut self) -> bool {
        let Some(result) = self.network.poll_push() else {
            return false;
        };
        // The "Pushing…" progress message has served its purpose; clear it so it
        // can't be resurrected by a later network op.
        self.clear_progress_message();
        let flight = self.in_flight_op.take();
        match result {
            Ok(()) => {
                self.toast(ToastKind::Success, "Pushed");
                if let Err(e) = self.refresh(true) {
                    self.show_error(format!("Refresh failed: {e}"));
                }
            }
            Err(e) => {
                if !self.try_prompt_credentials(&e, flight) {
                    self.show_git_error(e);
                }
            }
        }
        true
    }

    /// Poll the background pull. On success, refresh and either confirm or (on
    /// conflict) route into the guided resolve flow — op-state detection picks
    /// up the MERGE/REBASE the pull left behind during refresh.
    pub fn update_pull_status(&mut self) -> bool {
        let Some(result) = self.network.poll_pull() else {
            return false;
        };
        // The "Pulling…" progress message has served its purpose; clear it so it
        // can't be resurrected by a later network op.
        self.clear_progress_message();
        let flight = self.in_flight_op.take();
        match result {
            Ok(outcome) => {
                self.network.reset_timers();
                if let Err(e) = self.refresh(true) {
                    self.show_error(format!("Refresh failed: {e}"));
                    return true;
                }
                match outcome {
                    OpOutcome::Completed => {
                        // If the pull (fast-forward or merge) moved HEAD, record a
                        // reset-back undo. The pre-pull HEAD was snapshotted at
                        // launch (the op is async).
                        if let (Some(pre), Some(post)) = (self.pre_pull_head, self.repo.head_oid()) {
                            if pre != post {
                                self.record_undo(crate::undo::UndoEntry {
                                    description: "Pull".to_string(),
                                    confirm: format!(
                                        "Undo: pull → reset to {}?",
                                        crate::undo::short_oid(pre)
                                    ),
                                    plan: crate::undo::UndoPlan::ResetHard { to: pre },
                                    check: crate::undo::UndoCheck::HeadAtCleanTree(post),
                                });
                            }
                        }
                        self.pre_pull_head = None;
                        self.toast(ToastKind::Success, "Pulled");
                    }
                    OpOutcome::Conflicts { count } => {
                        self.focus_conflict_files();
                        // Keep the workflow guidance in the status bar; toast the
                        // (easily-missed) conflict outcome prominently.
                        self.set_message(Self::conflict_guidance(count));
                        self.toast(
                            ToastKind::Error,
                            format!("Pulled with {count} conflict{}", if count == 1 { "" } else { "s" }),
                        );
                    }
                }
            }
            Err(e) => {
                // A --ff-only pull that fails on divergence isn't an error to
                // surface — offer merge/rebase instead.
                if is_divergent_pull_error(&e) && self.last_pull.is_some() {
                    self.mode = AppMode::PullDivergence { selected: 0 };
                } else if !self.try_prompt_credentials(&e, flight) {
                    self.show_git_error(e);
                }
            }
        }
        true
    }

    pub fn is_fetching(&self) -> bool {
        self.network.is_fetching()
    }

    pub fn is_pushing(&self) -> bool {
        self.network.is_pushing()
    }

    pub fn is_pulling(&self) -> bool {
        self.network.is_pulling()
    }

    pub fn is_network_busy(&self) -> bool {
        self.network.is_busy()
    }

    pub fn check_auto_refresh(&mut self) -> bool {
        if matches!(self.mode, AppMode::FileDiff { .. }) {
            return false;
        }
        let events = self.network.check_auto_timers(&self.config.refresh);
        if events.should_auto_fetch {
            if let Some(remote) = self.auto_fetch_remote() {
                self.start_fetch_remote(remote, false, true);
                true
            } else {
                // No remote to fetch — reset the timer so we don't spin.
                self.network.reset_timers();
                false
            }
        } else if events.should_auto_refresh {
            if let Err(e) = self.refresh(false) {
                self.set_message(format!("Auto-refresh failed: {e}"));
            }
            self.network.mark_refreshed();
            true
        } else {
            false
        }
    }

    /// Kick off / poll the background open-PR fetch. Returns true when the PR
    /// map changed (so badges re-render). Toasts a concise summary when a PR
    /// appears or a CI status changes — but not on the initial load, and not on
    /// no-op 5-minute refreshes. Never blocks the UI thread.
    pub fn update_open_prs(&mut self) -> bool {
        self.pr_fetch.maybe_start(&self.repo_path);
        let Some(prs) = self.pr_fetch.poll() else {
            return false;
        };
        if self.pr_toasts_armed {
            if let Some(summary) = crate::pr::pr_refresh_summary(&self.open_prs, &prs) {
                self.toast(ToastKind::Info, summary);
            }
        }
        // Arm after the first successful fill so the startup population is quiet.
        self.pr_toasts_armed = true;
        self.open_prs = prs;
        true
    }

    pub fn poll_fs_watcher(&mut self) -> bool {
        if let Some(pending) = self.pending_watcher.as_mut() {
            if let Some(watcher) = pending.try_take() {
                self.watcher = watcher;
                self.pending_watcher = None;
            }
        }
        if !self.config.refresh.auto_refresh {
            return false;
        }
        if matches!(self.mode, AppMode::FileDiff { .. }) {
            return false;
        }
        let Some(watcher) = self.watcher.as_mut() else {
            return false;
        };
        match watcher.poll() {
            crate::watcher::PollResult::Refresh => {
                if let Err(e) = self.refresh(false) {
                    self.set_message(format!("Watch refresh failed: {e}"));
                }
                self.network.mark_refreshed();
                true
            }
            crate::watcher::PollResult::Disconnected => {
                self.set_message("Filesystem watcher disconnected".to_string());
                self.watcher = None;
                true
            }
            crate::watcher::PollResult::Idle => false,
        }
    }

    // ── Thin wrappers over NetworkManager ──────────────────────────────

    pub(crate) fn start_fetch_remote(&mut self, remote: String, show_message: bool, silent: bool) {
        self.dispatch_net_op(
            RetryableOp::Fetch { remote, show_message, silent },
            0,
        );
    }

    pub(crate) fn start_fetch_all(&mut self) {
        self.dispatch_net_op(RetryableOp::FetchAll, 0);
    }

    /// F5 "full update": force an immediate PR refetch, refresh the graph/status
    /// now for instant feedback, and kick off a background fetch of every
    /// remote (its completion triggers another refresh). Fully async; a fetch
    /// already in flight is left alone rather than duplicated.
    pub(crate) fn full_update(&mut self) {
        // Skip the PR fetch's 5-min interval on the next poll.
        self.pr_fetch.force();

        if let Err(e) = self.refresh(true) {
            self.show_error(format!("Refresh failed: {e}"));
        }
        self.reset_timers();

        if self.network.is_busy() {
            return;
        }
        if self.repo.remotes().is_empty() {
            return;
        }
        self.start_fetch_all();
    }

    pub(crate) fn start_push_current(&mut self) {
        self.dispatch_net_op(RetryableOp::Push(PushSpec::Current), 0);
    }

    pub(crate) fn start_publish(&mut self, remote: String, branch: String) {
        self.dispatch_net_op(RetryableOp::Push(PushSpec::Publish { remote, branch }), 0);
    }

    /// Push HEAD to an explicit remote without changing upstream tracking
    /// (`git push <remote> HEAD`).
    pub(crate) fn start_push_head_to(&mut self, remote: String) {
        self.dispatch_net_op(RetryableOp::Push(PushSpec::ToRemote { remote }), 0);
    }

    /// Start a pull with strategy `mode`. `remote = None` uses the branch's
    /// configured upstream; an explicit remote pulls `<remote> <current-branch>`.
    pub(crate) fn start_pull_remote(&mut self, remote: Option<String>, mode: PullMode) {
        let branch = if remote.is_some() {
            self.head_branch_info().map(|b| b.name.clone())
        } else {
            None
        };
        self.start_pull_with(remote, branch, mode);
    }

    /// Kick off the async pull, remembering (remote, branch) so a divergence
    /// prompt can rerun it with an explicit merge/rebase strategy.
    fn start_pull_with(&mut self, remote: Option<String>, branch: Option<String>, mode: PullMode) {
        self.last_pull = Some((remote.clone(), branch.clone()));
        // Snapshot HEAD so a completed pull that moved it can record an undo.
        self.pre_pull_head = self.repo.head_oid();
        self.dispatch_net_op(RetryableOp::Pull { remote, branch, mode }, 0);
    }

    /// Rerun the last pull with an explicit strategy after the divergence
    /// prompt. Respects the busy guard so it never launches a second concurrent
    /// pull.
    pub(crate) fn rerun_pull_with_mode(&mut self, mode: PullMode) {
        if self.network.is_busy() {
            self.set_message("busy: pull in progress");
            return;
        }
        let Some((remote, branch)) = self.last_pull.clone() else {
            return;
        };
        self.start_pull_with(remote, branch, mode);
    }

    /// Show a git failure with a humanized one-liner (when recognized) plus the
    /// raw stderr below for debuggability.
    pub(crate) fn show_git_error(&mut self, raw: String) {
        let msg = match humanize_git_error(&raw) {
            Some(human) => format!("{human}\n\n{}", raw.trim()),
            None => raw,
        };
        self.show_error(msg);
    }

    pub(crate) fn reset_timers(&mut self) {
        self.network.reset_timers();
    }
}
