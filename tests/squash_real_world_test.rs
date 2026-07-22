//! Real-world squash-merge classification, run against a genuine public repo
//! that squash-merges its PRs (casey/just: single-parent "Title (#N)" landing
//! commits on `master`).
//!
//! This test is both a regression net and an **investigation instrument** for
//! issue #100 ("squash-merged branches still not hidden in real repos"). It
//! clones the upstream repo, fetches the surviving `refs/pull/N/head` refs of
//! recently-merged PRs (GitHub keeps these even after the PR branch is deleted),
//! materialises them as branches, and asserts that
//! [`classify_merged_branches_with_targets`] flags them as merged and names the
//! correct squash landing commit.
//!
//! It exercises TWO shapes the offline unit tests could not:
//!  - the PR head as a **local** branch (the surviving-local-ref case, #60/#82);
//!  - the PR head as a **remote-only** branch `origin/<head>` (the surviving-
//!    *remote*-ref case — the common GitHub-squash-and-keep-branch outcome, #100
//!    H1). Before the #100 fix, remote branches were never classified; this test
//!    pins that they now are.
//!
//! Network-dependent, so it is `#[ignore]`. Run with:
//!
//! ```sh
//! cargo test --test squash_real_world_test -- --ignored --nocapture
//! ```
//!
//! It **skips gracefully** (returns without failing) when offline or when `git`/
//! `gh` are unavailable, so it never breaks a disconnected `--ignored` run. The
//! clone is cached under the system temp dir keyed by repo name, so reruns are
//! cheap.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use git2::{Oid, Repository};
use keifu::git::branch::BranchInfo;
use keifu::git::merged::classify_merged_branches_with_targets;

const REPO_SLUG: &str = "casey/just";
const REPO_URL: &str = "https://github.com/casey/just.git";
const DEFAULT_BRANCH: &str = "master";
/// How many recent merged PRs to probe.
const PR_SAMPLE: usize = 12;

/// A merged PR we will try to classify: its number, head-branch name, and the
/// squash landing commit GitHub recorded (`mergeCommit.oid`).
struct MergedPr {
    number: u64,
    head_ref: String,
    merge_commit: Oid,
}

fn cache_dir() -> PathBuf {
    std::env::temp_dir().join("keifu-squash-wild").join("just")
}

/// Run a command, returning stdout on success (`None` on spawn failure or a
/// non-zero exit) — used for the network steps so any failure degrades to a skip.
fn run(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    run(Command::new("git").arg("-C").arg(dir).args(args))
}

/// Ensure a cached clone exists; clone it if not. `None` on any failure (offline,
/// no `git`, …) so the caller can skip.
fn ensure_clone() -> Option<PathBuf> {
    let dir = cache_dir();
    if dir.join(".git").is_dir() {
        // Refresh the default branch so the squash commits of recent PRs are
        // present; ignore failure (we can still work with the cached state).
        let _ = git(&dir, &["fetch", "--quiet", "origin", DEFAULT_BRANCH]);
        return Some(dir);
    }
    std::fs::create_dir_all(dir.parent()?).ok()?;
    // A normal clone: classification needs trees+blobs for the patch-id diff, and
    // a full clone is reliably offline afterwards. casey/just is ~17MB.
    run(Command::new("git").args(["clone", "--quiet", REPO_URL, dir.to_str()?]))?;
    Some(dir)
}

/// Recent merged PRs whose landing commit is a **single-parent squash** (skip the
/// occasional merge-commit PR). Uses `gh`; `None` when `gh`/auth/network is absent.
fn fetch_merged_prs() -> Option<Vec<MergedPr>> {
    let json = run(Command::new("gh").args([
        "pr",
        "list",
        "-R",
        REPO_SLUG,
        "--state",
        "merged",
        "--limit",
        &PR_SAMPLE.to_string(),
        "--json",
        "number,headRefName,mergeCommit",
    ]))?;
    let parsed: serde_json::Value = serde_json::from_str(&json).ok()?;
    let mut prs = Vec::new();
    for item in parsed.as_array()? {
        let number = item.get("number")?.as_u64()?;
        let head_ref = item.get("headRefName")?.as_str()?.to_string();
        let oid_str = item
            .get("mergeCommit")
            .and_then(|m| m.get("oid"))
            .and_then(|o| o.as_str());
        let Some(oid_str) = oid_str else { continue };
        let Ok(merge_commit) = Oid::from_str(oid_str) else {
            continue;
        };
        prs.push(MergedPr {
            number,
            head_ref,
            merge_commit,
        });
    }
    Some(prs)
}

/// Fetch `refs/pull/<n>/head` into a local ref, returning the head OID. `None`
/// when the PR head is unreachable (rare) or the fetch fails.
fn fetch_pr_head(dir: &Path, number: u64) -> Option<Oid> {
    let refspec = format!("refs/pull/{number}/head:refs/keifu-test/pr-{number}");
    git(dir, &["fetch", "--quiet", "origin", &refspec])?;
    let out = git(dir, &["rev-parse", &format!("refs/keifu-test/pr-{number}")])?;
    Oid::from_str(out.trim()).ok()
}

fn local(name: &str, tip: Oid, head: bool) -> BranchInfo {
    BranchInfo {
        name: name.into(),
        is_head: head,
        is_remote: false,
        upstream: None,
        tip_oid: tip,
        ahead: 0,
        behind: 0,
    }
}

fn remote(name: &str, tip: Oid) -> BranchInfo {
    BranchInfo {
        name: name.into(),
        is_head: false,
        is_remote: true,
        upstream: None,
        tip_oid: tip,
        ahead: 0,
        behind: 0,
    }
}

