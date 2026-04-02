//! Two-tier diff caching with async loading and debouncing.
//!
//! Manages quick (synchronous, file-names-only) and full (async, line-stats)
//! diff caches for both committed and uncommitted changes.

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use git2::Oid;

use crate::git::{CommitDiffInfo, GitRepository, WorkingTreeStatus};

/// Result of async diff computation for a commit.
pub(crate) struct DiffResult {
    pub oid: Oid,
    pub diff: Result<CommitDiffInfo, String>,
}

/// Identifies the currently selected node for diff loading and caching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffTarget {
    Commit(Oid),
    Uncommitted,
}

pub(crate) type UncommittedDiffResult = (Result<CommitDiffInfo, String>, Option<WorkingTreeStatus>);

/// Delay before starting a diff load after selection changes.
/// Prevents unnecessary computation during fast scrolling.
pub(crate) const DIFF_LOAD_DEBOUNCE: Duration = Duration::from_millis(120);

/// Signals returned by `poll()` to notify App of state changes.
#[derive(Debug, Default)]
pub(crate) struct DiffCacheEvents {
    /// A new uncommitted diff has been loaded; App should sync file list.
    pub uncommitted_diff_loaded: bool,
    /// A status message to display to the user.
    pub message: Option<String>,
}

/// Two-tier diff cache with async loading and debouncing.
pub(crate) struct DiffCache {
    // Quick diff cache (synchronous, file names only, no line stats)
    quick_diff_cache: Option<CommitDiffInfo>,
    quick_diff_target: Option<DiffTarget>,

    // Full diff cache for commits (async load)
    diff_cache: Option<CommitDiffInfo>,
    diff_cache_oid: Option<Oid>,
    diff_loading_oid: Option<Oid>,
    diff_receiver: Option<Receiver<DiffResult>>,

    // Uncommitted diff cache
    uncommitted_diff_cache: Option<CommitDiffInfo>,
    uncommitted_diff_failed: bool,
    uncommitted_diff_loading: bool,
    uncommitted_diff_receiver: Option<Receiver<UncommittedDiffResult>>,
    /// Cache key: working tree status at the time of caching (for invalidation)
    uncommitted_cache_key: Option<WorkingTreeStatus>,

    // Target tracking and debounce
    selected_diff_target: Option<DiffTarget>,
    selected_diff_target_changed_at: Instant,
}

impl DiffCache {
    pub fn new() -> Self {
        Self {
            quick_diff_cache: None,
            quick_diff_target: None,
            diff_cache: None,
            diff_cache_oid: None,
            diff_loading_oid: None,
            diff_receiver: None,
            uncommitted_diff_cache: None,
            uncommitted_diff_failed: false,
            uncommitted_diff_loading: false,
            uncommitted_diff_receiver: None,
            uncommitted_cache_key: None,
            selected_diff_target: None,
            selected_diff_target_changed_at: Instant::now(),
        }
    }

    /// Clear all diff caches (for force refresh).
    pub fn clear_all(&mut self) {
        self.quick_diff_cache = None;
        self.quick_diff_target = None;
        self.diff_cache = None;
        self.diff_cache_oid = None;
        self.diff_loading_oid = None;
        self.diff_receiver = None;
        self.clear_uncommitted();
    }

    /// Clear uncommitted diff cache only.
    pub fn clear_uncommitted(&mut self) {
        self.uncommitted_diff_cache = None;
        self.uncommitted_diff_failed = false;
        self.uncommitted_diff_loading = false;
        self.uncommitted_diff_receiver = None;
        self.uncommitted_cache_key = None;
    }

    /// Clear the uncommitted diff data (but not the loading state).
    /// Used after file operations to ensure the fresh quick diff takes precedence.
    pub fn clear_uncommitted_data(&mut self) {
        self.uncommitted_diff_cache = None;
    }

