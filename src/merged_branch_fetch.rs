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
    pub shallow: Option<Vec<u8>>,
}

impl ClassifyInput {
    /// A cheap fingerprint of the inputs. When it's unchanged since the last
    /// spawn, the result would be identical, so no worker is spawned — this is
    /// what keeps a frequent refresh from re-running any git diffing.
    ///
    /// Also the key of the persistent classification cache
    /// ([`crate::merged_cache`]): the on-disk result is trusted only when this
    /// fingerprint matches, so the same notion of input identity guards both the
    /// in-process skip and the cross-session cache. Order-independent over the
    /// branch and gh-name sets; `repo_path` is deliberately excluded (the cache
    /// is already keyed per-repo on disk, and a moved checkout of the same repo
    /// classifies identically).
    pub fn signature(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.shallow.hash(&mut h);
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
            b.ahead.hash(&mut bh);
            b.behind.hash(&mut bh);
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
    task: crate::background::LatestTask<MergedClassification>,
    /// Signature of the input currently in flight or last completed. A new
    /// request with the same signature is a no-op.
    last_signature: Option<u64>,
    /// The gh-merged set of that same input, retained alongside `last_signature`
    /// so the persistent cache can be written with the exact inputs that
    /// produced the delivered result — even if the live gh set has since changed
    /// (the on-disk signature must match its stored result, or a later startup
    /// would trust a mismatched entry). See [`crate::merged_cache`].
    last_gh_merged: Option<HashSet<String>>,
}

impl Default for MergedClassifier {
    fn default() -> Self {
        Self::new()
    }
}

impl MergedClassifier {
    pub fn new() -> Self {
        Self {
            task: Default::default(),
            last_signature: None,
            last_gh_merged: None,
        }
    }

    /// Schedule the latest changed inputs on one bounded worker.
    /// Idempotent for unchanged inputs (the signature guard), so it's safe to
    /// call on every refresh.
    pub fn maybe_start(&mut self, input: ClassifyInput) {
        let sig = input.signature();
        if self.last_signature == Some(sig) {
            return;
        }
        self.last_signature = Some(sig);
        self.last_gh_merged = Some(input.gh_merged.clone());
        self.task.submit(true, move || classify(&input));
    }

    /// Poll for a completed classification. Returns the merged-branch-name set
    /// and squash-target map on completion, else `None`.
    pub fn poll(&mut self) -> Option<MergedClassification> {
        match self.task.take()? {
            Ok(result) => Some(result),
            Err(_) => {
                self.last_signature = None;
                self.last_gh_merged = None;
                None
            }
        }
    }

    pub fn cancel(&mut self) {
        self.task.cancel();
        self.last_signature = None;
        self.last_gh_merged = None;
    }

    /// Fingerprint of the input whose result is currently reflected (in flight or
    /// last completed). Read straight after [`Self::poll`] returns `Some`, it is
    /// the signature of the just-delivered result — the key to persist it under.
    pub fn last_signature(&self) -> Option<u64> {
        self.last_signature
    }

    /// The gh-merged set that goes with [`Self::last_signature`], for the same
    /// use: persisting a cache entry whose signature matches its stored result.
    pub fn last_gh_merged(&self) -> Option<&HashSet<String>> {
        self.last_gh_merged.as_ref()
    }

    /// Record that a result with this `signature`/`gh_merged` is already applied
    /// to app state — loaded from the persistent cache at startup rather than
    /// computed here. A subsequent [`Self::maybe_start`] with the same inputs
    /// then skips the redundant background run (same guard as an in-process
    /// repeat), so an exact cache hit costs no git diffing at all.
    pub fn note_cached(&mut self, signature: u64, gh_merged: HashSet<String>) {
        self.last_signature = Some(signature);
        self.last_gh_merged = Some(gh_merged);
    }
}

/// Open failures remain failures: never persist an unavailable repository as
/// an authoritative empty classification, and allow the same inputs to retry.
fn classify(input: &ClassifyInput) -> Result<MergedClassification, String> {
    let repo = Repository::open(&input.repo_path).map_err(|e| e.to_string())?;
    Ok(crate::git::merged::classify_merged_branches_with_targets(
        &repo,
        &input.branches,
        input.base_tip,
        &input.base_name,
        &input.gh_merged,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classification_fixture() -> (tempfile::TempDir, ClassifyInput) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repo");
        let repo = Repository::init(&path).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree = repo
            .find_tree(repo.treebuilder(None).unwrap().write().unwrap())
            .unwrap();
        let root = repo.commit(None, &sig, &sig, "root", &tree, &[]).unwrap();
        let root = repo.find_commit(root).unwrap();
        let feature = repo
            .commit(None, &sig, &sig, "feature", &tree, &[&root])
            .unwrap();
        let feature_commit = repo.find_commit(feature).unwrap();
        let tip = repo
            .commit(None, &sig, &sig, "merge", &tree, &[&root, &feature_commit])
            .unwrap();
        let branch = |name: &str, tip_oid, is_head| BranchInfo {
            name: name.into(),
            tip_oid,
            is_head,
            is_remote: false,
            upstream: None,
            ahead: 0,
            behind: 0,
        };
        let input = ClassifyInput {
            repo_path: path.to_string_lossy().into_owned(),
            branches: vec![branch("main", tip, true), branch("feature", feature, false)],
            base_name: "main".into(),
            base_tip: tip,
            gh_merged: HashSet::new(),
            shallow: None,
        };
        (dir, input)
    }

    #[test]
    fn tracking_configuration_changes_both_verdict_and_cache_identity() {
        let (_dir, mut input) = classification_fixture();
        let before = input.signature();
        assert!(classify(&input).unwrap().0.contains("feature"));
        input.branches[1].upstream = Some("origin/feature".into());
        input.branches[1].behind = 1;
        assert_ne!(before, input.signature());
        assert!(!classify(&input).unwrap().0.contains("feature"));
    }

    #[test]
    fn failed_open_does_not_seal_the_signature_and_same_input_can_retry() {
        let (dir, input) = classification_fixture();
        let path = std::path::Path::new(&input.repo_path);
        let moved = dir.path().join("unavailable");
        std::fs::rename(path, &moved).unwrap();
        let mut classifier = MergedClassifier::new();
        classifier.maybe_start(input.clone());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while classifier.last_signature().is_some() {
            assert!(
                classifier.poll().is_none(),
                "failure must not become an empty success"
            );
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        std::fs::rename(moved, path).unwrap();
        classifier.maybe_start(input);
        loop {
            if let Some((merged, _)) = classifier.poll() {
                assert!(merged.contains("feature"));
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

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
