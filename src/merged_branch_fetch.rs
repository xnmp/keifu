//! Squash-merge detection via GitHub PR state (`gh pr list --state merged`).
//!
//! This is the *primary* signal for issue #60: when a PR is squash-merged and
//! its remote branch deleted, the surviving local branch shares no commit with
//! the trunk, so local ancestry can't see the merge — but GitHub still knows the
//! PR merged, and its `headRefName` is exactly that branch's name.
//!
//! Non-blocking by construction, mirroring the open-PR fetch: the fetch runs on
//! a background thread via the generic [`crate::interval_fetch::IntervalFetch`]
//! and the UI polls a channel. If `gh` is missing, errors, or times out, the
//! producer returns `Err` — surfaced so the caller can latch/toast it once
//! (issue #65); the local patch-id fallback still classifies branches meanwhile.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use serde::Deserialize;

use crate::interval_fetch::IntervalFetch;

/// Interval between merged-PR refreshes (also the back-off after a failure).
/// Merged state changes slowly, so this is deliberately coarse.
const MERGED_FETCH_INTERVAL: Duration = Duration::from_secs(300);
/// Hard cap on a single `gh` invocation so a hung CLI can't leak a thread.
const GH_TIMEOUT: Duration = Duration::from_secs(10);

/// Raw `gh pr list --state merged --json headRefName` record. Only the head ref
/// matters; everything else is ignored.
#[derive(Debug, Deserialize)]
struct GhMergedPr {
    #[serde(rename = "headRefName")]
    head_ref_name: String,
}

/// Parse `gh pr list --state merged --json headRefName` output into the set of
/// merged head-branch names. Malformed JSON yields an empty set; blank head refs
/// are dropped.
pub fn parse_merged_pr_list(json: &str) -> HashSet<String> {
    let records: Vec<GhMergedPr> = serde_json::from_str(json).unwrap_or_default();
    records
        .into_iter()
        .map(|p| p.head_ref_name)
        .filter(|n| !n.is_empty())
        .collect()
}

/// Build the background merged-PR-branch fetcher: `gh pr list --state merged`
/// every [`MERGED_FETCH_INTERVAL`] on a worker thread, routed through the generic
/// [`IntervalFetch`].
pub fn merged_branch_fetch() -> IntervalFetch<HashSet<String>> {
    IntervalFetch::new(MERGED_FETCH_INTERVAL, fetch_merged_branches)
}

/// Run `gh pr list --state merged` in `repo_path`, returning the set of merged
/// head-branch names. `Err` (surfaced by the caller) on gh-missing, timeout, or
/// a non-zero exit — so a transient failure is observable rather than silently
/// emptying the merged-head set.
pub fn fetch_merged_branches(repo_path: &str) -> Result<HashSet<String>, String> {
    let out = crate::gh::run(
        repo_path,
        &[
            "pr",
            "list",
            "--state",
            "merged",
            "--json",
            "headRefName",
            "--limit",
            "100",
        ],
        GH_TIMEOUT,
    )?;
    if !out.success {
        return Err(if out.stderr.is_empty() {
            "gh pr list (merged) failed".to_string()
        } else {
            out.stderr
        });
    }
    Ok(parse_merged_pr_list(&out.stdout))
}

// ── Local merged-branch classification, run off the UI thread ────────────────

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use git2::{Oid, Repository};

use crate::git::BranchInfo;

/// A snapshot of everything the local classification depends on. Owned + `Send`
/// so it can cross to a worker thread. The worker reopens the repo by path (a
/// separate read-only handle, safe alongside the UI thread's handle).
#[derive(Clone)]
pub struct ClassifyInput {
    pub repo_path: String,
    pub branches: Vec<BranchInfo>,
    pub base_name: String,
    pub base_tip: Oid,
    pub gh_merged: HashSet<String>,
}