    /// Invalidate the uncommitted diff cache key to trigger a background reload,
    /// while keeping the cached data visible to avoid UI flicker.
    pub fn invalidate_uncommitted(&mut self) {
        self.uncommitted_diff_failed = false;
        if !self.uncommitted_diff_loading {
            self.uncommitted_diff_receiver = None;
        }
        self.uncommitted_cache_key = None;
    }

    /// Check if uncommitted cache can be reused based on working tree status.
    pub fn can_reuse_uncommitted_cache(
        &self,
        was_uncommitted_selected: bool,
        has_uncommitted_node: bool,
        current_status: Option<&WorkingTreeStatus>,
    ) -> bool {
        let Some(cache_key) = self.uncommitted_cache_key.as_ref() else {
            return false;
        };
        let Some(current_status) = current_status else {
            return false;
        };
        was_uncommitted_selected
            && has_uncommitted_node
            && cache_key.is_precise_cache_key()
            && current_status.is_precise_cache_key()
            && cache_key == current_status
    }

    /// Smart cache invalidation for auto-refresh (non-force).
    /// Keeps caches when the same content is selected.
    pub fn invalidate_for_auto_refresh(
        &mut self,
        selected_oid: Option<Oid>,
        was_uncommitted_selected: bool,
        has_uncommitted_node: bool,
        current_status: Option<&WorkingTreeStatus>,
    ) {
        // Keep commit diff cache if the same commit is still selected
        if self.diff_cache_oid != selected_oid {
            self.diff_cache = None;
            self.diff_cache_oid = None;
            self.diff_loading_oid = None;
            self.diff_receiver = None;
        }

        if !self.can_reuse_uncommitted_cache(
            was_uncommitted_selected,
            has_uncommitted_node,
            current_status,
        ) {
            self.invalidate_uncommitted();
        }
    }

    /// Update the selected diff target and compute quick diff if target changed.
    /// Returns the new target.
    pub fn sync_selected_target(
        &mut self,
        target: Option<DiffTarget>,
        repo: &git2::Repository,
    ) -> Option<DiffTarget> {
        if self.selected_diff_target != target {
            self.selected_diff_target = target;
            self.selected_diff_target_changed_at = Instant::now();

            // Compute quick file list synchronously for instant display
            if let Some(t) = target {
                if self.quick_diff_target != Some(t) {
                    self.quick_diff_target = Some(t);
                    self.quick_diff_cache = match t {
                        DiffTarget::Commit(oid) => {
                            CommitDiffInfo::quick_file_list_for_commit(repo, oid).ok()
                        }
                        DiffTarget::Uncommitted => {
                            CommitDiffInfo::quick_file_list_for_working_tree(repo).ok()
                        }
                    };
                }
            }
        }
        target
    }

    /// Force-set quick diff for uncommitted (used after file operations).
    pub fn set_quick_uncommitted(&mut self, repo: &git2::Repository) {
        self.quick_diff_target = Some(DiffTarget::Uncommitted);
        self.quick_diff_cache =
            CommitDiffInfo::quick_file_list_for_working_tree(repo).ok();
    }

    pub fn has_cached_diff_for_target(&self, target: DiffTarget) -> bool {
        match target {
            DiffTarget::Commit(oid) => self.diff_cache_oid == Some(oid),
            DiffTarget::Uncommitted => {
                let has_key = self.uncommitted_cache_key.is_some();
                has_key && (self.uncommitted_diff_cache.is_some() || self.uncommitted_diff_failed)
            }
        }
    }

    fn is_diff_loading_for_target(&self, target: DiffTarget) -> bool {
        match target {
            DiffTarget::Commit(oid) => self.diff_loading_oid == Some(oid),
            DiffTarget::Uncommitted => self.uncommitted_diff_loading,
        }
    }

    fn is_diff_debouncing_for_target(&self, target: DiffTarget) -> bool {
        self.selected_diff_target == Some(target)
            && self.selected_diff_target_changed_at.elapsed() < DIFF_LOAD_DEBOUNCE
    }

    fn has_in_flight_diff(&self) -> bool {
        self.diff_loading_oid.is_some() || self.uncommitted_diff_loading
    }

