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
    /// Comparison between two arbitrary commits, ordered `(old, new)` by commit
    /// time so the diff reads older → newer.
    Range(Oid, Oid),
}

pub type UncommittedDiffResult = (Result<CommitDiffInfo, String>, Option<WorkingTreeStatus>);

/// Result of async range-diff computation, tagged with its `(old, new)` key.
pub struct RangeDiffResult {
    pub key: (Oid, Oid),
    pub diff: Result<CommitDiffInfo, String>,
}

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
    /// Latches the uncommitted-diff failure message so it is emitted once per
    /// failure episode, not on every retry. When the working tree keeps
    /// changing between dispatch and completion the cache key never seals, so
    /// poll() re-dispatches (and re-fails) every tick; without this latch the
    /// error would re-flash continuously. Cleared on the next successful load.
    pub uncommitted_diff_error_reported: bool,
    pub uncommitted_diff_loading: bool,
    pub uncommitted_diff_receiver: Option<Receiver<UncommittedDiffResult>>,
    pub uncommitted_cache_key: Option<WorkingTreeStatus>,
    // Range (two-commit comparison) diff cache — mirrors the commit path but
    // keyed on the (old, new) OID pair.
    pub range_diff_cache: Option<CommitDiffInfo>,
    pub range_diff_key: Option<(Oid, Oid)>,
    pub range_diff_loading: Option<(Oid, Oid)>,
    pub range_diff_receiver: Option<Receiver<RangeDiffResult>>,
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
            uncommitted_diff_error_reported: false,
            uncommitted_diff_loading: false,
            uncommitted_diff_receiver: None,
            uncommitted_cache_key: None,
            range_diff_cache: None,
            range_diff_key: None,
            range_diff_loading: None,
            range_diff_receiver: None,
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
        self.range_diff_cache = None;
        self.range_diff_key = None;
        self.range_diff_loading = None;
        self.range_diff_receiver = None;
        self.clear_uncommitted();
    }

    /// Clear uncommitted diff cache only.
    pub fn clear_uncommitted(&mut self) {
        self.uncommitted_diff_cache = None;
        self.uncommitted_diff_failed = false;
        // A full clear starts a fresh episode, so re-arm the error-report latch.
        // (Deliberately NOT reset in invalidate_uncommitted — that runs on every
        // auto-refresh/file-op tick and would let the failure re-flash.)
        self.uncommitted_diff_error_reported = false;
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
                        DiffTarget::Range(old, new) => {
                            CommitDiffInfo::quick_file_list_for_range(repo, old, new).ok()
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

        // Build a lookup: path → stage statuses from the quick diff. A
        // partially-staged file legitimately appears twice (one staged row,
        // one unstaged), so this must be a multi-map — collapsing to a single
        // status per path flipped both of its rows to the last one seen.
        let mut quick_statuses: std::collections::HashMap<
            &std::path::Path,
            Vec<Option<crate::git::StageStatus>>,
        > = std::collections::HashMap::new();
        for f in &quick.files {
            quick_statuses
                .entry(f.path.as_path())
                .or_default()
                .push(f.stage_status);
        }

        // This fast path can only relabel or drop existing rows, not
        // synthesize new ones. If a path's row count changed while still
        // present in both diffs (a file became — or stopped being —
        // partially staged), bail without sealing the cache key so poll()
        // falls back to a real reload.
        let mut full_counts: std::collections::HashMap<&std::path::Path, usize> =
            std::collections::HashMap::new();
        for f in &full.files {
            *full_counts.entry(f.path.as_path()).or_default() += 1;
        }
        if full_counts
            .iter()
            .any(|(p, &n)| quick_statuses.get(p).is_some_and(|s| s.len() != n))
        {
            return;
        }
        // Likewise bail if the quick diff introduced a path this fast path
        // has no line-stats for at all (e.g. a file un-archived back into
        // the working tree) — it can't fabricate a row, so fall back to a
        // real reload instead of silently omitting the file.
        if quick_statuses.keys().any(|p| !full_counts.contains_key(p)) {
            return;
        }

        // Drop rows for paths no longer present in the quick diff — the
        // file op (restore/trash/gitignore/archive) removed them from the
        // working tree entirely, so the full diff's stale entry must not
        // linger and mask the change.
        full.files
            .retain(|f| quick_statuses.contains_key(f.path.as_path()));

        // Update stage_status on each remaining file. A path with two rows
        // is a partially-staged pair whose labels are already one of each —
        // leave them as they are.
        for file in &mut full.files {
            if let Some([status]) = quick_statuses.get(file.path.as_path()).map(Vec::as_slice) {
                file.stage_status = *status;
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
            DiffTarget::Range(old, new) => self.range_diff_key == Some((old, new)),
        }
    }

    fn is_diff_loading_for_target(&self, target: DiffTarget) -> bool {
        match target {
            DiffTarget::Commit(oid) => self.diff_loading_oid == Some(oid),
            DiffTarget::Uncommitted => self.uncommitted_diff_loading,
            DiffTarget::Range(old, new) => self.range_diff_loading == Some((old, new)),
        }
    }

    fn is_diff_debouncing_for_target(&self, target: DiffTarget) -> bool {
        self.selected_diff_target == Some(target)
            && self.selected_diff_target_changed_at.elapsed() < DIFF_LOAD_DEBOUNCE
    }

    fn has_in_flight_diff(&self) -> bool {
        self.diff_loading_oid.is_some()
            || self.uncommitted_diff_loading
            || self.range_diff_loading.is_some()
    }

    /// Get cached diff info for a specific target.
    pub fn cached_diff(&self, target: Option<DiffTarget>) -> Option<&CommitDiffInfo> {
        match target? {
            DiffTarget::Commit(oid) if self.diff_cache_oid == Some(oid) => self.diff_cache.as_ref(),
            DiffTarget::Commit(_) => None,
            DiffTarget::Uncommitted => self.uncommitted_diff_cache.as_ref(),
            DiffTarget::Range(old, new) if self.range_diff_key == Some((old, new)) => {
                self.range_diff_cache.as_ref()
            }
            DiffTarget::Range(_, _) => None,
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

        // Pull in completed results for range (two-commit) diff
        if let Some(ref receiver) = self.range_diff_receiver {
            match receiver.try_recv() {
                Ok(result) => {
                    match result.diff {
                        Ok(diff) => {
                            self.range_diff_cache = Some(diff);
                            self.range_diff_key = Some(result.key);
                        }
                        Err(e) => {
                            self.range_diff_cache = None;
                            self.range_diff_key = Some(result.key);
                            events.message = Some(format!("Failed to load diff: {e}"));
                        }
                    }
                    self.range_diff_loading = None;
                    self.range_diff_receiver = None;
                    events.diff_loaded = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.range_diff_loading = None;
                    self.range_diff_receiver = None;
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
                            // Success re-arms the failure latch for the next episode.
                            self.uncommitted_diff_error_reported = false;
                            events.uncommitted_diff_loaded = true;
                        }
                        Err(e) => {
                            self.uncommitted_diff_cache = None;
                            self.uncommitted_diff_failed = true;
                            // Report once per failure episode; a churning working
                            // tree makes poll() retry every tick, and re-arming the
                            // message each time would re-flash it continuously.
                            if !self.uncommitted_diff_error_reported {
                                self.uncommitted_diff_error_reported = true;
                                events.message = Some(format!("Failed to load diff: {e}"));
                            }
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
                    if !self.uncommitted_diff_error_reported {
                        self.uncommitted_diff_error_reported = true;
                        events.message = Some("Diff computation failed unexpectedly".to_string());
                    }
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
            DiffTarget::Range(old, new) => {
                let (tx, rx) = mpsc::channel();
                let repo_path = repo_path.to_string();

                self.range_diff_loading = Some((old, new));
                self.range_diff_receiver = Some(rx);

                thread::spawn(move || {
                    let diff = git2::Repository::open(&repo_path)
                        .map_err(|e| e.to_string())
                        .and_then(|repo| {
                            CommitDiffInfo::from_range(&repo, old, new).map_err(|e| e.to_string())
                        });

                    let _ = tx.send(RangeDiffResult {
                        key: (old, new),
                        diff,
                    });
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

    // ─── reclassify_uncommitted_staging ─────────────────────────────

    fn uncommitted_diff(
        staged: Vec<crate::git::FileDiffInfo>,
        unstaged: Vec<crate::git::FileDiffInfo>,
    ) -> CommitDiffInfo {
        let mut files = staged.clone();
        files.extend(unstaged.clone());
        CommitDiffInfo {
            files,
            total_insertions: 0,
            total_deletions: 0,
            total_files: 0,
            truncated: false,
            staged_files: staged,
            unstaged_files: unstaged,
        }
    }

    #[test]
    fn reclassify_keeps_a_partially_staged_pair_intact() {
        // a.txt is partially staged (one staged + one unstaged row); b.txt
        // just got staged. Reclassifying used to collapse a.txt into one
        // path→status entry, flipping both of its rows to the last one seen.
        let mut cache = DiffCache::new();
        cache.uncommitted_diff_cache = Some(uncommitted_diff(
            vec![diff_file("a.txt", Some(StageStatus::Staged))],
            vec![
                diff_file("a.txt", Some(StageStatus::Unstaged)),
                diff_file("b.txt", Some(StageStatus::Unstaged)),
            ],
        ));
        let quick = uncommitted_diff(
            vec![
                diff_file("a.txt", Some(StageStatus::Staged)),
                diff_file("b.txt", Some(StageStatus::Staged)),
            ],
            vec![diff_file("a.txt", Some(StageStatus::Unstaged))],
        );
        set_quick(&mut cache, DiffTarget::Uncommitted, quick);

        cache.reclassify_uncommitted_staging(Some(&precise_status()));

        let full = cache.uncommitted_diff_cache.as_ref().unwrap();
        let staged: Vec<_> = full
            .staged_files
            .iter()
            .map(|f| f.path.to_str().unwrap())
            .collect();
        let unstaged: Vec<_> = full
            .unstaged_files
            .iter()
            .map(|f| f.path.to_str().unwrap())
            .collect();
        assert_eq!(staged, vec!["a.txt", "b.txt"]);
        assert_eq!(unstaged, vec!["a.txt"]);
        assert!(cache.uncommitted_cache_key.is_some(), "fast path seals the key");
    }

    #[test]
    fn reclassify_bails_to_reload_when_a_file_becomes_partially_staged() {
        // full has one a.txt row but the quick diff now has two: the fast
        // path can't split a row, so it must leave the rows untouched and the
        // key unsealed so poll() falls back to a real reload.
        let mut cache = DiffCache::new();
        cache.uncommitted_diff_cache = Some(uncommitted_diff(
            vec![diff_file("a.txt", Some(StageStatus::Staged))],
            vec![],
        ));
        let quick = uncommitted_diff(
            vec![diff_file("a.txt", Some(StageStatus::Staged))],
            vec![diff_file("a.txt", Some(StageStatus::Unstaged))],
        );
        set_quick(&mut cache, DiffTarget::Uncommitted, quick);

        cache.reclassify_uncommitted_staging(Some(&precise_status()));

        assert!(
            cache.uncommitted_cache_key.is_none(),
            "key must stay unsealed so poll() reloads"
        );
        let full = cache.uncommitted_diff_cache.as_ref().unwrap();
        assert_eq!(full.staged_files.len(), 1);
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
    fn reclassify_drops_files_removed_from_working_tree() {
        // Regression test for restore/discard: after `r` restores a.txt, the
        // quick diff (rebuilt from the live working tree) no longer contains
        // it, but the stale full diff — loaded before the restore — still
        // does. `cached_diff_or_quick` always prefers the full diff when
        // present, so a stale row here means the restored file lingers in
        // the files pane until an unrelated cache-clearing event happens.
        let mut cache = DiffCache::new();
        cache.uncommitted_diff_cache = Some(uncommitted_diff(
            vec![],
            vec![
                diff_file("a.txt", Some(StageStatus::Unstaged)),
                diff_file("b.txt", Some(StageStatus::Unstaged)),
            ],
        ));
        // a.txt was restored — the fresh quick diff only has b.txt left.
        let quick = uncommitted_diff(
            vec![],
            vec![diff_file("b.txt", Some(StageStatus::Unstaged))],
        );
        set_quick(&mut cache, DiffTarget::Uncommitted, quick);

        let status = precise_status();
        cache.reclassify_uncommitted_staging(Some(&status));

        let full = cache.uncommitted_diff_cache.as_ref().unwrap();
        assert!(
            full.files.iter().all(|f| f.path != Path::new("a.txt")),
            "restored file must not linger in the full diff"
        );
        assert_eq!(full.files.len(), 1);
        assert_eq!(full.unstaged_files.len(), 1);
        assert_eq!(full.unstaged_files[0].path, PathBuf::from("b.txt"));

        // The fast path handled the removal on its own — the cache key is
        // sealed so poll() doesn't also kick off a redundant reload.
        assert_eq!(cache.uncommitted_cache_key.as_ref(), Some(&status));

        // The observable contract callers rely on: the file no longer shows
        // up via cached_diff_or_quick either.
        let visible = cache
            .cached_diff_or_quick(Some(DiffTarget::Uncommitted))
            .unwrap();
        assert!(visible.files.iter().all(|f| f.path != Path::new("a.txt")));
    }

    #[test]
    fn reclassify_bails_when_quick_has_a_file_full_has_never_seen() {
        // Mirror image of the removal case: an un-archive (or undo of an
        // archive) can reintroduce a file into the working tree. The fast
        // path has no line-stats for it, so it must bail and leave the key
        // unsealed rather than silently omitting the file from the full
        // diff while claiming the cache is up to date.
        let mut cache = DiffCache::new();
        cache.uncommitted_diff_cache = Some(uncommitted_diff(
            vec![],
            vec![diff_file("b.txt", Some(StageStatus::Unstaged))],
        ));
        let quick = uncommitted_diff(
            vec![],
            vec![
                diff_file("a.txt", Some(StageStatus::Unstaged)),
                diff_file("b.txt", Some(StageStatus::Unstaged)),
            ],
        );
        set_quick(&mut cache, DiffTarget::Uncommitted, quick);

        let status = precise_status();
        cache.reclassify_uncommitted_staging(Some(&status));

        assert!(
            cache.uncommitted_cache_key.is_none(),
            "key must stay unsealed so poll() reloads and picks up a.txt"
        );
        let full = cache.uncommitted_diff_cache.as_ref().unwrap();
        assert_eq!(
            full.files.len(),
            1,
            "fast path leaves full diff untouched on bail"
        );
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

    // ── Uninitialized submodule (issue #72) ─────────────────────────

    /// Build a repo containing an uncommitted, *uninitialized* submodule: a
    /// gitlink (mode 160000) staged in the index plus a matching `.gitmodules`
    /// entry, with no checkout on disk. Returns the owning `TempDir` (keeps the
    /// repo alive) and the repo path. No network or second repo needed — the
    /// gitlink points at the repo's own HEAD, since reclassify never resolves
    /// the submodule target.
    fn repo_with_uninitialized_submodule() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir(&main).unwrap();
        let git = crate::test_support::git;
        git(&main, &["init", "-q", "-b", "main"]);
        std::fs::write(main.join("a.txt"), "a").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "-qm", "c1"]);

        let sha = {
            let r = GitRepository::open(&main).unwrap();
            let sha = r.repo().head().unwrap().target().unwrap();
            sha
        };
        std::fs::write(
            main.join(".gitmodules"),
            "[submodule \"sub\"]\n\tpath = sub\n\turl = ./nonexistent\n",
        )
        .unwrap();
        // Stage a bare gitlink at `sub` without checking anything out — an
        // uninitialized submodule.
        git(
            &main,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{sha},sub"),
            ],
        );
        (tmp, main)
    }

    /// Regression coverage for the reclassify retain-by-path prune (issue #72).
    ///
    /// The concern: the full working-tree scan surfaces an uncommitted
    /// submodule, while the quick diff's `ignore_submodules(true)` was feared
    /// to suppress it — leaving the gitlink present in the full diff but absent
    /// from the quick diff, so the retain-by-path prune would wrongly drop it.
    ///
    /// In practice the premise doesn't hold: the quick diff *also* carries the
    /// gitlink (as a staged add plus a workdir-deleted row, since the submodule
    /// isn't checked out). Because the path is present in the quick diff, the
    /// retain never strips it. This test pins that invariant against a real
    /// gitlink fixture — the two precondition assertions are the real guard: if
    /// a future change ever makes the quick diff stop emitting the submodule
    /// while the full scan keeps it, the prune edge becomes reachable and the
    /// post-reclassify assertion below would start failing.
    #[test]
    fn reclassify_preserves_uninitialized_submodule_row() {
        let (_tmp, main) = repo_with_uninitialized_submodule();
        let repo = GitRepository::open(&main).unwrap();

        let full = CommitDiffInfo::from_working_tree(repo.repo()).unwrap();
        let quick = CommitDiffInfo::quick_file_list_for_working_tree(repo.repo()).unwrap();

        // Precondition 1: the full scan surfaces the uninitialized submodule.
        assert!(
            full.files.iter().any(|f| f.path == Path::new("sub")),
            "full working-tree diff should surface the uninitialized submodule gitlink"
        );
        // Precondition 2: the quick diff carries it too — this is the invariant
        // that keeps the retain-by-path prune from ever dropping the gitlink.
        assert!(
            quick.files.iter().any(|f| f.path == Path::new("sub")),
            "quick diff must also carry the submodule, or the reclassify prune edge becomes reachable"
        );

        let mut cache = DiffCache::new();
        cache.uncommitted_diff_cache = Some(full);
        cache.quick_diff_target = Some(DiffTarget::Uncommitted);
        cache.quick_diff_cache = Some(quick);

        cache.reclassify_uncommitted_staging(Some(&precise_status()));

        // The submodule row must survive the retain-by-path prune...
        let cached = cache.uncommitted_diff_cache.as_ref().unwrap();
        assert!(
            cached.files.iter().any(|f| f.path == Path::new("sub")),
            "reclassify must not prune the uninitialized submodule row"
        );
        // ...and stay visible through the accessor the files pane actually reads.
        let visible = cache
            .cached_diff_or_quick(Some(DiffTarget::Uncommitted))
            .unwrap();
        assert!(
            visible.files.iter().any(|f| f.path == Path::new("sub")),
            "submodule must stay visible in the files pane after reclassify"
        );
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

    // ── Range (two-commit comparison) caching ───────────────────────

    #[test]
    fn range_has_cached_diff_checks_key() {
        let mut cache = DiffCache::new();
        cache.range_diff_cache = Some(empty_diff());
        cache.range_diff_key = Some((oid1(), oid2()));

        assert!(cache.has_cached_diff_for_target(DiffTarget::Range(oid1(), oid2())));
        // A different pair (or reversed order) is a distinct target.
        assert!(!cache.has_cached_diff_for_target(DiffTarget::Range(oid2(), oid1())));
    }

    #[test]
    fn range_cached_diff_returns_only_on_matching_key() {
        let mut cache = DiffCache::new();
        let mut full = empty_diff();
        full.total_files = 7;
        cache.range_diff_cache = Some(full);
        cache.range_diff_key = Some((oid1(), oid2()));

        assert_eq!(
            cache
                .cached_diff(Some(DiffTarget::Range(oid1(), oid2())))
                .unwrap()
                .total_files,
            7
        );
        assert!(cache.cached_diff(Some(DiffTarget::Range(oid2(), oid1()))).is_none());
    }

    #[test]
    fn poll_receives_completed_range_diff() {
        let mut cache = DiffCache::new();
        let (tx, rx) = mpsc::channel();
        cache.range_diff_receiver = Some(rx);
        cache.range_diff_loading = Some((oid1(), oid2()));

        tx.send(RangeDiffResult {
            key: (oid1(), oid2()),
            diff: Ok(empty_diff()),
        })
        .unwrap();

        let events = cache.poll(None, "/dev/null", None);

        assert!(cache.cached_diff(Some(DiffTarget::Range(oid1(), oid2()))).is_some());
        assert_eq!(cache.range_diff_key, Some((oid1(), oid2())));
        assert!(cache.range_diff_loading.is_none());
        assert!(cache.range_diff_receiver.is_none());
        assert!(events.diff_loaded);
        assert!(events.message.is_none());
    }

    #[test]
    fn range_in_flight_blocks_starting_other_loads() {
        let mut cache = DiffCache::new();
        // A range diff is in flight; a commit target must not start a second
        // heavy load until it finishes.
        cache.range_diff_loading = Some((oid1(), oid2()));
        let (_tx, rx) = mpsc::channel();
        cache.range_diff_receiver = Some(rx);
        cache.selected_diff_target = Some(DiffTarget::Commit(oid1()));
        cache.selected_diff_target_changed_at = Instant::now() - Duration::from_secs(10);

        let _events = cache.poll(Some(DiffTarget::Commit(oid1())), "/dev/null", None);

        assert!(cache.diff_loading_oid.is_none());
    }

    #[test]
    fn clear_all_clears_range_cache() {
        let mut cache = DiffCache::new();
        cache.range_diff_cache = Some(empty_diff());
        cache.range_diff_key = Some((oid1(), oid2()));
        cache.range_diff_loading = Some((oid1(), oid2()));

        cache.clear_all();

        assert!(cache.cached_diff(Some(DiffTarget::Range(oid1(), oid2()))).is_none());
        assert!(cache.range_diff_key.is_none());
        assert!(cache.range_diff_loading.is_none());
    }

    // ── Uncommitted diff error-report latch (once-per-episode) ──────────

    /// Feed one completed uncommitted-diff result through poll. `target: None`
    /// makes poll return after draining receivers, so no real load is spawned.
    fn poll_uncommitted_result(
        cache: &mut DiffCache,
        result: Result<CommitDiffInfo, String>,
    ) -> Option<String> {
        let (tx, rx) = mpsc::channel::<UncommittedDiffResult>();
        tx.send((result, None)).unwrap();
        cache.uncommitted_diff_loading = true;
        cache.uncommitted_diff_receiver = Some(rx);
        cache.poll(None, "", None).message
    }

    #[test]
    fn uncommitted_diff_failure_reports_once_per_episode() {
        let mut cache = DiffCache::new();

        // First failure of an episode surfaces a message…
        let first = poll_uncommitted_result(&mut cache, Err("boom".into()));
        assert!(first.is_some(), "first failure should report");
        assert!(cache.uncommitted_diff_error_reported);

        // …a retry that fails again (working tree churned, key never sealed) is
        // latched and stays silent — this is the anti-re-flash guarantee.
        let second = poll_uncommitted_result(&mut cache, Err("boom again".into()));
        assert!(second.is_none(), "repeat failure must not re-flash");
    }

    #[test]
    fn uncommitted_diff_success_rearms_the_latch() {
        let mut cache = DiffCache::new();

        assert!(poll_uncommitted_result(&mut cache, Err("boom".into())).is_some());
        // A success clears the latch and emits no message.
        assert!(poll_uncommitted_result(&mut cache, Ok(empty_diff())).is_none());
        assert!(!cache.uncommitted_diff_error_reported, "success re-arms");
        // A subsequent, distinct failure episode reports again.
        assert!(poll_uncommitted_result(&mut cache, Err("boom".into())).is_some());
    }

    #[test]
    fn clear_uncommitted_rearms_the_latch() {
        let mut cache = DiffCache::new();
        assert!(poll_uncommitted_result(&mut cache, Err("boom".into())).is_some());
        cache.clear_uncommitted();
        assert!(!cache.uncommitted_diff_error_reported, "full clear starts a new episode");
        assert!(poll_uncommitted_result(&mut cache, Err("boom".into())).is_some());
    }

    #[test]
    fn invalidate_uncommitted_keeps_the_latch() {
        // The per-tick churn path must NOT re-arm, or the failure re-flashes.
        let mut cache = DiffCache::new();
        assert!(poll_uncommitted_result(&mut cache, Err("boom".into())).is_some());
        cache.invalidate_uncommitted();
        assert!(
            cache.uncommitted_diff_error_reported,
            "invalidate must preserve the latch so the error stays silent"
        );
    }
}
