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
pub struct DiffResult {
    pub oid: Oid,
    pub diff: Result<CommitDiffInfo, String>,
}

/// Identifies the currently selected node for diff loading and caching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffTarget {
    Commit(Oid),
    Uncommitted,
}

pub type UncommittedDiffResult = (Result<CommitDiffInfo, String>, Option<WorkingTreeStatus>);

/// Delay before starting a diff load after selection changes.
/// Prevents unnecessary computation during fast scrolling.
pub const DIFF_LOAD_DEBOUNCE: Duration = Duration::from_millis(120);

/// Signals returned by `poll()` to notify App of state changes.
#[derive(Debug, Default)]
pub struct DiffCacheEvents {
    /// A diff result was received (commit or uncommitted).
    pub diff_loaded: bool,
    /// A new uncommitted diff has been loaded; App should sync file list.
    pub uncommitted_diff_loaded: bool,
    /// A status message to display to the user.
    pub message: Option<String>,
}

/// Two-tier diff cache with async loading and debouncing.
pub struct DiffCache {
    pub quick_diff_cache: Option<CommitDiffInfo>,
    pub quick_diff_target: Option<DiffTarget>,
    pub diff_cache: Option<CommitDiffInfo>,
    pub diff_cache_oid: Option<Oid>,
    pub diff_loading_oid: Option<Oid>,
    pub diff_receiver: Option<Receiver<DiffResult>>,
    pub uncommitted_diff_cache: Option<CommitDiffInfo>,
    pub uncommitted_diff_failed: bool,
    pub uncommitted_diff_loading: bool,
    pub uncommitted_diff_receiver: Option<Receiver<UncommittedDiffResult>>,
    pub uncommitted_cache_key: Option<WorkingTreeStatus>,
    pub selected_diff_target: Option<DiffTarget>,
    pub selected_diff_target_changed_at: Instant,
}

