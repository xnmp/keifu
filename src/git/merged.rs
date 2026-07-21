//! Merged-branch classification: "has this branch already landed on the trunk?"
//!
//! Two shapes of "merged" have to be recognised:
//!
//!  - **merge-commit / fast-forward merges** — every commit of the branch is an
//!    ancestor of the base, so the branch tip is reachable from the base tip.
//!    Cheap: a single ancestry query.
//!
//!  - **squash merges** — the PR was squashed into one commit on the base and
//!    the branch's own ref survived (the remote copy was deleted, the local one
//!    lingers). *No commit is shared*, so ancestry sees nothing; instead the
//!    branch's cumulative diff since the fork point carries the same **patch-id**
//!    as the squashed commit on the base. This is the `git cherry` / patch-id
//!    idea applied to the whole branch rather than commit-by-commit, which is
//!    what catches a squash. See issue #60.
//!
//! Pure functions over a `git2::Repository`; no UI, no caching (the caller
//! memoises). The GitHub-PR signal (a branch whose head matches a *merged* PR)
//! is layered on top by the caller — this module is the local-git fallback that
//! also works for non-GitHub repos.

use std::collections::{HashMap, HashSet};

use git2::{Oid, Repository, Tree};

use super::BranchInfo;

/// Upper bound on base-branch commits scanned when hunting for a squash-merge
/// equivalent, so a very long trunk history can't stall a refresh. A squashed
/// commit lands at the tip when it merges and only moves further back as the
/// trunk advances; this window comfortably covers active branches.
const SQUASH_SCAN_LIMIT: usize = 400;

/// Whether the work on `branch_tip` is already present in `base_tip` — i.e. the
/// branch has been merged, by a merge commit, a fast-forward, or a squash.
pub fn is_merged_into(repo: &Repository, branch_tip: Oid, base_tip: Oid) -> bool {
    if branch_tip == base_tip {
        return true;
    }
    is_ancestor_merged(repo, branch_tip, base_tip) || is_squash_merged(repo, branch_tip, base_tip)
}

/// Cheap ancestry test: `branch_tip` is contained in `base_tip`'s history, as
/// produced by a merge commit or a fast-forward merge.
pub fn is_ancestor_merged(repo: &Repository, branch_tip: Oid, base_tip: Oid) -> bool {
    repo.graph_descendant_of(base_tip, branch_tip)
        .unwrap_or(false)
}

/// Patch-id test for squash merges: the branch's whole diff since the fork point
/// matches the diff of some single-parent commit on the base branch — the
/// squashed commit that introduced the branch's changes wholesale.
///
/// Returns `false` for unrelated histories and for branches whose changes never
/// landed. Bounded by [`SQUASH_SCAN_LIMIT`].
pub fn is_squash_merged(repo: &Repository, branch_tip: Oid, base_tip: Oid) -> bool {
    let Ok(fork) = repo.merge_base(branch_tip, base_tip) else {
        return false; // unrelated histories → nothing was merged
    };
    if fork == branch_tip {
        // Branch fully contained in the base — an ancestry merge, already true
        // via `is_ancestor_merged`; report it here too for a standalone call.
        return true;
    }
    squash_target_from_fork(repo, fork, branch_tip, base_tip).is_some()
}

/// The **squash commit** on the base that landed `branch_tip`'s work: the single
/// commit on the base whose diff carries the same patch-id as the branch's
/// cumulative diff since the fork point. `Some(oid)` names it; `None` when there
/// is no such squash (unrelated histories, an ancestry/fast-forward merge with
/// no distinct squash commit, or a branch that never landed).
///
/// This is the concrete counterpart to [`is_squash_merged`]: same detection, but
/// it returns *which* commit matched so a link line can be drawn to it (issue
/// #81). Bounded by [`SQUASH_SCAN_LIMIT`].
pub fn squash_merge_target(repo: &Repository, branch_tip: Oid, base_tip: Oid) -> Option<Oid> {
    let fork = repo.merge_base(branch_tip, base_tip).ok()?;
    if fork == branch_tip {
        // Fully-contained branch: an ancestry merge, not a squash — there is no
        // single distinct landing commit to name, so report no target.
        return None;
    }
    squash_target_from_fork(repo, fork, branch_tip, base_tip)
}