impl ClassifyInput {
    /// A cheap fingerprint of the inputs. When it's unchanged since the last
    /// spawn, the result would be identical, so no worker is spawned — this is
    /// what keeps a frequent refresh from re-running any git diffing.
    fn signature(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.base_name.hash(&mut h);
        self.base_tip.hash(&mut h);
        // Order-independent over branches and gh names (XOR of per-item hashes),
        // since neither collection has a stable iteration order.
        let mut branch_acc: u64 = 0;
        for b in &self.branches {
            let mut bh = std::collections::hash_map::DefaultHasher::new();
            b.name.hash(&mut bh);
            b.tip_oid.hash(&mut bh);
            b.is_remote.hash(&mut bh);
            b.is_head.hash(&mut bh);
            branch_acc ^= bh.finish();
        }
        branch_acc.hash(&mut h);
        let mut gh_acc: u64 = 0;
        for n in &self.gh_merged {
            let mut nh = std::collections::hash_map::DefaultHasher::new();
            n.hash(&mut nh);
            gh_acc ^= nh.finish();
        }
        gh_acc.hash(&mut h);
        h.finish()
    }
}

/// Background classifier for merged branches. Mirrors [`MergedBranchFetch`]: the
/// (potentially expensive: ancestry + bounded patch-id scans per branch)
/// classification runs on a worker thread and the UI polls a channel, so a
/// refresh never does git diffing inline on the UI thread.
/// The classifier's result: the set of merged branch names, plus a `branch name
/// → squash landing commit` map for the squash-merged ones (drives the link
/// line, issue #81). See [`crate::git::merged::classify_merged_branches_with_targets`].
pub type MergedClassification = (HashSet<String>, HashMap<String, Oid>);

pub struct MergedClassifier {
    receiver: Option<Receiver<MergedClassification>>,
    /// Signature of the input currently in flight or last completed. A new
    /// request with the same signature is a no-op.
    last_signature: Option<u64>,
}

impl Default for MergedClassifier {
    fn default() -> Self {
        Self::new()
    }
}

impl MergedClassifier {
    pub fn new() -> Self {
        Self {
            receiver: None,
            last_signature: None,
        }
    }

    /// Spawn a classification when the inputs changed and none is in flight.
    /// Idempotent for unchanged inputs (the signature guard), so it's safe to
    /// call on every refresh.
    pub fn maybe_start(&mut self, input: ClassifyInput) {
        if self.receiver.is_some() {
            return;
        }
        let sig = input.signature();
        if self.last_signature == Some(sig) {
            return;
        }
        self.last_signature = Some(sig);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(classify(&input));
        });
        self.receiver = Some(rx);
    }

    /// Poll for a completed classification. Returns the merged-branch-name set
    /// and squash-target map on completion, else `None`.
    pub fn poll(&mut self) -> Option<MergedClassification> {
        let rx = self.receiver.as_ref()?;
        match rx.try_recv() {
            Ok(result) => {
                self.receiver = None;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                // Worker died; allow a future maybe_start with the same inputs to
                // retry by clearing the remembered signature.
                self.receiver = None;
                self.last_signature = None;
                None
            }
        }
    }
}

/// Open the repo by path and run the pure classifier. An unopenable repo yields
/// an empty result (the feature is silently absent, like the gh path).
fn classify(input: &ClassifyInput) -> MergedClassification {
    let Ok(repo) = Repository::open(&input.repo_path) else {
        return (HashSet::new(), HashMap::new());
    };
    crate::git::merged::classify_merged_branches_with_targets(
        &repo,
        &input.branches,
        input.base_tip,
        &input.base_name,
        &input.gh_merged,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_merged_head_refs() {
        let json = r#"[
            {"headRefName": "feat/x"},
            {"headRefName": "fix/y"}
        ]"#;
        let set = parse_merged_pr_list(json);
        assert_eq!(set.len(), 2);
        assert!(set.contains("feat/x"));
        assert!(set.contains("fix/y"));
    }

    #[test]
    fn drops_blank_head_refs() {
        let json = r#"[{"headRefName": ""}, {"headRefName": "keep"}]"#;
        let set = parse_merged_pr_list(json);
        assert_eq!(set.len(), 1);
        assert!(set.contains("keep"));
    }

    #[test]
    fn malformed_or_empty_json_yields_empty_set() {
        assert!(parse_merged_pr_list("not json").is_empty());
        assert!(parse_merged_pr_list("").is_empty());
        assert!(parse_merged_pr_list("[]").is_empty());
    }
}