    /// Get cached diff info for a specific target.
    pub fn cached_diff(&self, target: Option<DiffTarget>) -> Option<&CommitDiffInfo> {
        match target? {
            DiffTarget::Commit(oid) if self.diff_cache_oid == Some(oid) => self.diff_cache.as_ref(),
            DiffTarget::Commit(_) => None,
            DiffTarget::Uncommitted => self.uncommitted_diff_cache.as_ref(),
        }
    }

    /// Get the best available diff: full if cached, otherwise quick file list.
    pub fn cached_diff_or_quick(&self, target: Option<DiffTarget>) -> Option<&CommitDiffInfo> {
        self.cached_diff(target).or(self.quick_diff_cache.as_ref())
    }

    /// Whether line stats are still loading (full diff not yet available but quick is).
    pub fn is_line_stats_loading(&self, target: Option<DiffTarget>) -> bool {
        self.cached_diff(target).is_none() && self.quick_diff_cache.is_some()
    }

    /// Whether diff is loading or pending (debouncing) for the selected node.
    pub fn is_diff_loading(&self, target: Option<DiffTarget>) -> bool {
        let Some(target) = target else {
            return false;
        };
        !self.has_cached_diff_for_target(target)
            && (self.is_diff_loading_for_target(target)
                || self.is_diff_debouncing_for_target(target)
                || self.has_in_flight_diff())
    }