/// Shared core of [`is_squash_merged`] / [`squash_merge_target`]: given the fork
/// point, find the single base commit whose diff patch-id equals the branch's
/// cumulative diff. Returns the matching commit's Oid, or `None`.
fn squash_target_from_fork(
    repo: &Repository,
    fork: Oid,
    branch_tip: Oid,
    base_tip: Oid,
) -> Option<Oid> {
    let branch_patch = combined_patch_id(repo, fork, branch_tip)?;

    // Walk the base from its tip back toward (but not past) the fork point,
    // comparing each single-parent commit's patch-id with the branch's.
    let mut walk = repo.revwalk().ok()?;
    walk.push(base_tip).ok()?;
    // Hiding the fork stops the walk from descending into shared history.
    let _ = walk.hide(fork);

    for oid in walk.filter_map(Result::ok).take(SQUASH_SCAN_LIMIT) {
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        // Only an ordinary (single-parent) commit can carry the branch's diff as
        // a squash; merge commits have their own combined diff and are skipped.
        if commit.parent_count() != 1 {
            continue;
        }
        let Ok(parent) = commit.parent(0) else {
            continue;
        };
        let pid = tree_diff_patch_id(repo, parent.tree().ok().as_ref(), commit.tree().ok().as_ref());
        if pid == Some(branch_patch) {
            return Some(oid);
        }
    }
    None
}

/// Patch-id of the branch's cumulative diff from the fork point to its tip.
fn combined_patch_id(repo: &Repository, fork: Oid, branch_tip: Oid) -> Option<Oid> {
    let fork_tree = repo.find_commit(fork).ok()?.tree().ok()?;
    let tip_tree = repo.find_commit(branch_tip).ok()?.tree().ok()?;
    tree_diff_patch_id(repo, Some(&fork_tree), Some(&tip_tree))
}

/// Patch-id of the diff between two trees (`None` on any git error). The patch-id
/// is content-addressed and context-independent, so two diffs that introduce the
/// same changes hash equal regardless of the commits they sit on.
fn tree_diff_patch_id(repo: &Repository, old: Option<&Tree>, new: Option<&Tree>) -> Option<Oid> {
    let diff = repo.diff_tree_to_tree(old, new, None).ok()?;
    diff.patchid(None).ok()
}

/// The trunk branch to measure "merged into" against: a local `main`/`master`
/// first, then their `origin/` remotes, then the checked-out HEAD as a last
/// resort. `None` only when there are no branches at all.
pub fn base_branch(branches: &[BranchInfo]) -> Option<&BranchInfo> {
    for name in ["main", "master"] {
        if let Some(b) = branches.iter().find(|b| !b.is_remote && b.name == name) {
            return Some(b);
        }
    }
    for name in ["origin/main", "origin/master"] {
        if let Some(b) = branches.iter().find(|b| b.is_remote && b.name == name) {
            return Some(b);
        }
    }
    branches.iter().find(|b| b.is_head)
}

/// Whether every change on `branch_tip` is already present in `base_tip` — the
/// branch is not "ahead in content", even if it's ahead by commits. True when the
/// base→branch tree diff contains only deletions (files the base has that the
/// branch lacks, because the base moved on) or is empty. Any Added / Modified /
/// Renamed / Typechange delta means the branch carries work the base lacks, so it
/// is NOT merged.
///
/// This is the local cross-check that guards the GitHub merged-PR signal against
/// **branch-name reuse**: a brand-new `dev`/`wip` branch that happens to share a
/// name with an old merged PR carries novel content, so this returns false and
/// the branch is not misclassified as merged.
pub fn branch_changes_landed(repo: &Repository, branch_tip: Oid, base_tip: Oid) -> bool {
    let (Ok(base_tree), Ok(branch_tree)) = (
        repo.find_commit(base_tip).and_then(|c| c.tree()),
        repo.find_commit(branch_tip).and_then(|c| c.tree()),
    ) else {
        return false;
    };
    let Ok(diff) = repo.diff_tree_to_tree(Some(&base_tree), Some(&branch_tree), None) else {
        return false;
    };
    diff.deltas().all(|d| d.status() == git2::Delta::Deleted)
}

