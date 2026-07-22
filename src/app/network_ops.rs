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
                // Opt-in fast-forward of local branches strictly behind their
                // upstream. Only on a user-initiated refresh (F5 / manual fetch),
                // never on silent auto-fetch. Runs after the fetch made remote-
                // tracking refs current and before the refresh below, so the one
                // refresh renders the moved branches at their new tips.
                if !silent && self.config.refresh.fast_forward_on_refresh {
                    self.fast_forward_on_refresh();
                }
                if matches!(self.mode, AppMode::FileDiff { .. }) {
                    self.pending_refresh = true;
                } else {
                    let prev_head = self.repo.head_oid();
                    let prev_tips = Self::branch_tips(&self.branches);
                    match self.refresh(true) {
                        Ok(()) => {
                            // Tip-level comparison, not branch *count*: a fetch
                            // that only advances an existing tip (the common
                            // fast-forward) must still count as a change.
                            let changed = self.repo.head_oid() != prev_head
                                || Self::branch_tips(&self.branches) != prev_tips;
                            if changed {
                                // Moved refs mean the gh-derived state is stale
                                // too: PR head OIDs for badges (#107) and the
                                // merged-PR set that drives hide-merged (#104).
                                // Re-poll now instead of waiting out the 5-min
                                // interval; an unchanged fetch stays on the slow
                                // timer so quiet repos don't hammer gh.
                                self.force_gh_refresh();
                            }
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
                // Even on failure, a fetch-all may have *partially* succeeded:
                // healthy remotes updated their tracking refs on disk while a
                // broken remote produced the error (issue #91). Refresh first so
                // those updates surface in the graph — refresh does not touch any
                // of the fetch-error surfaces below (on success it clears only its
                // own wt_status latch), so the credential prompt / silent latch /
                // error modal still win. Mirror the Ok path's FileDiff deferral so
                // the diff viewer isn't disrupted mid-view.
                if matches!(self.mode, AppMode::FileDiff { .. }) {
                    self.pending_refresh = true;
                } else if let Err(re) = self.refresh(true) {
                    tracing::warn!(error = %re, "refresh after failed fetch");
                }
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
        // Push start is reported via toast (see `dispatch_net_op`), not a
        // sticky status message, so there is nothing to clear here.
        let flight = self.in_flight_op.take();
        // A remote-branch deletion rides the push pipeline; identify it so
        // completion produces the right toast (and restores on failure).
        let delete_target = flight.as_ref().and_then(|f| match &f.op {
            RetryableOp::Push(PushSpec::Delete { remote, branch }) => {
                Some((remote.clone(), branch.clone()))
            }
            _ => None,
        });
        match result {
            Ok(()) => {
                if let Some((remote, branch)) = &delete_target {
                    // The ref is gone upstream and locally (push --delete prunes
                    // the tracking ref); drop the optimistic hide and reconcile.
                    self.pending_remote_deletions.remove(&format!("{remote}/{branch}"));
                    self.toast(ToastKind::Success, format!("Deleted {remote}/{branch}"));
                } else {
                    self.toast(ToastKind::Success, "Pushed");
                }
                if let Err(e) = self.refresh(true) {
                    self.report_refresh_error(e);
                }
                // A successful push changed the remote by definition: the PR's
                // head OID (badges, #107) and — after a merge push — the gh
                // merged set (#104) are stale until re-polled.
                self.force_gh_refresh();
            }
            Err(e) => {
                // An HTTPS auth failure opens the credential prompt and retries
                // the same op; keep the optimistic hide in place for the retry.
                if self.try_prompt_credentials(&e, flight) {
                    // Prompt opened — nothing else to do.
                } else if let Some((remote, branch)) = &delete_target {
                    // Terminal deletion failure: surface it and undo the
                    // optimistic hide via a refresh so the branch reappears.
                    self.pending_remote_deletions.remove(&format!("{remote}/{branch}"));
                    self.toast(
                        ToastKind::Error,
                        format!("Delete {remote}/{branch} failed: {e}"),
                    );
                    if let Err(re) = self.refresh(true) {
                        self.report_refresh_error(re);
                    }
                } else {
                    self.show_git_error(e);
                }
            }
        }
        true
    }

    /// Skip the gh fetchers' 5-minute intervals so the next poll re-queries
    /// open PRs and the merged-PR set. Called when local evidence says the
    /// remote changed (fetch moved refs, push completed) — the event-driven
    /// complement of the slow timers (#104, #107). F5 routes through the same
    /// forces in `full_update`.
    fn force_gh_refresh(&mut self) {
        self.pr_fetch.force();
        self.merged.pr_branch_fetch.force();
    }

    /// Branch tips as (name, oid) pairs — the identity a fetch can move.
    fn branch_tips(branches: &[crate::git::BranchInfo]) -> Vec<(String, git2::Oid)> {
        branches.iter().map(|b| (b.name.clone(), b.tip_oid)).collect()
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
            Err(e) => self.handle_pull_error(e, flight),
        }
        true
    }

    /// Route a pull failure to the right surface. Divergence offers the
    /// merge/rebase picker; a dirty worktree (#96) is an expected, actionable
    /// condition — an error toast, never the input-swallowing error modal;
    /// auth failures prompt for credentials; anything else is a genuine error
    /// and gets the modal.
    fn handle_pull_error(&mut self, e: String, flight: Option<InFlightOp>) {
        if is_divergent_pull_error(&e) && self.last_pull.is_some() {
            self.mode = AppMode::PullDivergence { selected: 0 };
        } else if is_dirty_worktree_pull_error(&e) {
            let msg = humanize_git_error(&e).unwrap_or_else(|| {
                "Pull blocked by local changes — commit or stash them first".to_string()
            });
            self.toast(ToastKind::Error, msg);
        } else if !self.try_prompt_credentials(&e, flight) {
            self.show_git_error(e);
        }
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
        let Some((set, targets)) = self.merged.classify.poll() else {
            return false;
        };
        let unchanged = set == self.merged.branches && targets == self.merged.squash_targets;
        self.merged.branches = set;
        self.merged.squash_targets = targets;
        // Persist the freshly-computed result to the cross-session cache (#104)
        // so the next startup can serve it instantly. Written on every delivery
        // — even an unchanged/empty result — so a repo with no merged branches
        // still gets a warm cache. Keyed by the *just-completed* input's
        // signature (read from the classifier, not the live inputs, which may
        // have moved on) so the entry's signature always matches its result.
        self.persist_merged_cache();
        if unchanged {
            // Nothing visible changed — skip the graph rebuild.
            return false;
        }
        // Only the merged filter/dimming changed — the refs on disk are
        // unchanged — so run just the graph rebuild + selection restore +
        // cache reconcile, skipping the expensive `reload_refs` (repo reopen +
        // `git status` + branch enumeration) a full refresh would redo.
        // Best-effort: a rebuild failure just leaves the prior graph.
        let _ = self.rebuild_and_restore(false);
        true
    }

    /// Write the current merged classification to the persistent cache (#104),
    /// tagged with the signature and gh set of the input that produced it (read
    /// from the classifier, which retains them alongside the delivered result).
    /// Best-effort: a missing base branch or any IO error is silently skipped —
    /// the cache is an optimization, never load-bearing.
    fn persist_merged_cache(&self) {
        let (Some(signature), Some(gh_merged)) = (
            self.merged.classify.last_signature(),
            self.merged.classify.last_gh_merged(),
        ) else {
            return;
        };
        crate::merged_cache::MergedCache {
            signature,
            gh_merged: gh_merged.clone(),
            merged: self.merged.branches.clone(),
            squash_targets: self.merged.squash_targets.clone(),
        }
        .save(&self.repo_path);
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

    /// Fast-forward every local branch strictly behind its upstream (opt-in via
    /// `refresh.fast_forward_on_refresh`). Reports a single summary toast when
    /// anything moved and stays silent when nothing did; per-branch failures are
    /// logged and folded into the summary rather than aborting the sweep. The
    /// caller refreshes afterward so moved branches render at their new tips.
    pub(crate) fn fast_forward_on_refresh(&mut self) {
        let summary = crate::git::operations::fast_forward_behind_branches(self.repo.repo());
        if summary.is_empty() {
            return;
        }
        for (branch, err) in &summary.failed {
            tracing::warn!(branch = %branch, error = %err, "fast-forward on refresh failed");
        }
        let moved = summary.moved.len();
        let failed = summary.failed.len();
        let plural = if moved == 1 { "" } else { "es" };
        if failed == 0 {
            self.toast(
                crate::toast::ToastKind::Success,
                format!("Fast-forwarded {moved} branch{plural}"),
            );
        } else {
            self.toast(
                crate::toast::ToastKind::Error,
                format!("Fast-forwarded {moved}, {failed} failed"),
            );
        }
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

    /// #96: a pull blocked by uncommitted local changes is an expected,
    /// actionable condition — it must surface as an error toast and leave the
    /// UI fully usable, never the input-swallowing `AppMode::Error` modal.
    #[test]
    fn dirty_worktree_pull_error_toasts_instead_of_modal() {
        let (_tempdir, mut app) = test_app();
        app.handle_pull_error(
            "error: Your local changes to the following files would be overwritten by merge:\n\ta.txt\nPlease commit your changes or stash them before you merge.\nAborting".to_string(),
            None,
        );
        assert!(
            matches!(app.mode, AppMode::Normal),
            "mode must stay Normal (UI accessible), got {:?}",
            app.mode
        );
        assert!(
            app.toasts.visible().iter().any(|t| t.text.contains("commit or stash")),
            "expected a commit-or-stash error toast"
        );

        // An unrecognized pull failure is an error toast too (#116) — no
        // failure of any kind may lock the UI behind a modal.
        app.handle_pull_error("some unexpected failure".to_string(), None);
        assert!(matches!(app.mode, AppMode::Normal));
        assert!(
            app.toasts
                .visible()
                .iter()
                .any(|t| t.text.contains("some unexpected failure")),
            "unrecognized failure surfaces as an error toast"
        );
    }

    /// App over a repo with one commit and a side branch — enough to move a
    /// branch tip out from under the app and watch a fetch-completion react.
    fn test_app_with_side_branch() -> (tempfile::TempDir, App) {
        let tempdir = tempfile::tempdir().unwrap();
        {
            let repo = git2::Repository::init(tempdir.path()).unwrap();
            let sig = git2::Signature::now("t", "t@example.com").unwrap();
            let tree_id = repo.index().unwrap().write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch("side", &head, false).unwrap();
        }
        let repo = GitRepository::open(tempdir.path()).unwrap();
        let app = App::from_repo(repo).unwrap();
        (tempdir, app)
    }

    /// Advance `side` to a new commit without touching HEAD — the shape of a
    /// fetch that fast-forwards a remote-tracking ref in place.
    fn advance_side_branch(path: &std::path::Path) {
        let repo = git2::Repository::open(path).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let tree = parent.tree().unwrap();
        let oid = repo
            .commit(None, &sig, &sig, "advance", &tree, &[&parent])
            .unwrap();
        let commit = repo.find_commit(oid).unwrap();
        repo.branch("side", &commit, true).unwrap();
    }

    /// #104/#107: a fetch that moves a branch tip *in place* (same branch
    /// count, same HEAD — the everyday fast-forward) must count as a change:
    /// the user-facing toast says the graph updated, and the gh fetchers are
    /// forced off their 5-minute interval so PR badges and the merged-PR set
    /// catch up now, not minutes later.
    #[test]
    fn fetch_that_moves_a_tip_counts_as_changed_and_forces_gh_repoll() {
        let (tempdir, mut app) = test_app_with_side_branch();
        // Arm the interval gates so a force is observable as a due transition.
        app.pr_fetch.mark_fetched_for_test();
        app.merged.pr_branch_fetch.mark_fetched_for_test();
        assert!(!app.pr_fetch.is_due());

        advance_side_branch(tempdir.path());
        app.network.complete_fetch_for_test(Ok(()), false);
        assert!(app.update_fetch_status());

        assert!(
            app.toasts.visible().iter().any(|t| t.text.contains("graph updated")),
            "an in-place tip move must toast as a change, not 'up to date'"
        );
        assert!(app.pr_fetch.is_due(), "open-PR fetch must be forced (#107)");
        assert!(
            app.merged.pr_branch_fetch.is_due(),
            "merged-PR fetch must be forced (#104)"
        );
    }

    /// The complement: a fetch that moved nothing keeps the gh fetchers on
    /// their slow interval — quiet repos must not hammer gh every auto-fetch.
    #[test]
    fn no_op_fetch_leaves_gh_fetchers_on_the_interval() {
        let (_tempdir, mut app) = test_app_with_side_branch();
        app.pr_fetch.mark_fetched_for_test();
        app.merged.pr_branch_fetch.mark_fetched_for_test();

        app.network.complete_fetch_for_test(Ok(()), false);
        assert!(app.update_fetch_status());

        assert!(
            app.toasts.visible().iter().any(|t| t.text.contains("up to date")),
            "unchanged fetch keeps the up-to-date toast"
        );
        assert!(!app.pr_fetch.is_due(), "no ref change → no forced gh poll");
        assert!(!app.merged.pr_branch_fetch.is_due());
    }

    /// #104/#107: a successful push changed the remote by definition, so the
    /// gh fetchers must re-poll immediately (new PR head OID for badges; a
    /// merge push lands in the gh merged set).
    #[test]
    fn successful_push_forces_gh_repoll() {
        let (_tempdir, mut app) = test_app_with_side_branch();
        app.pr_fetch.mark_fetched_for_test();
        app.merged.pr_branch_fetch.mark_fetched_for_test();

        app.network.complete_push_for_test(Ok(()));
        assert!(app.update_push_status());

        assert!(app.pr_fetch.is_due());
        assert!(app.merged.pr_branch_fetch.is_due());
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
