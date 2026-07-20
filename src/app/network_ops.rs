//! Network operations: fetch/pull/push, auto-refresh, fs-watcher polling.

use super::*;
use crate::toast::ToastKind;

impl App {
    pub fn update_fetch_status(&mut self) -> bool {
        let Some((result, silent)) = self.network.poll_fetch() else {
            return false;
        };
        let flight = self.in_flight_op.take();
        // Re-arm the auto-fetch/refresh timers on every completion, success or
        // failure. Resetting only on Ok left the timer stale after a failure, so
        // check_auto_timers saw the interval as already elapsed and re-fired the
        // auto-fetch on the very next tick — storming toasts while offline.
        self.network.reset_timers();
        match result {
            Ok(()) => {
                self.refresh_latches.auto_fetch = false;
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
                        Err(e) => self.report_refresh_error(e),
                    }
                }
            }
            // A silent auto-fetch error was previously toasted unconditionally,
            // storming the status bar on a persistent failure (e.g. offline).
            // Latch it: report once per failure episode, re-arm on success. A
            // user-initiated fetch keeps the full error dialog. An HTTPS auth
            // failure on a user-initiated fetch opens the credential prompt.
            Err(e) => {
                if self.try_prompt_credentials(&e, flight) {
                    // Prompt opened.
                } else if silent {
                    if !self.refresh_latches.auto_fetch {
                        self.refresh_latches.auto_fetch = true;
                        self.set_message(format!("Auto-fetch failed: {e}"));
                    }
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
                    self.report_refresh_error(e);
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
                    self.report_refresh_error(e);
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
                        self.set_message(Self::conflict_guidance(count));
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
            // Latch the failure so a persistently-failing auto-refresh reports
            // once per episode instead of every interval; re-arm on success.
            match self.refresh(false) {
                Ok(()) => self.refresh_latches.auto_refresh = false,
                Err(e) => {
                    if !self.refresh_latches.auto_refresh {
                        self.refresh_latches.auto_refresh = true;
                        self.set_message(format!("Auto-refresh failed: {e}"));
                    }
                }
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
        let prs = match self.pr_fetch.poll() {
            Some(Ok(prs)) => {
                self.refresh_latches.pr_fetch = false;
                prs
            }
            // gh-missing / no-remote / timeout: report once per episode and keep
            // the last-good PR map rather than wiping the badges (issue #65).
            Some(Err(e)) => {
                if !self.refresh_latches.pr_fetch {
                    self.refresh_latches.pr_fetch = true;
                    self.set_message(format!("PR fetch failed: {e}"));
                }
                return false;
            }
            None => return false,
        };
        if self.pr_toasts_armed {
            if let Some(summary) = crate::pr::pr_refresh_summary(&self.open_prs, &prs) {
                self.toast(ToastKind::Info, summary);
            }
        }
        // Arm after the first successful fill so the startup population is quiet.
        self.pr_toasts_armed = true;
        self.open_prs = prs;
        // The base-update-merge set (issue #55) is derived from the open PRs, so
        // recompute it now that they changed (guarded by its own signature).
        self.recompute_base_update_merges();
        true
    }

    /// Kick off / poll the background merged-PR fetch (`gh pr list --state
    /// merged`). On a change, hand the new signal to the background classifier;
    /// its result is applied by `update_merged_classification`. This is the
    /// primary squash-merge signal (issue #60). Never blocks the UI.
    pub fn update_merged_prs(&mut self) -> bool {
        self.merged.pr_branch_fetch.maybe_start(&self.repo_path);
        let set = match self.merged.pr_branch_fetch.poll() {
            Some(Ok(set)) => {
                self.refresh_latches.merged_fetch = false;
                set
            }
            // gh-missing / no-remote / timeout: report once per episode and keep
            // the last-good merged-branch signal (issue #65). The local patch-id
            // classifier still runs, so squash-merge detection degrades, not dies.
            Some(Err(e)) => {
                if !self.refresh_latches.merged_fetch {
                    self.refresh_latches.merged_fetch = true;
                    self.set_message(format!("Merged-PR fetch failed: {e}"));
                }
                return false;
            }
            None => return false,
        };
        if set == self.merged.pr_branches {
            return false;
        }
        self.merged.pr_branches = set;
        // Re-run the (off-thread) classification with the new GitHub signal.
        self.kick_merged_classification();
        false
    }

    /// Poll the background merged-branch classifier. When it delivers a set that
    /// differs from the current one, apply it and rebuild the graph so dimming —
    /// and hiding, when the toggle is on — reflect the new classification. Never
    /// blocks the UI thread (the git diffing happened on the worker).
    pub fn update_merged_classification(&mut self) -> bool {
        let Some(set) = self.merged.classify.poll() else {
            return false;
        };
        if set == self.merged.branches {
            return false;
        }
        self.merged.branches = set;
        // Only the merged filter/dimming changed — the refs on disk are
        // unchanged — so run just the graph rebuild + selection restore +
        // cache reconcile, skipping the expensive `reload_refs` (repo reopen +
        // `git status` + branch enumeration) a full refresh would redo.
        // Best-effort: a rebuild failure just leaves the prior graph.
        let _ = self.rebuild_and_restore(false);
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
            crate::watcher::PollResult::Refresh { git_changed } => {
                // A `.git` ref/HEAD change under a long-lived libgit2 handle is
                // only observed after a reopen, so mark the repo dirty; a
                // working-tree-only tick refreshes the graph/status but leaves
                // the handle (and this flag) alone, skipping the reopen cost.
                if git_changed {
                    self.repo_dirty = true;
                }
                // Latch: a burst of failing watcher-driven refreshes (e.g. during
                // a build) reports once per episode, not on every poll; re-arm on
                // success.
                match self.refresh(false) {
                    Ok(()) => self.refresh_latches.watch_refresh = false,
                    Err(e) => {
                        if !self.refresh_latches.watch_refresh {
                            self.refresh_latches.watch_refresh = true;
                            self.set_message(format!("Watch refresh failed: {e}"));
                        }
                    }
                }
                self.network.mark_refreshed();
                true
            }
            crate::watcher::PollResult::Disconnected => {
                self.toast(
                    crate::toast::ToastKind::Error,
                    "Filesystem watcher disconnected",
                );
                self.watcher = None;
                self.watcher_disconnected = true;
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
        self.merged.pr_branch_fetch.force();

        if let Err(e) = self.refresh(true) {
            self.report_refresh_error(e);
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
            self.toast(crate::toast::ToastKind::Info, BUSY_PULL_IN_PROGRESS);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GitRepository;

    /// A minimal App over a freshly-initialized (unborn HEAD) repo — enough to
    /// exercise `update_fetch_status` without any commits or remotes.
    fn test_app() -> (tempfile::TempDir, App) {
        let tempdir = tempfile::tempdir().unwrap();
        git2::Repository::init(tempdir.path()).unwrap();
        let repo = GitRepository::open(tempdir.path()).unwrap();
        let app = App::from_repo(repo).unwrap();
        (tempdir, app)
    }

    /// A persistently-failing silent auto-fetch (e.g. offline) must report
    /// exactly once per failure episode — not storm the status bar on every
    /// retry — and a success must clear the latch so the next episode reports
    /// again.
    #[test]
    fn silent_auto_fetch_error_reports_once_until_success_clears_latch() {
        let (_tempdir, mut app) = test_app();

        // First failure of the episode: latched and reported.
        app.network.complete_fetch_for_test(Err("offline".to_string()), true);
        assert!(app.update_fetch_status());
        assert!(app.refresh_latches.auto_fetch);
        assert_eq!(app.message.as_deref(), Some("Auto-fetch failed: offline"));

        // Repeated failures within the same episode must not re-report.
        for _ in 0..5 {
            app.message = None;
            app.network.complete_fetch_for_test(Err("offline".to_string()), true);
            assert!(app.update_fetch_status());
            assert!(app.refresh_latches.auto_fetch);
            assert_eq!(app.message, None, "latched failure must not re-report");
        }

        // A success clears the latch.
        app.network.complete_fetch_for_test(Ok(()), true);
        assert!(app.update_fetch_status());
        assert!(!app.refresh_latches.auto_fetch);

        // The next failure starts a new episode and reports again.
        app.message = None;
        app.network.complete_fetch_for_test(Err("offline".to_string()), true);
        assert!(app.update_fetch_status());
        assert!(app.refresh_latches.auto_fetch);
        assert_eq!(app.message.as_deref(), Some("Auto-fetch failed: offline"));
    }
}