/// The trunk tips to measure "merged" against: the chosen base tip, plus its
/// remote-tracking counterpart (`origin/<base>`) when the base is a *local*
/// trunk and the remote copy is a distinct commit.
///
/// A squash lands on the remote branch first, and keifu auto-fetches, so the
/// local trunk routinely lags `origin/<trunk>` until the user pulls. Measuring
/// only the local tip misses every squash-merge that has been fetched but not
/// pulled, and the GitHub-PR signal doesn't rescue it either (no `gh`, or the
/// content cross-check runs against the same stale local tip) — so NEITHER path
/// fires and the merged branch stays visible. Adding the remote tip closes that
/// gap (issue #82). The evidence tested against it is the same (ancestry / exact
/// patch-id), so this only widens *where* a landing is looked for, not what
/// counts as one — no new false-positive surface.
///
/// Bounded: at most two tips, so per-branch classification cost at most doubles
/// (one extra ancestry query + one extra bounded patch-id scan). Classification
/// already runs off the UI thread.
fn base_tips(branches: &[BranchInfo], base_tip: Oid, base_name: &str) -> Vec<Oid> {
    let mut tips = vec![base_tip];
    // A bare (local) trunk name like "main"/"master" is the only case with a
    // distinct `origin/…` counterpart to add; an already-remote base has none.
    if !base_name.contains('/') {
        let remote = format!("origin/{base_name}");
        if let Some(b) = branches
            .iter()
            .find(|b| b.is_remote && b.name == remote && b.tip_oid != base_tip)
        {
            tips.push(b.tip_oid);
        }
    }
    tips
}

/// Whether a single branch counts as merged into the trunk. Local branches only
/// (a surviving squash-merged ref is local); never the trunk itself or the
/// checked-out HEAD — hiding or dimming the branch you are on is more confusing
/// than useful.
///
/// `base_tips` are the trunk tips to test against (local trunk plus, when it
/// lags, its `origin/…` counterpart — see [`base_tips`]). Merged when **any** of:
///  - ancestry: the branch tip is contained in a base (merge / fast-forward);
///  - squash: the branch's cumulative diff matches a squashed commit on a base;
///  - GitHub says a PR with this head branch merged **and** the branch carries no
///    content some base lacks ([`branch_changes_landed`]). The content cross-check
///    is what makes the (name-based) GitHub signal safe against name reuse.
fn branch_is_merged(
    repo: &Repository,
    b: &BranchInfo,
    base_tips: &[Oid],
    base_name: &str,
    gh_merged: &HashSet<String>,
) -> bool {
    if b.is_remote || b.is_head || b.name == base_name || base_tips.contains(&b.tip_oid) {
        return false;
    }
    let landed_in_git = base_tips
        .iter()
        .any(|&t| is_ancestor_merged(repo, b.tip_oid, t) || is_squash_merged(repo, b.tip_oid, t));
    landed_in_git
        || (gh_merged.contains(&b.name)
            && base_tips
                .iter()
                .any(|&t| branch_changes_landed(repo, b.tip_oid, t)))
}

/// Names of the **local** branches merged into the base branch, combining local
/// git detection (ancestry + squash patch-id) with the GitHub merged-PR signal
/// (cross-checked locally via [`branch_changes_landed`]). See [`branch_is_merged`].
///
/// Intended to run **off the UI thread** (one ancestry query plus a bounded
/// patch-id scan per candidate branch); the result is delivered back and applied
/// by the caller.
pub fn classify_merged_branches(
    repo: &Repository,
    branches: &[BranchInfo],
    base_tip: Oid,
    base_name: &str,
    gh_merged: &HashSet<String>,
) -> HashSet<String> {
    classify_merged_branches_with_targets(repo, branches, base_tip, base_name, gh_merged).0
}

/// Like [`classify_merged_branches`], but additionally reports **which trunk
/// commit** landed each *squash-merged* branch. Returns the merged-branch-name
/// set (identical to [`classify_merged_branches`], unchanged semantics) plus a
/// `branch name → squash commit Oid` map covering only the branches with a
/// concrete squash landing commit (a patch-id match against a base tip).
///
/// Ancestry / fast-forward merges have no single distinct landing commit to
/// name, so they appear in the set but never in the map — the link line (issue
/// #81) is specifically about squashes. When both a local and a remote trunk tip
/// could match, the first base tip that yields a target wins (they carry the same
/// squash, so any is correct).
pub fn classify_merged_branches_with_targets(
    repo: &Repository,
    branches: &[BranchInfo],
    base_tip: Oid,
    base_name: &str,
    gh_merged: &HashSet<String>,
) -> (HashSet<String>, HashMap<String, Oid>) {
    // Measure against the local trunk *and* its remote-tracking tip when the
    // local one lags (the post-fetch, pre-pull state) — see [`base_tips`].
    let tips = base_tips(branches, base_tip, base_name);
    let mut merged = HashSet::new();
    let mut targets = HashMap::new();
    for b in branches {
        if !branch_is_merged(repo, b, &tips, base_name, gh_merged) {
            continue;
        }
        merged.insert(b.name.clone());
        // Record the squash landing commit when one is nameable. Purely additive
        // over the yes/no classification above — a branch with no squash target
        // (ancestry merge, or a GitHub-only content match) stays out of the map.
        if let Some(target) = tips
            .iter()
            .find_map(|&t| squash_merge_target(repo, b.tip_oid, t))
        {
            targets.insert(b.name.clone(), target);
        }
    }
    (merged, targets)
}