    /// Poll async receivers and spawn new loads as needed.
    /// Returns events that App should act on.
    pub fn poll(
        &mut self,
        target: Option<DiffTarget>,
        repo_path: &str,
        working_tree_status: Option<&WorkingTreeStatus>,
    ) -> DiffCacheEvents {
        let mut events = DiffCacheEvents::default();

        // Pull in completed results for commit diff
        if let Some(ref receiver) = self.diff_receiver {
            match receiver.try_recv() {
                Ok(result) => {
                    match result.diff {
                        Ok(diff) => {
                            self.diff_cache = Some(diff);
                            self.diff_cache_oid = Some(result.oid);
                        }
                        Err(e) => {
                            self.diff_cache = None;
                            self.diff_cache_oid = Some(result.oid);
                            events.message = Some(format!("Failed to load diff: {e}"));
                        }
                    }
                    self.diff_loading_oid = None;
                    self.diff_receiver = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.diff_loading_oid = None;
                    self.diff_receiver = None;
                    events.message = Some("Diff computation failed unexpectedly".to_string());
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        // Pull in completed results for uncommitted diff
        if let Some(ref receiver) = self.uncommitted_diff_receiver {
            match receiver.try_recv() {
                Ok((result, status)) => {
                    match result {
                        Ok(diff) => {
                            self.uncommitted_diff_cache = Some(diff);
                            self.uncommitted_diff_failed = false;
                            events.uncommitted_diff_loaded = true;
                        }
                        Err(e) => {
                            self.uncommitted_diff_cache = None;
                            self.uncommitted_diff_failed = true;
                            events.message = Some(format!("Failed to load diff: {e}"));
                        }
                    }
                    let effective_status = status.or_else(|| working_tree_status.cloned());
                    if effective_status.as_ref() == working_tree_status {
                        self.uncommitted_cache_key = effective_status;
                    }
                    self.uncommitted_diff_loading = false;
                    self.uncommitted_diff_receiver = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.uncommitted_diff_loading = false;
                    self.uncommitted_diff_receiver = None;
                    self.uncommitted_diff_failed = true;
                    self.uncommitted_cache_key = working_tree_status.cloned();
                    events.message = Some("Diff computation failed unexpectedly".to_string());
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        let Some(target) = target else {
            return events;
        };

        if self.has_cached_diff_for_target(target)
            || self.is_diff_loading_for_target(target)
            || self.is_diff_debouncing_for_target(target)
        {
            return events;
        }

        // Keep only one heavy diff computation in flight
        if self.has_in_flight_diff() {
            return events;
        }

        match target {
            DiffTarget::Uncommitted => {
                let (tx, rx) = mpsc::channel();
                let repo_path = repo_path.to_string();

                self.uncommitted_diff_failed = false;
                self.uncommitted_diff_loading = true;
                self.uncommitted_diff_receiver = Some(rx);

                thread::spawn(move || {
                    let repo = match GitRepository::open(&repo_path) {
                        Ok(r) => r,
                        Err(e) => {
                            let _ = tx.send((Err(e.to_string()), None));
                            return;
                        }
                    };
                    let status = repo.get_working_tree_status().unwrap_or_default();
                    let diff =
                        CommitDiffInfo::from_working_tree(repo.repo()).map_err(|e| e.to_string());
                    let _ = tx.send((diff, status));
                });
            }
            DiffTarget::Commit(oid) => {
                let (tx, rx) = mpsc::channel();
                let repo_path = repo_path.to_string();

                self.diff_loading_oid = Some(oid);
                self.diff_receiver = Some(rx);

                thread::spawn(move || {
                    let diff = git2::Repository::open(&repo_path)
                        .map_err(|e| e.to_string())
                        .and_then(|repo| {
                            CommitDiffInfo::from_commit(&repo, oid).map_err(|e| e.to_string())
                        });

                    let _ = tx.send(DiffResult { oid, diff });
                });
            }
        }

        events
    }
}

#[cfg(test)]
impl DiffCache {
    pub fn set_uncommitted_cache(&mut self, diff: Option<CommitDiffInfo>) {
        self.uncommitted_diff_cache = diff;
    }
    pub fn set_uncommitted_failed(&mut self, failed: bool) {
        self.uncommitted_diff_failed = failed;
    }
    pub fn set_uncommitted_loading(&mut self, loading: bool) {
        self.uncommitted_diff_loading = loading;
    }
    pub fn set_diff_loading_oid(&mut self, oid: Option<Oid>) {
        self.diff_loading_oid = oid;
    }
    pub fn set_diff_cache(&mut self, oid: Option<Oid>, diff: Option<CommitDiffInfo>) {
        self.diff_cache_oid = oid;
        self.diff_cache = diff;
    }
    pub fn set_uncommitted_cache_key(&mut self, key: Option<WorkingTreeStatus>) {
        self.uncommitted_cache_key = key;
    }
    pub fn set_selected_target(&mut self, target: Option<DiffTarget>) {
        self.selected_diff_target = target;
    }
    pub fn set_selected_target_changed_at(&mut self, instant: Instant) {
        self.selected_diff_target_changed_at = instant;
    }
    pub fn set_quick_diff(&mut self, target: Option<DiffTarget>, cache: Option<CommitDiffInfo>) {
        self.quick_diff_target = target;
        self.quick_diff_cache = cache;
    }
    pub fn diff_receiver(&self) -> &Option<Receiver<DiffResult>> {
        &self.diff_receiver
    }
    pub fn uncommitted_diff_receiver(&self) -> &Option<Receiver<UncommittedDiffResult>> {
        &self.uncommitted_diff_receiver
    }
    pub fn diff_loading_oid(&self) -> Option<Oid> {
        self.diff_loading_oid
    }
    pub fn uncommitted_diff_loading(&self) -> bool {
        self.uncommitted_diff_loading
    }
    pub fn uncommitted_diff_failed(&self) -> bool {
        self.uncommitted_diff_failed
    }
    pub fn uncommitted_cache_key(&self) -> Option<&WorkingTreeStatus> {
        self.uncommitted_cache_key.as_ref()
    }
    pub fn set_diff_receiver(&mut self, rx: Option<Receiver<DiffResult>>) {
        self.diff_receiver = rx;
    }
    pub fn set_uncommitted_diff_receiver(&mut self, rx: Option<Receiver<UncommittedDiffResult>>) {
        self.uncommitted_diff_receiver = rx;
    }
    pub fn diff_cache_oid(&self) -> Option<Oid> {
        self.diff_cache_oid
    }
    pub fn get_diff_cache(&self) -> Option<&CommitDiffInfo> {
        self.diff_cache.as_ref()
    }
}