impl Default for DiffCache {
    fn default() -> Self {
        Self::new()
    }
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
    /// Returns the new target and whether it changed (callers should re-render
    /// when it did, since the quick diff was recomputed).
    pub fn sync_selected_target(
        &mut self,
        target: Option<DiffTarget>,
        repo: &git2::Repository,
    ) -> (Option<DiffTarget>, bool) {
        let changed = self.selected_diff_target != target;
        if changed {
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
        (target, changed)
    }

    /// Force-set quick diff for uncommitted (used after file operations).
    pub fn set_quick_uncommitted(&mut self, repo: &git2::Repository) {
        self.quick_diff_target = Some(DiffTarget::Uncommitted);
        self.quick_diff_cache =
            CommitDiffInfo::quick_file_list_for_working_tree(repo).ok();
    }

    /// Reclassify the cached full uncommitted diff's staging status using the
    /// quick diff as the source of truth. This avoids a redundant async reload
    /// after stage/unstage operations where line counts don't change.
    /// Seals the cache key so `poll()` won't trigger a reload.
    pub fn reclassify_uncommitted_staging(&mut self, current_status: Option<&crate::git::WorkingTreeStatus>) {
        let Some(full) = self.uncommitted_diff_cache.as_mut() else {
            // Full cache not yet loaded — seal the key so poll() won't
            // trigger a reload (the quick diff is already correct).
            self.uncommitted_cache_key = current_status.cloned();
            return;
        };
        let Some(quick) = self.quick_diff_cache.as_ref() else {
            return;
        };

        // Build a lookup: path → stage_status from the quick diff
        let stage_map: std::collections::HashMap<&std::path::Path, Option<crate::git::StageStatus>> =
            quick.files.iter().map(|f| (f.path.as_path(), f.stage_status)).collect();

        // Update stage_status on each file in the full diff
        for file in &mut full.files {
            if let Some(&status) = stage_map.get(file.path.as_path()) {
                file.stage_status = status;
            }
        }

        // Rebuild staged/unstaged separation
        full.staged_files = full.files.iter()
            .filter(|f| matches!(f.stage_status, Some(crate::git::StageStatus::Staged)))
            .cloned()
            .collect();
        full.unstaged_files = full.files.iter()
            .filter(|f| !matches!(f.stage_status, Some(crate::git::StageStatus::Staged)))
            .cloned()
            .collect();

        // Seal cache key to match current working tree status so poll() won't reload
        self.uncommitted_cache_key = current_status.cloned();
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

    /// Get the quick diff, but only if it was computed for the given target —
    /// a quick diff for a different target is stale and must not be shown.
    fn quick_diff_for_target(&self, target: Option<DiffTarget>) -> Option<&CommitDiffInfo> {
        if target.is_some() && self.quick_diff_target == target {
            self.quick_diff_cache.as_ref()
        } else {
            None
        }
    }

    /// Get the best available diff: full if cached, otherwise quick file list.
    pub fn cached_diff_or_quick(&self, target: Option<DiffTarget>) -> Option<&CommitDiffInfo> {
        self.cached_diff(target)
            .or_else(|| self.quick_diff_for_target(target))
    }

    /// Whether line stats are still loading (full diff not yet available but quick is).
    pub fn is_line_stats_loading(&self, target: Option<DiffTarget>) -> bool {
        self.cached_diff(target).is_none() && self.quick_diff_for_target(target).is_some()
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
                    events.diff_loaded = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.diff_loading_oid = None;
                    self.diff_receiver = None;
                    events.diff_loaded = true;
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
                    events.diff_loaded = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.uncommitted_diff_loading = false;
                    self.uncommitted_diff_receiver = None;
                    self.uncommitted_diff_failed = true;
                    self.uncommitted_cache_key = working_tree_status.cloned();
                    events.diff_loaded = true;
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
mod tests {
    use super::*;
    use crate::git::{FileChangeKind, StageStatus};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::Instant;

    fn diff_file(path: &str, stage: Option<StageStatus>) -> crate::git::FileDiffInfo {
        crate::git::FileDiffInfo {
            path: PathBuf::from(path),
            kind: FileChangeKind::Modified,
            is_binary: false,
            insertions: 1,
            deletions: 0,
            stage_status: stage,
        }
    }

    fn set_diff(cache: &mut DiffCache, oid: Oid, diff: CommitDiffInfo) {
        cache.diff_cache_oid = Some(oid);
        cache.diff_cache = Some(diff);
    }

    fn set_quick(cache: &mut DiffCache, target: DiffTarget, diff: CommitDiffInfo) {
        cache.quick_diff_target = Some(target);
        cache.quick_diff_cache = Some(diff);
    }

    fn oid1() -> Oid {
        Oid::from_str("0000000000000000000000000000000000000001").unwrap()
    }

    fn oid2() -> Oid {
        Oid::from_str("0000000000000000000000000000000000000002").unwrap()
    }

    fn empty_diff() -> CommitDiffInfo {
        CommitDiffInfo {
            files: vec![],
            total_insertions: 0,
            total_deletions: 0,
            total_files: 0,
            truncated: false,
            staged_files: vec![],
            unstaged_files: vec![],
        }
    }

    fn precise_status() -> WorkingTreeStatus {
        WorkingTreeStatus {
            file_paths: vec![PathBuf::from("a.txt")],
            mtime_hash: 100,
            has_collapsed_untracked_dirs: false,
        }
    }

    fn precise_status_different() -> WorkingTreeStatus {
        WorkingTreeStatus {
            file_paths: vec![PathBuf::from("b.txt")],
            mtime_hash: 200,
            has_collapsed_untracked_dirs: false,
        }
    }

    fn imprecise_status() -> WorkingTreeStatus {
        WorkingTreeStatus {
            file_paths: vec![PathBuf::from("a.txt")],
            mtime_hash: 100,
            has_collapsed_untracked_dirs: true,
        }
    }

    // ── Cache clearing ──────────────────────────────────────────────

    #[test]
    fn clear_all_clears_everything() {
        let mut cache = DiffCache::new();
        set_diff(&mut cache, oid1(), empty_diff());
        set_quick(&mut cache, DiffTarget::Commit(oid1()), empty_diff());
        cache.uncommitted_diff_cache = Some(empty_diff());
        cache.uncommitted_cache_key = Some(precise_status());
        cache.uncommitted_diff_loading = true;
        cache.diff_loading_oid = Some(oid1());

        cache.clear_all();

        assert!(cache.diff_cache.as_ref().is_none());
        assert!(cache.diff_cache_oid.is_none());
        assert!(cache.diff_loading_oid.is_none());
        assert!(cache.cached_diff_or_quick(Some(DiffTarget::Commit(oid1()))).is_none());
        assert!(cache.cached_diff(Some(DiffTarget::Uncommitted)).is_none());
        assert!(!cache.uncommitted_diff_loading);
        assert!(!cache.uncommitted_diff_failed);
        assert!(cache.uncommitted_cache_key.as_ref().is_none());
    }

    #[test]
    fn clear_uncommitted_only_clears_uncommitted_state() {
        let mut cache = DiffCache::new();
        set_diff(&mut cache, oid1(), empty_diff());
        cache.uncommitted_diff_cache = Some(empty_diff());
        cache.uncommitted_cache_key = Some(precise_status());
        cache.uncommitted_diff_loading = true;
        cache.uncommitted_diff_failed = true;

        cache.clear_uncommitted();

        // Commit cache preserved
        assert!(cache.diff_cache.as_ref().is_some());
        assert_eq!(cache.diff_cache_oid, Some(oid1()));
        // Uncommitted state cleared
        assert!(cache.cached_diff(Some(DiffTarget::Uncommitted)).is_none());
        assert!(!cache.uncommitted_diff_loading);
        assert!(!cache.uncommitted_diff_failed);
        assert!(cache.uncommitted_cache_key.as_ref().is_none());
    }

    #[test]
    fn clear_uncommitted_data_only_clears_diff_not_loading_state() {
        let mut cache = DiffCache::new();
        cache.uncommitted_diff_cache = Some(empty_diff());
        cache.uncommitted_diff_loading = true;
        cache.uncommitted_cache_key = Some(precise_status());

        cache.clear_uncommitted_data();

        assert!(cache.cached_diff(Some(DiffTarget::Uncommitted)).is_none());
        // Loading state and cache key preserved
        assert!(cache.uncommitted_diff_loading);
        assert!(cache.uncommitted_cache_key.as_ref().is_some());
    }

    #[test]
    fn invalidate_uncommitted_clears_key_but_keeps_data() {
        let mut cache = DiffCache::new();
        cache.uncommitted_diff_cache = Some(empty_diff());
        cache.uncommitted_cache_key = Some(precise_status());
        cache.uncommitted_diff_failed = true;

        cache.invalidate_uncommitted();

        // Data still present for display
        assert!(cache.cached_diff(Some(DiffTarget::Uncommitted)).is_some());
        // Cache key cleared to trigger reload
        assert!(cache.uncommitted_cache_key.as_ref().is_none());
        // Failed flag cleared
        assert!(!cache.uncommitted_diff_failed);
    }

    // Rewritten from asserting `uncommitted_diff_receiver.is_some()/is_none()`
    // (an implementation detail) to the observable contract: whether a
    // result that arrives on the channel after invalidation is still picked
    // up by `poll()`.
    #[test]
    fn invalidate_while_loading_still_applies_in_flight_result() {
        let mut cache = DiffCache::new();
        let (tx, rx) = mpsc::channel::<UncommittedDiffResult>();
        cache.uncommitted_diff_receiver = Some(rx);
        cache.uncommitted_diff_loading = true;

        // Invalidate while a load is genuinely in flight: the receiver must
        // be kept alive so the eventual result isn't lost.
        cache.invalidate_uncommitted();

        // The in-flight load completes after invalidation.
        let status = precise_status();
        tx.send((Ok(empty_diff()), Some(status.clone()))).unwrap();

        let events = cache.poll(None, "/dev/null", Some(&status));

        assert!(cache.cached_diff(Some(DiffTarget::Uncommitted)).is_some());
        assert!(events.uncommitted_diff_loaded);
    }

    #[test]
    fn invalidate_while_not_loading_discards_late_result() {
        let mut cache = DiffCache::new();
        let (tx, rx) = mpsc::channel::<UncommittedDiffResult>();
        cache.uncommitted_diff_receiver = Some(rx);
        cache.uncommitted_diff_loading = false;

        // A result from a stale/abandoned load is already sitting in the
        // channel...
        let status = precise_status();
        tx.send((Ok(empty_diff()), Some(status.clone()))).unwrap();

        // ...but invalidating while not "loading" treats the channel as
        // stale and drops it, so the late result can never be observed.
        cache.invalidate_uncommitted();

        let events = cache.poll(None, "/dev/null", Some(&status));

        assert!(cache.cached_diff(Some(DiffTarget::Uncommitted)).is_none());
        assert!(!events.uncommitted_diff_loaded);
    }

    // ── Reclassify uncommitted staging ──────────────────────────────

    #[test]
    fn reclassify_updates_stage_status_from_quick_diff() {
        let mut cache = DiffCache::new();

        // Full (async) cache: a.txt staged, b.txt unstaged — now stale.
        let full = CommitDiffInfo {
            files: vec![
                diff_file("a.txt", Some(StageStatus::Staged)),
                diff_file("b.txt", Some(StageStatus::Unstaged)),
            ],
            total_insertions: 2,
            total_deletions: 0,
            total_files: 2,
            truncated: false,
            staged_files: vec![diff_file("a.txt", Some(StageStatus::Staged))],
            unstaged_files: vec![diff_file("b.txt", Some(StageStatus::Unstaged))],
        };
        cache.uncommitted_diff_cache = Some(full);

        // Quick (sync) diff reflects the real, just-changed staging state:
        // the user staged b.txt and unstaged a.txt.
        let quick = CommitDiffInfo {
            files: vec![
                diff_file("a.txt", Some(StageStatus::Unstaged)),
                diff_file("b.txt", Some(StageStatus::Staged)),
            ],
            ..Default::default()
        };
        cache.quick_diff_cache = Some(quick);

        let status = precise_status();
        cache.reclassify_uncommitted_staging(Some(&status));

        let full = cache.uncommitted_diff_cache.as_ref().unwrap();
        let a = full.files.iter().find(|f| f.path == Path::new("a.txt")).unwrap();
        let b = full.files.iter().find(|f| f.path == Path::new("b.txt")).unwrap();
        assert_eq!(a.stage_status, Some(StageStatus::Unstaged));
        assert_eq!(b.stage_status, Some(StageStatus::Staged));

        // staged_files/unstaged_files rebuilt to match the new statuses.
        assert_eq!(full.staged_files.len(), 1);
        assert_eq!(full.staged_files[0].path, PathBuf::from("b.txt"));
        assert_eq!(full.unstaged_files.len(), 1);
        assert_eq!(full.unstaged_files[0].path, PathBuf::from("a.txt"));

        // Cache key sealed to the current status so poll() treats this
        // target as already cached (no reload needed).
        assert_eq!(cache.uncommitted_cache_key.as_ref(), Some(&status));
    }

    #[test]
    fn reclassify_without_full_cache_still_seals_key() {
        let mut cache = DiffCache::new();
        // No full diff cached yet, but the quick diff already ran, and a
        // background load for the full diff is in flight (the realistic
        // scenario when a file gets staged mid-load).
        cache.quick_diff_cache = Some(empty_diff());
        cache.quick_diff_target = Some(DiffTarget::Uncommitted);
        let (_tx, rx) = mpsc::channel();
        cache.uncommitted_diff_receiver = Some(rx);
        cache.uncommitted_diff_loading = true;

        let status = precise_status();
        cache.reclassify_uncommitted_staging(Some(&status));

        // Nothing to reclassify — full cache still absent.
        assert!(cache.uncommitted_diff_cache.is_none());
        // But the key is sealed to the current status regardless (early
        // return path at the top of reclassify_uncommitted_staging).
        assert_eq!(cache.uncommitted_cache_key.as_ref(), Some(&status));

        // Observable contract: poll() does not spawn a second concurrent
        // load — the in-flight receiver is left untouched.
        let events = cache.poll(Some(DiffTarget::Uncommitted), "/dev/null", Some(&status));
        assert!(cache.uncommitted_diff_loading);
        assert!(cache.uncommitted_diff_receiver.is_some());
        assert!(!events.uncommitted_diff_loaded);
    }

    // ── Cache reuse decisions ───────────────────────────────────────

    #[test]
    fn can_reuse_returns_false_when_no_cache_key() {
        let cache = DiffCache::new();
        let status = precise_status();
        assert!(!cache.can_reuse_uncommitted_cache(true, true, Some(&status)));
    }

    #[test]
    fn can_reuse_returns_false_when_no_current_status() {
        let mut cache = DiffCache::new();
        cache.uncommitted_cache_key = Some(precise_status());
        assert!(!cache.can_reuse_uncommitted_cache(true, true, None));
    }

    #[test]
    fn can_reuse_returns_false_when_not_uncommitted_selected() {
        let mut cache = DiffCache::new();
        let status = precise_status();
        cache.uncommitted_cache_key = Some(status.clone());
        assert!(!cache.can_reuse_uncommitted_cache(false, true, Some(&status)));
    }

    #[test]
    fn can_reuse_returns_false_when_no_uncommitted_node() {
        let mut cache = DiffCache::new();
        let status = precise_status();
        cache.uncommitted_cache_key = Some(status.clone());
        assert!(!cache.can_reuse_uncommitted_cache(true, false, Some(&status)));
    }

    #[test]
    fn can_reuse_returns_true_when_all_conditions_met() {
        let mut cache = DiffCache::new();
        let status = precise_status();
        cache.uncommitted_cache_key = Some(status.clone());
        assert!(cache.can_reuse_uncommitted_cache(true, true, Some(&status)));
    }

    #[test]
    fn can_reuse_returns_false_when_statuses_differ() {
        let mut cache = DiffCache::new();
        cache.uncommitted_cache_key = Some(precise_status());
        let different = precise_status_different();
        assert!(!cache.can_reuse_uncommitted_cache(true, true, Some(&different)));
    }

    #[test]
    fn can_reuse_returns_false_when_cache_key_imprecise() {
        let mut cache = DiffCache::new();
        cache.uncommitted_cache_key = Some(imprecise_status());
        let status = imprecise_status();
        assert!(!cache.can_reuse_uncommitted_cache(true, true, Some(&status)));
    }

    #[test]
    fn can_reuse_returns_false_when_current_status_imprecise() {
        let mut cache = DiffCache::new();
        let precise = precise_status();
        cache.uncommitted_cache_key = Some(precise);
        let imprecise = imprecise_status();
        assert!(!cache.can_reuse_uncommitted_cache(true, true, Some(&imprecise)));
    }

    // ── Auto-refresh invalidation ───────────────────────────────────

    #[test]
    fn auto_refresh_keeps_commit_cache_when_same_oid() {
        let mut cache = DiffCache::new();
        set_diff(&mut cache, oid1(), empty_diff());

        cache.invalidate_for_auto_refresh(Some(oid1()), false, false, None);

        assert!(cache.diff_cache.as_ref().is_some());
        assert_eq!(cache.diff_cache_oid, Some(oid1()));
    }

    #[test]
    fn auto_refresh_clears_commit_cache_when_different_oid() {
        let mut cache = DiffCache::new();
        set_diff(&mut cache, oid1(), empty_diff());

        cache.invalidate_for_auto_refresh(Some(oid2()), false, false, None);

        assert!(cache.diff_cache.as_ref().is_none());
        assert!(cache.diff_cache_oid.is_none());
    }

    #[test]
    fn auto_refresh_clears_commit_cache_when_none_selected() {
        let mut cache = DiffCache::new();
        set_diff(&mut cache, oid1(), empty_diff());

        cache.invalidate_for_auto_refresh(None, false, false, None);

        assert!(cache.diff_cache.as_ref().is_none());
    }

    #[test]
    fn auto_refresh_invalidates_uncommitted_when_cache_not_reusable() {
        let mut cache = DiffCache::new();
        cache.uncommitted_cache_key = Some(precise_status());
        cache.uncommitted_diff_cache = Some(empty_diff());

        // not uncommitted selected => can't reuse
        cache.invalidate_for_auto_refresh(None, false, true, Some(&precise_status()));

        assert!(cache.uncommitted_cache_key.as_ref().is_none());
        // Data kept for display (invalidate_uncommitted behavior)
        assert!(cache.cached_diff(Some(DiffTarget::Uncommitted)).is_some());
    }

    #[test]
    fn auto_refresh_keeps_uncommitted_when_reusable() {
        let mut cache = DiffCache::new();
        let status = precise_status();
        cache.uncommitted_cache_key = Some(status.clone());
        cache.uncommitted_diff_cache = Some(empty_diff());

        cache.invalidate_for_auto_refresh(None, true, true, Some(&status));

        assert!(cache.uncommitted_cache_key.as_ref().is_some());
    }

    // ── Target tracking ─────────────────────────────────────────────

    #[test]
    fn has_cached_diff_for_commit_checks_oid() {
        let mut cache = DiffCache::new();
        set_diff(&mut cache, oid1(), empty_diff());

        assert!(cache.has_cached_diff_for_target(DiffTarget::Commit(oid1())));
        assert!(!cache.has_cached_diff_for_target(DiffTarget::Commit(oid2())));
    }

    #[test]
    fn has_cached_diff_for_uncommitted_checks_key_and_data() {
        let mut cache = DiffCache::new();

        // No key, no data
        assert!(!cache.has_cached_diff_for_target(DiffTarget::Uncommitted));

        // Key but no data or failed
        cache.uncommitted_cache_key = Some(precise_status());
        assert!(!cache.has_cached_diff_for_target(DiffTarget::Uncommitted));

        // Key + data
        cache.uncommitted_diff_cache = Some(empty_diff());
        assert!(cache.has_cached_diff_for_target(DiffTarget::Uncommitted));

        // Key + failed (no data)
        cache.uncommitted_diff_cache = None;
        cache.uncommitted_diff_failed = true;
        assert!(cache.has_cached_diff_for_target(DiffTarget::Uncommitted));
    }

    #[test]
    fn is_diff_loading_returns_false_when_no_target() {
        let cache = DiffCache::new();
        assert!(!cache.is_diff_loading(None));
    }

    #[test]
    fn is_diff_loading_returns_false_when_cached() {
        let mut cache = DiffCache::new();
        set_diff(&mut cache, oid1(), empty_diff());
        assert!(!cache.is_diff_loading(Some(DiffTarget::Commit(oid1()))));
    }

    #[test]
    fn is_diff_loading_returns_true_when_loading() {
        let mut cache = DiffCache::new();
        cache.diff_loading_oid = Some(oid1());
        assert!(cache.is_diff_loading(Some(DiffTarget::Commit(oid1()))));
    }

    #[test]
    fn is_diff_loading_returns_true_when_in_flight_diff_exists() {
        let mut cache = DiffCache::new();
        // Loading oid2 but asking about oid1 — in-flight blocks
        cache.diff_loading_oid = Some(oid2());
        assert!(cache.is_diff_loading(Some(DiffTarget::Commit(oid1()))));
    }

    #[test]
    fn is_diff_loading_returns_true_when_debouncing() {
        let mut cache = DiffCache::new();
        cache.selected_diff_target = Some(DiffTarget::Commit(oid1()));
        cache.selected_diff_target_changed_at = Instant::now();
        assert!(cache.is_diff_loading(Some(DiffTarget::Commit(oid1()))));
    }

    #[test]
    fn is_diff_loading_uncommitted_when_loading() {
        let mut cache = DiffCache::new();
        cache.uncommitted_diff_loading = true;
        assert!(cache.is_diff_loading(Some(DiffTarget::Uncommitted)));
    }

    // ── Diff retrieval ──────────────────────────────────────────────

    #[test]
    fn cached_diff_none_target_returns_none() {
        let cache = DiffCache::new();
        assert!(cache.cached_diff(None).is_none());
    }

    #[test]
    fn cached_diff_commit_returns_diff_when_oid_matches() {
        let mut cache = DiffCache::new();
        set_diff(&mut cache, oid1(), empty_diff());
        assert!(cache.cached_diff(Some(DiffTarget::Commit(oid1()))).is_some());
    }

    #[test]
    fn cached_diff_commit_returns_none_when_oid_mismatch() {
        let mut cache = DiffCache::new();
        set_diff(&mut cache, oid1(), empty_diff());
        assert!(cache.cached_diff(Some(DiffTarget::Commit(oid2()))).is_none());
    }

    #[test]
    fn cached_diff_uncommitted_returns_cache() {
        let mut cache = DiffCache::new();
        cache.uncommitted_diff_cache = Some(empty_diff());
        assert!(cache.cached_diff(Some(DiffTarget::Uncommitted)).is_some());
    }

    #[test]
    fn cached_diff_or_quick_falls_back_to_quick() {
        let mut cache = DiffCache::new();
        set_quick(&mut cache, DiffTarget::Commit(oid1()), empty_diff());
        // No full diff cached, should fall back to quick
        assert!(cache.cached_diff_or_quick(Some(DiffTarget::Commit(oid1()))).is_some());
    }

    #[test]
    fn cached_diff_or_quick_ignores_quick_for_different_target() {
        let mut cache = DiffCache::new();
        // Quick diff was computed for oid1, but oid2 is now selected —
        // returning it would show the previous commit's files.
        set_quick(&mut cache, DiffTarget::Commit(oid1()), empty_diff());
        assert!(cache.cached_diff_or_quick(Some(DiffTarget::Commit(oid2()))).is_none());
        assert!(cache.cached_diff_or_quick(Some(DiffTarget::Uncommitted)).is_none());
    }

    #[test]
    fn cached_diff_or_quick_returns_none_when_no_target() {
        let mut cache = DiffCache::new();
        set_quick(&mut cache, DiffTarget::Commit(oid1()), empty_diff());
        assert!(cache.cached_diff_or_quick(None).is_none());
    }

    #[test]
    fn cached_diff_or_quick_prefers_full_over_quick() {
        let mut cache = DiffCache::new();
        let mut full = empty_diff();
        full.total_files = 42;
        set_diff(&mut cache, oid1(), full);
        set_quick(&mut cache, DiffTarget::Commit(oid1()), empty_diff());

        let result = cache.cached_diff_or_quick(Some(DiffTarget::Commit(oid1()))).unwrap();
        assert_eq!(result.total_files, 42);
    }

    #[test]
    fn is_line_stats_loading_true_when_quick_exists_but_full_doesnt() {
        let mut cache = DiffCache::new();
        set_quick(&mut cache, DiffTarget::Commit(oid1()), empty_diff());
        assert!(cache.is_line_stats_loading(Some(DiffTarget::Commit(oid1()))));
    }

    #[test]
    fn is_line_stats_loading_false_when_full_exists() {
        let mut cache = DiffCache::new();
        set_diff(&mut cache, oid1(), empty_diff());
        set_quick(&mut cache, DiffTarget::Commit(oid1()), empty_diff());
        assert!(!cache.is_line_stats_loading(Some(DiffTarget::Commit(oid1()))));
    }

    #[test]
    fn is_line_stats_loading_false_when_quick_is_for_different_target() {
        let mut cache = DiffCache::new();
        set_quick(&mut cache, DiffTarget::Commit(oid1()), empty_diff());
        assert!(!cache.is_line_stats_loading(Some(DiffTarget::Commit(oid2()))));
    }

    // ── Target sync ─────────────────────────────────────────────────

    #[test]
    fn sync_selected_target_reports_change() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut cache = DiffCache::new();

        let (target, changed) =
            cache.sync_selected_target(Some(DiffTarget::Uncommitted), &repo);
        assert_eq!(target, Some(DiffTarget::Uncommitted));
        assert!(changed);

        // Same target again — no change, no re-render needed
        let (_, changed) = cache.sync_selected_target(Some(DiffTarget::Uncommitted), &repo);
        assert!(!changed);

        // Selection cleared — change again
        let (_, changed) = cache.sync_selected_target(None, &repo);
        assert!(changed);
    }

    // ── Poll state machine ──────────────────────────────────────────

    #[test]
    fn poll_no_receivers_no_target_returns_default_events() {
        let mut cache = DiffCache::new();
        let events = cache.poll(None, "/dev/null", None);
        assert!(!events.uncommitted_diff_loaded);
        assert!(events.message.is_none());
    }

    #[test]
    fn poll_receives_completed_commit_diff() {
        let mut cache = DiffCache::new();
        let (tx, rx) = mpsc::channel();
        cache.diff_receiver = Some(rx);
        cache.diff_loading_oid = Some(oid1());

        tx.send(DiffResult {
            oid: oid1(),
            diff: Ok(empty_diff()),
        })
        .unwrap();

        let events = cache.poll(None, "/dev/null", None);

        assert!(cache.diff_cache.as_ref().is_some());
        assert_eq!(cache.diff_cache_oid, Some(oid1()));
        assert!(cache.diff_loading_oid.is_none());
        assert!(cache.diff_receiver.is_none());
        assert!(events.message.is_none());
    }

    #[test]
    fn poll_receives_failed_commit_diff() {
        let mut cache = DiffCache::new();
        let (tx, rx) = mpsc::channel();
        cache.diff_receiver = Some(rx);
        cache.diff_loading_oid = Some(oid1());

        tx.send(DiffResult {
            oid: oid1(),
            diff: Err("something broke".to_string()),
        })
        .unwrap();

        let events = cache.poll(None, "/dev/null", None);

        assert!(cache.diff_cache.as_ref().is_none());
        assert_eq!(cache.diff_cache_oid, Some(oid1()));
        assert!(events.message.is_some());
        assert!(events.message.unwrap().contains("Failed to load diff"));
    }

    #[test]
    fn poll_receives_completed_uncommitted_diff() {
        let mut cache = DiffCache::new();
        let (tx, rx) = mpsc::channel();
        cache.uncommitted_diff_receiver = Some(rx);
        cache.uncommitted_diff_loading = true;

        let status = precise_status();
        tx.send((Ok(empty_diff()), Some(status.clone()))).unwrap();

        let events = cache.poll(None, "/dev/null", Some(&status));

        assert!(cache.cached_diff(Some(DiffTarget::Uncommitted)).is_some());
        assert!(!cache.uncommitted_diff_failed);
        assert!(!cache.uncommitted_diff_loading);
        assert!(events.uncommitted_diff_loaded);
        assert!(cache.uncommitted_cache_key.as_ref().is_some());
    }

    #[test]
    fn poll_receives_failed_uncommitted_diff() {
        let mut cache = DiffCache::new();
        let (tx, rx) = mpsc::channel();
        cache.uncommitted_diff_receiver = Some(rx);
        cache.uncommitted_diff_loading = true;

        tx.send((Err("fail".to_string()), None)).unwrap();

        let events = cache.poll(None, "/dev/null", None);

        assert!(cache.cached_diff(Some(DiffTarget::Uncommitted)).is_none());
        assert!(cache.uncommitted_diff_failed);
        assert!(!cache.uncommitted_diff_loading);
        assert!(events.message.is_some());
    }

    #[test]
    fn poll_does_not_start_load_when_debouncing() {
        let mut cache = DiffCache::new();
        cache.selected_diff_target = Some(DiffTarget::Commit(oid1()));
        cache.selected_diff_target_changed_at = Instant::now();

        // Target wants to load oid1 but debounce hasn't elapsed
        let events = cache.poll(Some(DiffTarget::Commit(oid1())), "/dev/null", None);

        assert!(cache.diff_loading_oid.is_none());
        assert!(cache.diff_receiver.is_none());
        assert!(events.message.is_none());
    }

    #[test]
    fn poll_does_not_start_load_when_already_loading_for_target() {
        let mut cache = DiffCache::new();
        cache.diff_loading_oid = Some(oid1());
        let (_tx, rx) = mpsc::channel();
        cache.diff_receiver = Some(rx);

        let _events = cache.poll(Some(DiffTarget::Commit(oid1())), "/dev/null", None);

        // Should not have replaced the receiver
        assert_eq!(cache.diff_loading_oid, Some(oid1()));
    }

    #[test]
    fn poll_does_not_start_second_load_when_one_in_flight() {
        let mut cache = DiffCache::new();
        // Commit diff is in flight for oid1
        cache.diff_loading_oid = Some(oid1());
        let (_tx, rx) = mpsc::channel();
        cache.diff_receiver = Some(rx);
        // Set debounce far in the past so it wouldn't block
        cache.selected_diff_target = Some(DiffTarget::Uncommitted);
        cache.selected_diff_target_changed_at = Instant::now() - Duration::from_secs(10);

        let _events = cache.poll(Some(DiffTarget::Uncommitted), "/dev/null", None);

        // Should not have started uncommitted load
        assert!(!cache.uncommitted_diff_loading);
    }

    #[test]
    fn poll_handles_disconnected_commit_receiver() {
        let mut cache = DiffCache::new();
        let (tx, rx) = mpsc::channel::<DiffResult>();
        cache.diff_receiver = Some(rx);
        cache.diff_loading_oid = Some(oid1());
        drop(tx);

        let events = cache.poll(None, "/dev/null", None);

        assert!(cache.diff_loading_oid.is_none());
        assert!(cache.diff_receiver.is_none());
        assert!(events.message.is_some());
        assert!(events.message.unwrap().contains("failed unexpectedly"));
    }

    #[test]
    fn poll_handles_disconnected_uncommitted_receiver() {
        let mut cache = DiffCache::new();
        let (tx, rx) = mpsc::channel::<UncommittedDiffResult>();
        cache.uncommitted_diff_receiver = Some(rx);
        cache.uncommitted_diff_loading = true;
        drop(tx);

        let status = precise_status();
        let events = cache.poll(None, "/dev/null", Some(&status));

        assert!(!cache.uncommitted_diff_loading);
        assert!(cache.uncommitted_diff_receiver.is_none());
        assert!(cache.uncommitted_diff_failed);
        assert!(events.message.is_some());
        assert!(events.message.unwrap().contains("failed unexpectedly"));
        // Cache key should be set from working_tree_status
        assert!(cache.uncommitted_cache_key.as_ref().is_some());
    }

    #[test]
    fn poll_does_not_start_load_when_already_cached() {
        let mut cache = DiffCache::new();
        set_diff(&mut cache, oid1(), empty_diff());
        // Debounce far in the past
        cache.selected_diff_target = Some(DiffTarget::Commit(oid1()));
        cache.selected_diff_target_changed_at = Instant::now() - Duration::from_secs(10);

        let _events = cache.poll(Some(DiffTarget::Commit(oid1())), "/dev/null", None);

        // Should not have started a new load
        assert!(cache.diff_loading_oid.is_none());
    }

    #[test]
    fn poll_uncommitted_diff_sets_cache_key_when_status_matches() {
        let mut cache = DiffCache::new();
        let (tx, rx) = mpsc::channel();
        cache.uncommitted_diff_receiver = Some(rx);
        cache.uncommitted_diff_loading = true;

        let status = precise_status();
        // Thread sends back the same status
        tx.send((Ok(empty_diff()), Some(status.clone()))).unwrap();

        let _events = cache.poll(None, "/dev/null", Some(&status));

        assert_eq!(cache.uncommitted_cache_key.as_ref(), Some(&status));
    }

    #[test]
    fn poll_uncommitted_diff_no_cache_key_when_status_diverged() {
        let mut cache = DiffCache::new();
        let (tx, rx) = mpsc::channel();
        cache.uncommitted_diff_receiver = Some(rx);
        cache.uncommitted_diff_loading = true;

        let old_status = precise_status();
        let new_status = precise_status_different();
        // Thread captured old status but current status changed
        tx.send((Ok(empty_diff()), Some(old_status))).unwrap();

        let _events = cache.poll(None, "/dev/null", Some(&new_status));

        // effective_status (old) != working_tree_status (new), so no key set
        assert!(cache.uncommitted_cache_key.as_ref().is_none());
    }
}