#[test]
#[ignore = "network: clones casey/just and fetches PR refs — run with --ignored"]
fn squash_merged_pr_heads_are_classified_against_a_real_repo() {
    let Some(dir) = ensure_clone() else {
        eprintln!("SKIP: could not clone {REPO_SLUG} (offline or no git)");
        return;
    };
    let Some(prs) = fetch_merged_prs() else {
        eprintln!("SKIP: could not list merged PRs (no gh/auth/network)");
        return;
    };
    let Ok(repo) = Repository::open(&dir) else {
        eprintln!("SKIP: could not open cloned repo");
        return;
    };
    // The trunk tip to measure against: origin/<default>.
    let Some(base_tip) = git(&dir, &["rev-parse", &format!("origin/{DEFAULT_BRANCH}")])
        .and_then(|s| Oid::from_str(s.trim()).ok())
    else {
        eprintln!("SKIP: could not resolve origin/{DEFAULT_BRANCH}");
        return;
    };

    // Materialise the PR heads and record which landing commit each should map to.
    // Skip PRs whose landing commit isn't a single-parent squash, or whose squash
    // commit is not present locally (a merge-commit PR, or history too shallow).
    let mut heads: Vec<(u64, String, Oid, Oid)> = Vec::new(); // (num, head_ref, pr_tip, expected_target)
    for pr in &prs {
        // Only single-parent landings are squashes.
        let is_squash = repo
            .find_commit(pr.merge_commit)
            .map(|c| c.parent_count() == 1)
            .unwrap_or(false);
        if !is_squash {
            eprintln!(
                "skip PR #{}: landing commit is not a single-parent squash",
                pr.number
            );
            continue;
        }
        let Some(pr_tip) = fetch_pr_head(&dir, pr.number) else {
            eprintln!(
                "skip PR #{}: could not fetch refs/pull/{}/head",
                pr.number, pr.number
            );
            continue;
        };
        heads.push((pr.number, pr.head_ref.clone(), pr_tip, pr.merge_commit));
    }

    if heads.is_empty() {
        eprintln!("SKIP: no squash-merged PR heads could be materialised");
        return;
    }
    eprintln!(
        "Materialised {} squash-merged PR heads from {REPO_SLUG}",
        heads.len()
    );

    // ── Shape 1: PR heads as LOCAL branches (surviving-local-ref case) ──────────
    let mut branches: Vec<BranchInfo> = vec![local(DEFAULT_BRANCH, base_tip, true)];
    for (num, _head, tip, _t) in &heads {
        branches.push(local(&format!("pr-{num}"), *tip, false));
    }
    let (merged_local, targets_local) = classify_merged_branches_with_targets(
        &repo,
        &branches,
        base_tip,
        DEFAULT_BRANCH,
        &HashSet::new(),
    );

    let mut local_hits = 0usize;
    let mut local_misses = Vec::new();
    for (num, head, _tip, expected) in &heads {
        let name = format!("pr-{num}");
        let classified = merged_local.contains(&name);
        let target = targets_local.get(&name).copied();
        if classified {
            local_hits += 1;
            eprintln!(
                "  LOCAL  pr-{num} ({head}): MERGED  target={:?} expected={} match={}",
                target.map(|o| o.to_string()),
                expected,
                target == Some(*expected),
            );
        } else {
            local_misses.push((*num, head.clone()));
            eprintln!("  LOCAL  pr-{num} ({head}): NOT classified");
        }
    }

    // ── Shape 2: PR heads as REMOTE branches origin/<head> (surviving-remote) ───
    let mut rbranches: Vec<BranchInfo> = vec![local(DEFAULT_BRANCH, base_tip, true)];
    for (_num, head, tip, _t) in &heads {
        rbranches.push(remote(&format!("origin/{head}"), *tip));
    }
    let (merged_remote, _targets_remote) = classify_merged_branches_with_targets(
        &repo,
        &rbranches,
        base_tip,
        DEFAULT_BRANCH,
        &HashSet::new(),
    );
    let remote_hits = heads
        .iter()
        .filter(|(_n, head, _t, _e)| merged_remote.contains(&format!("origin/{head}")))
        .count();

    eprintln!(
        "\nSUMMARY: {}/{} local PR heads classified; {}/{} remote PR heads classified",
        local_hits,
        heads.len(),
        remote_hits,
        heads.len(),
    );

    // The core assertion: the local squash-merged PR heads must be classified,
    // with the correct squash landing commit as target. A clean squash repo like
    // casey/just should classify essentially all of them; we require the strong
    // majority so an occasional conflict-resolved or base-drifted landing (which
    // legitimately needs the gh signal) doesn't make the net flaky.
    assert!(
        local_hits * 2 > heads.len(),
        "expected most squash-merged PR heads to be classified as merged locally, \
         got {local_hits}/{}. Misses: {:?}",
        heads.len(),
        local_misses,
    );
    // Every local hit must name the right landing commit (this is the #81 link
    // target). A hit without a target, or with the wrong one, is a real bug.
    for (num, head, _tip, expected) in &heads {
        let name = format!("pr-{num}");
        if merged_local.contains(&name) {
            assert_eq!(
                targets_local.get(&name).copied(),
                Some(*expected),
                "pr-{num} ({head}) classified merged but named the wrong squash target",
            );
        }
    }
    // The #100 H1 fix: surviving *remote* PR-head refs must classify too.
    assert!(
        remote_hits * 2 > heads.len(),
        "expected remote-only squash-merged PR heads to classify after the #100 H1 fix, \
         got {remote_hits}/{}",
        heads.len(),
    );
}