/// Local-only classification (no GitHub signal) — ancestry + squash detection.
/// Kept for the non-GitHub path and for direct testing.
pub fn merged_local_branches(
    repo: &Repository,
    branches: &[BranchInfo],
    base_tip: Oid,
    base_name: &str,
) -> HashSet<String> {
    classify_merged_branches(repo, branches, base_tip, base_name, &HashSet::new())
}

/// Upper bound on PR-branch commits scanned per PR when hunting base-update
/// merges, so an enormous branch can't stall a refresh. A PR branch is short in
/// practice; this window is generous.
const BASE_UPDATE_SCAN_LIMIT: usize = 400;

/// Classify **base-update ("back-merge") commits**: merge commits that sit on an
/// open PR's branch — reachable from the PR head but *not yet on the base* — and
/// whose **second parent** is on the base branch. That is exactly the shape of
/// "the updated base branch was merged INTO the PR branch to refresh it": the
/// first parent stays on the PR branch, the second reaches into the base. Such a
/// merge is graph noise (issue #55), so the renderer mutes it when the option is
/// on.
///
/// The opposite direction — a **PR landing** — is deliberately *not* matched: a
/// landing merge sits ON the base (its first parent is the base, its second is
/// the PR head), so it is contained in `base_tip` and the "not yet on the base"
/// filter (`revwalk.hide(base_tip)`) drops it before the second-parent test ever
/// runs. Direction is thus decided structurally, not by message text.
///
/// Pure over a `git2::Repository`; `pr_heads` are the open PRs' head-commit OIDs
/// (from `PrInfo::head_oid`). Cheap: for each PR it walks only that branch's own
/// commits (the revwalk hides `base_tip`), bounded by [`BASE_UPDATE_SCAN_LIMIT`],
/// with one ancestry query per merge encountered.
pub fn classify_base_update_merges(
    repo: &Repository,
    pr_heads: &[Oid],
    base_tip: Oid,
) -> HashSet<Oid> {
    let mut out = HashSet::new();
    for &head in pr_heads {
        // Commits reachable from the PR head but NOT already on the base: the
        // PR branch's own commits. A landing merge is on the base, so hiding
        // `base_tip` removes it here — only still-ahead back-merges survive.
        let Ok(mut walk) = repo.revwalk() else {
            continue;
        };
        if walk.push(head).is_err() {
            continue;
        }
        let _ = walk.hide(base_tip);
        for oid in walk.filter_map(Result::ok).take(BASE_UPDATE_SCAN_LIMIT) {
            let Ok(commit) = repo.find_commit(oid) else {
                continue;
            };
            // Only a real merge (2+ parents) can be a back-merge.
            if commit.parent_count() < 2 {
                continue;
            }
            let Ok(second) = commit.parent_id(1) else {
                continue;
            };
            // Second parent already on the base ⇒ the base was merged in.
            if second == base_tip
                || repo.graph_descendant_of(base_tip, second).unwrap_or(false)
            {
                out.insert(oid);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Signature, Time};

    /// Commit a tree containing exactly `files` (path, content) onto `refname`
    /// with the given parents. Building the whole tree each time (rather than
    /// mutating an index) keeps the object graph explicit, so tree-to-tree diffs
    /// — and therefore patch-ids — are exactly what the test intends.
    fn commit(
        repo: &Repository,
        refname: &str,
        secs: i64,
        parents: &[Oid],
        files: &[(&str, &str)],
    ) -> Oid {
        let mut tb = repo.treebuilder(None).unwrap();
        for (path, content) in files {
            let blob = repo.blob(content.as_bytes()).unwrap();
            tb.insert(path, blob, 0o100644).unwrap();
        }
        let tree = repo.find_tree(tb.write().unwrap()).unwrap();
        let sig = Signature::new("Dev", "dev@example.com", &Time::new(secs, 0)).unwrap();
        let parent_commits: Vec<_> = parents.iter().map(|p| repo.find_commit(*p).unwrap()).collect();
        let parent_refs: Vec<&git2::Commit> = parent_commits.iter().collect();
        repo.commit(Some(refname), &sig, &sig, "msg", &tree, &parent_refs)
            .unwrap()
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

    #[test]
    fn ancestor_branch_is_merged() {
        // main: a <- b <- c ; topic points at b, fully contained in main.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("b.txt", "b")]);
        let c = commit(&repo, "refs/heads/main", 3000, &[b], &[("base.txt", "base"), ("b.txt", "b"), ("c.txt", "c")]);
        assert!(is_ancestor_merged(&repo, b, c));
        assert!(is_merged_into(&repo, b, c));
    }

    #[test]
    fn merge_commit_merge_is_detected() {
        // main: a <- b ; topic: a <- t ; merge topic into main -> m (2 parents).
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "main")]);
        let t = commit(&repo, "refs/heads/topic", 2500, &[a], &[("base.txt", "base"), ("feat.txt", "feat")]);
        // Merge commit carrying both files; parents are main tip and topic tip.
        let m = commit(
            &repo,
            "refs/heads/main",
            3000,
            &[b, t],
            &[("base.txt", "base"), ("main.txt", "main"), ("feat.txt", "feat")],
        );
        assert!(is_merged_into(&repo, t, m), "topic reachable from the merge commit");
    }

    #[test]
    fn squash_merged_branch_is_detected() {
        // main:    a <- b            (b adds main.txt)
        // feature: a <- f1 <- f2     (f1 adds feat.txt=one, f2 -> two)
        // squash:  b <- s            (s adds feat.txt=two, main's copy of feature)
        // feature's own ref survives; the remote copy is "deleted" (never reffed).
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "main")]);
        let f1 = commit(&repo, "refs/heads/feature", 2100, &[a], &[("base.txt", "base"), ("feat.txt", "one")]);
        let f2 = commit(&repo, "refs/heads/feature", 2200, &[f1], &[("base.txt", "base"), ("feat.txt", "two")]);
        // The squash commit introduces the feature's *net* change (add feat.txt
        // = two) on top of main — same hunk as feature's cumulative diff.
        let s = commit(
            &repo,
            "refs/heads/main",
            3000,
            &[b],
            &[("base.txt", "base"), ("main.txt", "main"), ("feat.txt", "two")],
        );

        assert!(!is_ancestor_merged(&repo, f2, s), "no commit is shared after a squash");
        assert!(is_squash_merged(&repo, f2, s), "patch-id matches the squashed commit");
        assert!(is_merged_into(&repo, f2, s));
    }

    #[test]
    fn unmerged_branch_is_not_merged() {
        // feature changes a different file that never landed on main.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "main")]);
        let g = commit(&repo, "refs/heads/feature", 2100, &[a], &[("base.txt", "base"), ("other.txt", "x")]);
        assert!(!is_ancestor_merged(&repo, g, b));
        assert!(!is_squash_merged(&repo, g, b));
        assert!(!is_merged_into(&repo, g, b));
    }

    #[test]
    fn unrelated_history_is_not_merged() {
        // Two roots with no common ancestor: merge_base fails → not merged.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let z = commit(&repo, "refs/heads/orphan", 1000, &[], &[("z.txt", "z")]);
        assert!(!is_merged_into(&repo, z, a));
    }

    #[test]
    fn base_branch_prefers_local_main_then_master_then_head() {
        let bs = vec![
            local("feature", Oid::zero(), true),
            local("master", Oid::zero(), false),
            local("main", Oid::zero(), false),
        ];
        assert_eq!(base_branch(&bs).map(|b| b.name.as_str()), Some("main"));

        let bs = vec![local("feature", Oid::zero(), true), local("master", Oid::zero(), false)];
        assert_eq!(base_branch(&bs).map(|b| b.name.as_str()), Some("master"));

        let bs = vec![local("feature", Oid::zero(), true), local("dev", Oid::zero(), false)];
        assert_eq!(base_branch(&bs).map(|b| b.name.as_str()), Some("feature"));

        assert!(base_branch(&[]).is_none());
    }

    #[test]
    fn merged_local_branches_classifies_the_set() {
        // main advances to s (squash of feature). topic is a plain ancestor.
        // gone is unmerged. HEAD (main) and the base are never classified.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "main")]);
        repo.reference("refs/heads/topic", b, true, "topic").unwrap();
        let f1 = commit(&repo, "refs/heads/feature", 2100, &[a], &[("base.txt", "base"), ("feat.txt", "one")]);
        let f2 = commit(&repo, "refs/heads/feature", 2200, &[f1], &[("base.txt", "base"), ("feat.txt", "two")]);
        let g = commit(&repo, "refs/heads/gone", 2300, &[a], &[("base.txt", "base"), ("other.txt", "x")]);
        let s = commit(
            &repo,
            "refs/heads/main",
            3000,
            &[b],
            &[("base.txt", "base"), ("main.txt", "main"), ("feat.txt", "two")],
        );

        let branches = vec![
            local("main", s, true), // HEAD + base
            local("topic", b, false),
            local("feature", f2, false),
            local("gone", g, false),
        ];
        let merged = merged_local_branches(&repo, &branches, s, "main");
        assert!(merged.contains("topic"), "ancestor merge");
        assert!(merged.contains("feature"), "squash merge");
        assert!(!merged.contains("gone"), "never merged");
        assert!(!merged.contains("main"), "base/HEAD is never classified");
        assert_eq!(merged.len(), 2);

        // The targets variant reports the same set PLUS the squash landing
        // commit for the squash-merged branch only.
        let (merged2, targets) =
            classify_merged_branches_with_targets(&repo, &branches, s, "main", &HashSet::new());
        assert_eq!(merged2, merged, "target variant's set matches the plain one");
        assert_eq!(
            targets.get("feature"),
            Some(&s),
            "feature squash-links to the squash commit s"
        );
        assert!(
            !targets.contains_key("topic"),
            "an ancestry merge has no single squash landing commit"
        );
        assert!(!targets.contains_key("gone"), "unmerged branch has no target");
    }

    #[test]
    fn squash_merge_target_names_the_commit_or_none() {
        // main:    a <- b <- s   (s squashes feature onto main)
        // feature: a <- f1 <- f2
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "main")]);
        let f1 = commit(&repo, "refs/heads/feature", 2100, &[a], &[("base.txt", "base"), ("feat.txt", "one")]);
        let f2 = commit(&repo, "refs/heads/feature", 2200, &[f1], &[("base.txt", "base"), ("feat.txt", "two")]);
        let s = commit(
            &repo,
            "refs/heads/main",
            3000,
            &[b],
            &[("base.txt", "base"), ("main.txt", "main"), ("feat.txt", "two")],
        );
        assert_eq!(
            squash_merge_target(&repo, f2, s),
            Some(s),
            "the squash commit is named"
        );
        // A branch that never landed has no target.
        let g = commit(&repo, "refs/heads/gone", 2300, &[a], &[("base.txt", "base"), ("other.txt", "x")]);
        assert_eq!(squash_merge_target(&repo, g, s), None, "unmerged → no target");
        // An ancestry (fully-contained) branch is not a squash → no target.
        assert_eq!(squash_merge_target(&repo, b, s), None, "ancestry → no target");
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
    fn squash_landed_on_remote_trunk_is_classified_when_local_trunk_lags() {
        // Issue #82. A feature is squash-merged onto `origin/main`; keifu has
        // fetched (so origin/main carries the squash) but the user hasn't pulled,
        // so local `main` still sits at the fork point. Classification must still
        // hide the feature — measured against the ahead remote tip, not just the
        // stale local one.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        // Fork point; local main stays here (stale).
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        // Feature: two commits building feat.txt (multi-commit, so only the
        // *cumulative* diff equals the squash — the case #82 is about).
        let f1 = commit(&repo, "refs/heads/feature", 2100, &[a], &[("base.txt", "base"), ("feat.txt", "one")]);
        let f2 = commit(&repo, "refs/heads/feature", 2200, &[f1], &[("base.txt", "base"), ("feat.txt", "one\ntwo")]);
        // Squash lands the feature's net diff on origin/main (single commit).
        let s = commit(&repo, "refs/remotes/origin/main", 3000, &[a], &[("base.txt", "base"), ("feat.txt", "one\ntwo")]);
        // An unrelated branch that genuinely never landed.
        let g = commit(&repo, "refs/heads/gone", 2300, &[a], &[("base.txt", "base"), ("other.txt", "x")]);

        let branches = vec![
            local("main", a, true), // stale local trunk, also HEAD
            remote("origin/main", s),
            local("feature", f2, false),
            local("gone", g, false),
        ];
        // Sanity: against the stale local tip alone, the squash is invisible.
        assert!(!is_squash_merged(&repo, f2, a), "stale local tip cannot see the squash");
        assert!(is_squash_merged(&repo, f2, s), "remote tip carries the squash");

        // base_branch prefers the (stale) local main; classification must reach
        // through to origin/main anyway.
        let base = base_branch(&branches).unwrap();
        assert_eq!(base.name, "main");
        let merged = merged_local_branches(&repo, &branches, base.tip_oid, &base.name);
        assert!(merged.contains("feature"), "squash on ahead remote trunk must be classified");
        assert!(!merged.contains("gone"), "genuinely unmerged branch stays visible");
        assert!(!merged.contains("main"), "the trunk itself is never classified");
        assert!(!merged.contains("origin/main"), "remote branches are never classified");
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn ancestor_merge_on_remote_trunk_is_classified_when_local_trunk_lags() {
        // The same remote-tip reach must also cover a plain (non-squash) merge
        // that has been fetched into origin/main but not pulled to local main.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let t = commit(&repo, "refs/heads/topic", 2000, &[a], &[("base.txt", "base"), ("t.txt", "t")]);
        // origin/main advanced to include topic and one further commit; local
        // main lags at `a`. topic is a strict ancestor of origin/main's tip.
        let u = commit(&repo, "refs/remotes/origin/main", 2500, &[t], &[("base.txt", "base"), ("t.txt", "t"), ("u.txt", "u")]);
        let branches = vec![local("main", a, true), remote("origin/main", u), local("topic", t, false)];
        let base = base_branch(&branches).unwrap();
        let merged = merged_local_branches(&repo, &branches, base.tip_oid, &base.name);
        assert!(merged.contains("topic"), "ancestor merge visible only on remote trunk must classify");
    }

    #[test]
    fn base_tips_omits_matching_or_absent_remote_trunk() {
        // When local main is up to date (origin/main equal), the remote adds no
        // second tip — behaviour is identical to before.
        let z = Oid::zero();
        let branches = vec![local("main", z, true), remote("origin/main", z)];
        assert_eq!(base_tips(&branches, z, "main"), vec![z], "equal remote adds no tip");
        // A remote base name has no bare `origin/…` counterpart to add.
        assert_eq!(base_tips(&branches, z, "origin/main"), vec![z]);
        // No remote trunk present at all → just the local tip.
        assert_eq!(base_tips(&[local("main", z, true)], z, "main"), vec![z]);
    }

    fn gh(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn gh_signal_does_not_classify_a_name_reused_branch_with_novel_commits() {
        // Regression: an old PR whose head branch was "feature" merged (GitHub
        // reports "feature" as a merged head ref). A NEW, unmerged branch reuses
        // that name with novel work. It must NOT be dimmed/hidden just because the
        // name matches — its content is not in the base.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "main")]);
        // New "feature" adds a file the base has never seen.
        let f = commit(&repo, "refs/heads/feature", 2100, &[a], &[("base.txt", "base"), ("novel.txt", "new work")]);

        let branches = vec![local("main", b, true), local("feature", f, false)];
        // GitHub claims a merged PR with head "feature".
        let merged = classify_merged_branches(&repo, &branches, b, "main", &gh(&["feature"]));
        assert!(
            !merged.contains("feature"),
            "name reuse with novel content must not be classified merged"
        );
        assert!(!branch_changes_landed(&repo, f, b), "branch carries content the base lacks");
    }

    #[test]
    fn gh_signal_classifies_a_landed_branch_that_patch_id_alone_would_miss() {
        // A branch whose changes all landed on the base (content ⊆ base), but via
        // two separate commits rather than one squash — so no single base commit's
        // patch-id matches the branch's cumulative diff, and it isn't an ancestor.
        // The GitHub signal + the content cross-check catch it.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        // Branch adds x.txt and y.txt across two commits.
        let f1 = commit(&repo, "refs/heads/feature", 1100, &[a], &[("base.txt", "base"), ("x.txt", "x")]);
        let f2 = commit(&repo, "refs/heads/feature", 1200, &[f1], &[("base.txt", "base"), ("x.txt", "x"), ("y.txt", "y")]);
        // Base lands the same content, but as two separate commits (no single
        // squash commit equals the branch's combined diff).
        let b1 = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("x.txt", "x")]);
        let b2 = commit(&repo, "refs/heads/main", 2100, &[b1], &[("base.txt", "base"), ("x.txt", "x"), ("y.txt", "y")]);

        assert!(!is_ancestor_merged(&repo, f2, b2));
        assert!(!is_squash_merged(&repo, f2, b2), "no single squash commit matches");
        assert!(branch_changes_landed(&repo, f2, b2), "all content is present in the base");

        let branches = vec![local("main", b2, true), local("feature", f2, false)];
        // Without the GitHub signal it stays unclassified (content match alone is
        // not trusted); with it, it's merged.
        assert!(!classify_merged_branches(&repo, &branches, b2, "main", &HashSet::new()).contains("feature"));
        assert!(classify_merged_branches(&repo, &branches, b2, "main", &gh(&["feature"])).contains("feature"));
    }

    #[test]
    fn net_empty_branch_against_allow_empty_trunk_commit_counts_as_landed() {
        // Edge case: a branch that introduces no content change (its tree equals
        // the fork point) and a base that advanced with an empty commit. The
        // branch has nothing to land, so content is trivially present in the base.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        // Branch commit with the SAME tree as `a` (net-empty change).
        let f = commit(&repo, "refs/heads/feature", 1100, &[a], &[("base.txt", "base")]);
        // Base advances with an empty commit (same tree again).
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base")]);

        assert!(branch_changes_landed(&repo, f, b), "net-empty branch has nothing to land");
        let branches = vec![local("main", b, true), local("feature", f, false)];
        assert!(classify_merged_branches(&repo, &branches, b, "main", &gh(&["feature"])).contains("feature"));
    }

    // ── base-update ("back-merge") classification (#55) ──────────────

    #[test]
    fn back_merge_of_updated_base_into_pr_branch_is_detected() {
        // main:    a <- c            (base advances to c)
        // feature: a <- f            (PR branch)
        // back-merge: merge c INTO feature => m, on the feature branch.
        //   m's parents: [f (feature, first), c (base, second)].
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let c = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base2")]);
        let f = commit(&repo, "refs/heads/feature", 1500, &[a], &[("base.txt", "base"), ("feat.txt", "x")]);
        // Merge the updated base (c) into the PR branch; first parent = feature.
        let m = commit(
            &repo,
            "refs/heads/feature",
            3000,
            &[f, c],
            &[("base.txt", "base2"), ("feat.txt", "x")],
        );
        let set = classify_base_update_merges(&repo, &[m], c);
        assert!(set.contains(&m), "the back-merge commit is classified");
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn pr_landing_merge_is_not_classified_as_back_merge() {
        // The opposite direction: feature lands ON main.
        // main:    a <- b
        // feature: a <- t
        // landing: merge t INTO main => m, ON main. Parents [b (main), t (feat)].
        // The PR head is `t`; `m` sits on the base, so it must NOT be muted.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "m")]);
        let t = commit(&repo, "refs/heads/feature", 1500, &[a], &[("base.txt", "base"), ("feat.txt", "x")]);
        let m = commit(
            &repo,
            "refs/heads/main",
            3000,
            &[b, t],
            &[("base.txt", "base"), ("main.txt", "m"), ("feat.txt", "x")],
        );
        // Base tip is the landing merge itself.
        let set = classify_base_update_merges(&repo, &[t], m);
        assert!(set.is_empty(), "a PR-landing merge is never a back-merge");
    }

    #[test]
    fn merge_of_a_sibling_feature_not_on_base_is_not_classified() {
        // A merge on the PR branch that pulls in ANOTHER unmerged feature (not
        // the base) must not be muted — its second parent is not on the base.
        // main:    a <- c
        // feat:    a <- f
        // sibling: a <- s   (never merged to main)
        // merge s INTO feat => m. Parents [f, s]; s is not on base.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let c = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base2")]);
        let f = commit(&repo, "refs/heads/feat", 1500, &[a], &[("base.txt", "base"), ("f.txt", "f")]);
        let s = commit(&repo, "refs/heads/sibling", 1600, &[a], &[("base.txt", "base"), ("s.txt", "s")]);
        let m = commit(
            &repo,
            "refs/heads/feat",
            3000,
            &[f, s],
            &[("base.txt", "base"), ("f.txt", "f"), ("s.txt", "s")],
        );
        let set = classify_base_update_merges(&repo, &[m], c);
        assert!(set.is_empty(), "merging a non-base sibling is not a base-update");
    }

    #[test]
    fn no_pr_heads_or_no_merges_yields_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let f = commit(&repo, "refs/heads/feature", 1500, &[a], &[("base.txt", "base"), ("x.txt", "x")]);
        // No PR heads at all.
        assert!(classify_base_update_merges(&repo, &[], a).is_empty());
        // A PR head whose branch has no merge commit.
        assert!(classify_base_update_merges(&repo, &[f], a).is_empty());
    }

    #[test]
    fn back_merge_detected_when_head_is_above_the_merge() {
        // The PR head need not BE the back-merge; the merge can sit mid-branch.
        // feature: a <- f <- m <- g   where m merges base c into feature.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let c = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base2")]);
        let f = commit(&repo, "refs/heads/feature", 1500, &[a], &[("base.txt", "base"), ("feat.txt", "x")]);
        let m = commit(
            &repo,
            "refs/heads/feature",
            3000,
            &[f, c],
            &[("base.txt", "base2"), ("feat.txt", "x")],
        );
        let g = commit(&repo, "refs/heads/feature", 3500, &[m], &[("base.txt", "base2"), ("feat.txt", "y")]);
        let set = classify_base_update_merges(&repo, &[g], c);
        assert!(set.contains(&m), "a mid-branch back-merge is still found");
    }
}
