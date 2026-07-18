//! Network operations: fetch/pull/push, auto-refresh, fs-watcher polling.

use super::*;

impl App {
    pub fn update_fetch_status(&mut self) -> bool {
        let Some(result) = self.network.poll_fetch() else {
            return false;
        };
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
                            let new_head = self.repo.head_oid();
                            let new_branch_count = self.branches.len();
                            if prev_head != new_head || prev_branch_count != new_branch_count {
                                self.set_message("Fetched from origin");
                            }
                        }
                        Err(e) => self.show_error(format!("Refresh failed: {e}")),
                    }
                }
            }
            Err(e) => self.show_git_error(e),
        }
        true
    }

    pub fn update_push_status(&mut self) -> bool {
        let Some(result) = self.network.poll_push() else {
            return false;
        };
        match result {
            Ok(()) => {
                self.set_message("Pushed");
                if let Err(e) = self.refresh(true) {
                    self.show_error(format!("Refresh failed: {e}"));
                }
            }
            Err(e) => self.show_git_error(e),
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
        match result {
            Ok(outcome) => {
                self.network.reset_timers();
                if let Err(e) = self.refresh(true) {
                    self.show_error(format!("Refresh failed: {e}"));
                    return true;
                }
                match outcome {
                    OpOutcome::Completed => self.set_message("Pulled"),
                    OpOutcome::Conflicts { count } => {
                        self.focus_conflict_files();
                        self.set_message(Self::conflict_guidance(count));
                    }
                }
            }
            Err(e) => {
                // A --ff-only pull that fails on divergence isn't an error to
                // surface — offer merge/rebase instead.
                if is_divergent_pull_error(&e) && self.last_pull.is_some() {
                    self.mode = AppMode::PullDivergence { selected: 0 };
                } else {
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
    /// map changed (so badges re-render). Never blocks the UI thread.
    pub fn update_open_prs(&mut self) -> bool {
        self.pr_fetch.maybe_start(&self.repo_path);
        if let Some(prs) = self.pr_fetch.poll() {
            self.open_prs = prs;
            true
        } else {
            false
        }
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
        if let Some(msg) = self
            .network
            .start_fetch(&self.repo_path, &remote, show_message, silent)
        {
            self.set_message(msg);
        }
    }

    pub(crate) fn start_fetch_all(&mut self) {
        let msg = self.network.start_fetch_all(&self.repo_path);
        self.set_message(msg);
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
        let msg = self.network.start_push(&self.repo_path, PushSpec::Current);
        self.set_message(msg);
    }

    pub(crate) fn start_publish(&mut self, remote: String, branch: String) {
        let msg = self
            .network
            .start_push(&self.repo_path, PushSpec::Publish { remote, branch });
        self.set_message(msg);
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
        let msg = self.network.start_pull(&self.repo_path, remote, branch, mode);
        self.set_message(msg);
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
