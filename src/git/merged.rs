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
//! Classification is **transitive**: a branch merged into an already-merged
//! branch (a stacked PR, a sub-branch folded into its feature before the
//! feature landed) is itself merged — its work reached the trunk through the
//! chain even when no direct trunk signal exists. The classifier iterates to a
//! fixed point, treating each classified branch's tip as an additional landed
//! tip (see [`classify_merged_branches_with_targets`]).
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
///
/// A patch-id match is **cross-checked** against [`branch_content_in_base`] (a
/// three-way merge) before it is trusted. The zero-context patch-id (see
/// [`tree_diff_patch_id`]) keys only on the changed lines, so a *trivial* branch —
/// one that adds or removes the same line(s) an unrelated trunk commit also
/// touched elsewhere in the same file — can collide on patch-id alone. The
/// three-way merge is exact: it returns the base tree only when the branch's work
/// really is contained, so a collision (whose merge would place the change in two
/// places, or delete it from one the base kept) is rejected. Together they name a
/// real squash without ever hiding genuinely unlanded work (issue #97).
///
/// The containment check runs against the **matched commit** (the landing
/// point), not the base tip: after a genuine squash, later trunk commits are
/// free to edit the very lines the branch introduced, and by the tip those
/// edits would three-way-conflict with the branch and wrongly un-classify a
/// branch that really did land. At the squash commit itself the branch's work
/// is contained by construction, while a collision's differing placement still
/// fails there.
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
        // A pid hit is a candidate, not proof (zero-context ids can collide) —
        // confirm the branch's work is exactly contained at this landing point,
        // and keep scanning past a collision in case the real squash sits
        // deeper in the walk.
        if pid == Some(branch_patch) && branch_content_in_base(repo, branch_tip, oid) {
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
/// hashes the diff's added/removed lines, so two diffs that introduce the same
/// changes hash equal regardless of the commits they sit on.
///
/// The diff is generated with **zero context lines** on purpose. A patch-id
/// normally folds the surrounding context lines into its hash, so a trunk commit
/// that edits lines *near* (within the default 3-line window of) a branch's own
/// change shifts the context and breaks the match — even though the squashed diff
/// adds exactly the same lines. Dropping context makes the id depend only on the
/// lines the change actually touches, which is what a squash preserves across an
/// advancing base (issue #97).
///
/// Trade-off: without context, two *different* changes that touch the same file
/// with the same added/removed line set (e.g. inserting an identical line at a
/// different place) now hash equal. So a zero-context patch-id match is treated as
/// a *candidate*, not proof — [`squash_target_from_fork`] confirms it with an
/// exact three-way containment check before trusting it, so a collision can never
/// hide unlanded work.
fn tree_diff_patch_id(repo: &Repository, old: Option<&Tree>, new: Option<&Tree>) -> Option<Oid> {
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(0);
    let diff = repo.diff_tree_to_tree(old, new, Some(&mut opts)).ok()?;
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

/// Whether every change `branch_tip` introduced since it forked from the trunk is
/// already present in `base_tip` — the branch is not "ahead in content", even if
/// it's ahead by commits.
///
/// A **three-way merge** with the fork point as the common ancestor: if merging
/// the branch into the base changes nothing (the merged tree equals the base
/// tree), the base already carries all of the branch's work, so the branch has
/// landed — by a squash, a rebase, or the same change re-applied. Anchoring the
/// comparison to the fork ancestor is what makes it survive an **advancing base**:
/// a trunk commit that edits the same files (even the same regions) after the fork
/// does not count against the branch, because the merge attributes those edits to
/// the base side, not the branch. A raw `base→branch` tree diff can't tell "the
/// branch is behind on this file" from "the branch changed this file", and so
/// wrongly reads a behind-file as novel work (issue #97).
///
/// Precise, not fuzzy: a branch carrying any change the base lacks yields a merged
/// tree that differs from the base — or a conflict — so it is NOT reported merged,
/// and real unlanded work is never hidden. This is the local cross-check that
/// guards the GitHub merged-PR signal against **branch-name reuse**: a brand-new
/// branch reusing an old merged PR's name carries novel content, so it returns
/// false. Returns `false` on unrelated histories or any git error.
pub fn branch_content_in_base(repo: &Repository, branch_tip: Oid, base_tip: Oid) -> bool {
    let Ok(fork) = repo.merge_base(branch_tip, base_tip) else {
        return false; // unrelated histories → nothing was merged
    };
    let (Ok(ancestor_tree), Ok(base_tree), Ok(branch_tree)) = (
        repo.find_commit(fork).and_then(|c| c.tree()),
        repo.find_commit(base_tip).and_then(|c| c.tree()),
        repo.find_commit(branch_tip).and_then(|c| c.tree()),
    ) else {
        return false;
    };
    let Ok(mut index) = repo.merge_trees(&ancestor_tree, &base_tree, &branch_tree, None) else {
        return false;
    };
    // A conflict means the branch diverges from the base (e.g. both changed the
    // same lines differently — a conflict resolved on the way in); it is not
    // cleanly contained, so leave it to the GitHub-PR signal.
    if index.has_conflicts() {
        return false;
    }
    // Merged tree equal to the base tree ⇒ the branch added nothing the base
    // lacks. `write_tree_to` reuses the existing base tree object when the merge
    // is a no-op, so this writes no new object in the "contained" case.
    index
        .write_tree_to(repo)
        .map(|merged| merged == base_tree.id())
        .unwrap_or(false)
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
    // The checked-out branch is a trunk too (#100): on a repo whose working
    // trunk is a long-lived non-main branch (keifu's own chong-dev), branches
    // land there and would otherwise never classify — the selected main/master
    // base has no knowledge of them. "Merged" therefore means "landed in the
    // trunk OR in the line you are on". The HEAD branch itself is protected
    // from classification by `branch_is_merged`'s `is_head` guard, and the
    // primary trunk by the `base_name`/tip guards, so adding this tip cannot
    // hide either trunk.
    if let Some(head) = branches.iter().find(|b| b.is_head) {
        if !tips.contains(&head.tip_oid) {
            tips.push(head.tip_oid);
        }
        // …and so is the working trunk's REMOTE counterpart (#109): a
        // squash-merge PR against the working trunk lands on origin/<head>
        // first, and the local head lags until the next pull — the post-merge,
        // pre-pull state in which the landed branch must already classify.
        // Prefer the configured upstream name; fall back to the origin/
        // convention (mirroring the origin/<base> reach above). Being a trunk
        // tip also protects the counterpart itself from classification.
        let upstream = head
            .upstream
            .clone()
            .unwrap_or_else(|| format!("origin/{}", head.name));
        if let Some(r) = branches
            .iter()
            .find(|b| b.is_remote && b.name == upstream && !tips.contains(&b.tip_oid))
        {
            tips.push(r.tip_oid);
        }
    }
    tips
}

/// Upper bound on commits collected per trunk tip when building the trunk
/// first-parent line (see [`trunk_first_parent_line`]). Cheap to walk — parent
/// lookups only, no diffs — so the window is generous; a branch parked deeper
/// than this behind a trunk tip falls back to the old ancestry reading.
const TRUNK_LINE_SCAN_LIMIT: usize = 2000;

/// The commits that ARE the trunk lines: the first-parent chain of every trunk
/// tip, bounded by [`TRUNK_LINE_SCAN_LIMIT`] per tip. A branch whose tip is in
/// this set points at an *older state of a line already in view* — it is
/// **behind**, not merged (#112). Raw ancestry cannot tell "landed and the line
/// moved on" apart from "never went anywhere yet", but the first-parent chain
/// can: a genuinely merged branch's tip hangs OFF the line — reachable only
/// through a merge commit's second parent, or (squash) not reachable at all —
/// never on the line itself.
fn trunk_first_parent_line(repo: &Repository, tips: &[Oid]) -> HashSet<Oid> {
    let mut line = HashSet::new();
    for &tip in tips {
        let mut cur = tip;
        for _ in 0..TRUNK_LINE_SCAN_LIMIT {
            if !line.insert(cur) {
                break; // chains converge; the rest is already collected
            }
            match repo.find_commit(cur).ok().and_then(|c| c.parent_id(0).ok()) {
                Some(parent) => cur = parent,
                None => break, // root
            }
        }
    }
    line
}

/// The head-branch name to test against the GitHub merged-PR set. GitHub reports
/// a PR's `headRefName` *without* any remote prefix ("feature", "fix/x"), while a
/// remote-tracking ref is named "origin/feature". Since a remote name never
/// contains a '/', the segment before the first '/' of a remote ref is exactly
/// the remote name — stripping it yields the head ref GitHub would report. Local
/// refs are passed through unchanged.
fn gh_key(b: &BranchInfo) -> &str {
    if b.is_remote {
        b.name
            .split_once('/')
            .map(|(_, rest)| rest)
            .unwrap_or(&b.name)
    } else {
        &b.name
    }
}

/// Whether a single branch counts as merged into the trunk. Covers **both local
/// and remote** branches: after a GitHub squash-merge the surviving ref is often
/// the *remote* one (`origin/feature`) — the PR branch was kept on the remote, or
/// only ever fetched, never checked out locally — so classifying local refs alone
/// left those visible and un-linked (issue #100). Never the trunk itself, the
/// checked-out HEAD, or any tip that *is* a base tip (which excludes the local
/// trunk and its `origin/…` counterpart, added to `base_tips` by [`base_tips`]) —
/// hiding or dimming the trunk or the branch you are on is more confusing than
/// useful. Nor any tip that sits ON a trunk line's first-parent chain: that is a
/// branch which is merely *behind*, not merged (#112, [`trunk_first_parent_line`]).
///
/// `base_tips` are the tips to test against: the trunk tips (local trunk plus,
/// when it lags, its `origin/…` counterpart — see [`base_tips`]) and, in the
/// later rounds of [`classify_merged_branches_with_targets`]'s fixed point, the
/// tips of branches already classified merged (transitive merges: landing in a
/// landed branch is landing). Merged when **any** of:
///  - ancestry: the branch tip is contained in a base (merge / fast-forward);
///  - squash: the branch's cumulative diff matches a squashed commit on a base;
///  - GitHub says a PR with this head branch merged **and** the branch carries no
///    content some base lacks ([`branch_content_in_base`]). The content cross-check
///    is what makes the (name-based) GitHub signal safe against name reuse. A
///    remote ref is matched against GitHub by its prefix-stripped name (see
///    [`gh_key`]).
fn branch_is_merged(
    repo: &Repository,
    b: &BranchInfo,
    branches: &[BranchInfo],
    base_tips: &[Oid],
    trunk_line: &HashSet<Oid>,
    base_name: &str,
    gh_merged: &HashSet<String>,
) -> bool {
    // The trunk's own remote counterpart (`origin/<base>`) is caught here: it is
    // always a base tip when distinct (see [`base_tips`]), so `base_tips` contains
    // its OID. The HEAD guard only applies to the local checked-out branch.
    if b.is_head || b.name == base_name || base_tips.contains(&b.tip_oid) {
        return false;
    }
    // A tip ON a trunk line's own first-parent history is a branch that is
    // simply BEHIND — an older state of a line already in view, not landed
    // work (#112, see [`trunk_first_parent_line`]). Every signal is refused,
    // gh included: for an on-line tip the containment cross-check is trivially
    // true, so the PR name alone would decide, and "behind" must never read as
    // "merged".
    if trunk_line.contains(&b.tip_oid) {
        return false;
    }
    // A remote *mirror of the trunk on any other remote* (`upstream/main`,
    // `fork/master`, …) is itself a trunk, not a landed feature branch, and must
    // never be classified — otherwise a mirror that merely lags the base reads as
    // an ancestry "merge" and gets hidden/dimmed. `base_tips` only exempts the
    // `origin/` mirror (by tip), so guard the rest here by trunk short-name: a
    // remote ref whose branch part equals the trunk's is a trunk copy (#100
    // review). `base_short` strips a leading remote segment from the base name
    // (bare `main` → `main`, `origin/main` → `main`); trunk names carry no '/'.
    let base_short = base_name.split_once('/').map_or(base_name, |(_, rest)| rest);
    if b.is_remote && gh_key(b) == base_short {
        return false;
    }
    // Trunk-by-convention names are trunks even when NOT the selected base: in
    // a repo with both `main` and `master`, the non-selected one lagging behind
    // the checked-out line must not read as "merged" — same reasoning as the
    // base-name guard, extended to every name `base_branch` itself treats as a
    // trunk candidate.
    if !b.is_remote && (b.name == "main" || b.name == "master") {
        return false;
    }
    // A branch that merely LAGS the live copy of its own line is stale, not
    // landed work, and its tip being an ancestor of things makes the *ancestry*
    // signal lie (#105). Two symmetric shapes:
    //  - local strictly behind its upstream (dev lagging origin/dev) — the
    //    remote is the live line; `behind` is 0 when there is no upstream;
    //  - remote strictly behind its LOCAL counterpart (origin/chong-dev lagging
    //    a checked-out chong-dev with unpushed commits) — the local is the live
    //    line, and since the checked-out tip is a trunk tip (#103) the mirror
    //    would otherwise read as "merged into the line you're on" (#107).
    // The exact signals (patch-id squash target, gh + containment) stay
    // eligible — they only fire when the branch's content genuinely landed.
    let stale_tracking = if b.is_remote {
        branches.iter().any(|l| {
            !l.is_remote
                && l.name == gh_key(b)
                && l.tip_oid != b.tip_oid
                && repo.graph_descendant_of(l.tip_oid, b.tip_oid).unwrap_or(false)
        })
    } else {
        b.ahead == 0 && b.behind > 0
    };
    let landed_in_git = base_tips.iter().any(|&t| {
        if stale_tracking {
            // Only a concrete squash landing commit counts — both the ancestry
            // signal and `is_squash_merged`'s fully-contained shortcut are
            // exactly the "tip is an ancestor" reading that staleness fakes.
            squash_merge_target(repo, b.tip_oid, t).is_some()
        } else {
            is_ancestor_merged(repo, b.tip_oid, t) || is_squash_merged(repo, b.tip_oid, t)
        }
    });
    landed_in_git
        || (gh_merged.contains(gh_key(b))
            && base_tips
                .iter()
                .any(|&t| branch_content_in_base(repo, b.tip_oid, t)))
}

/// Plain-text trace of every decision [`classify_merged_branches`] makes, for
/// `keifu --explain-merged` (#100 diagnostics): base selection, the trunk tips
/// tested, and — per branch — each guard and signal with the OIDs involved, so
/// a "branch should be hidden but isn't" report can name the exact gate that
/// failed on the user's real repository instead of guessing from fixtures.
/// Purely observational: mirrors [`branch_is_merged`] without changing it.
pub fn explain_classification(
    repo: &Repository,
    branches: &[BranchInfo],
    gh_merged: &HashSet<String>,
) -> String {
    use std::fmt::Write as _;
    let short = |o: Oid| o.to_string()[..7].to_string();
    let mut out = String::new();

    let Some(base) = base_branch(branches) else {
        return "no base branch (no local/origin main or master, no HEAD) — nothing can be classified\n".into();
    };
    let tips = base_tips(branches, base.tip_oid, &base.name);
    let trunk_line = trunk_first_parent_line(repo, &tips);
    let _ = writeln!(out, "base: {} @ {}", base.name, short(base.tip_oid));
    let _ = writeln!(
        out,
        "trunk tips tested: {}",
        tips.iter().map(|&t| short(t)).collect::<Vec<_>>().join(", ")
    );
    let _ = writeln!(
        out,
        "trunk first-parent line: {} commits collected (cap {TRUNK_LINE_SCAN_LIMIT}/tip)",
        trunk_line.len()
    );
    let _ = writeln!(out, "gh merged-PR heads known: {}", gh_merged.len());

    let base_short = base.name.split_once('/').map_or(base.name.as_str(), |(_, rest)| rest);
    // Branches the direct (trunk-tips-only) pass below classifies, so the
    // transitive section at the end can report exactly the ones only the fixed
    // point catches.
    let mut direct_merged: HashSet<&str> = HashSet::new();
    for b in branches {
        let kind = if b.is_remote { "remote" } else { "local " };
        let _ = writeln!(out, "\n{kind} {} @ {}", b.name, short(b.tip_oid));
        if b.is_head {
            let _ = writeln!(out, "  guard: checked-out HEAD — never classified");
            continue;
        }
        if b.name == base.name || tips.contains(&b.tip_oid) {
            let _ = writeln!(out, "  guard: trunk / trunk tip — never classified");
            continue;
        }
        if trunk_line.contains(&b.tip_oid) {
            let _ = writeln!(
                out,
                "  guard: tip is ON a trunk line's first-parent chain — behind, not merged (#112)"
            );
            continue;
        }
        if b.is_remote && gh_key(b) == base_short {
            let _ = writeln!(out, "  guard: trunk mirror on another remote — never classified");
            continue;
        }
        let stale_tracking = if b.is_remote {
            branches.iter().any(|l| {
                !l.is_remote
                    && l.name == gh_key(b)
                    && l.tip_oid != b.tip_oid
                    && repo.graph_descendant_of(l.tip_oid, b.tip_oid).unwrap_or(false)
            })
        } else {
            b.ahead == 0 && b.behind > 0
        };
        if stale_tracking {
            let _ = writeln!(
                out,
                "  lags the live copy of its own line — stale tracking ref, ancestry signal disabled (#105/#107)"
            );
        }
        let mut verdict: Option<String> = None;
        for &t in &tips {
            let fork = repo.merge_base(b.tip_oid, t).ok();
            let dist = fork.map(|f| {
                let mut walk = match repo.revwalk() {
                    Ok(w) => w,
                    Err(_) => return "?".to_string(),
                };
                if walk.push(t).is_err() {
                    return "?".to_string();
                }
                let _ = walk.hide(f);
                let n = walk.take(SQUASH_SCAN_LIMIT + 1).count();
                if n > SQUASH_SCAN_LIMIT { format!(">{SQUASH_SCAN_LIMIT} (CAP!)") } else { n.to_string() }
            });
            let ancestry = is_ancestor_merged(repo, b.tip_oid, t);
            let squash = squash_merge_target(repo, b.tip_oid, t);
            let contained = branch_content_in_base(repo, b.tip_oid, t);
            let _ = writeln!(
                out,
                "  vs {}: fork={} dist={} ancestry={} squash={} contained={}",
                short(t),
                fork.map(short).unwrap_or_else(|| "none(unrelated)".into()),
                dist.unwrap_or_else(|| "-".into()),
                ancestry,
                squash.map(short).unwrap_or_else(|| "miss".into()),
                contained,
            );
            if verdict.is_none() {
                if ancestry && !stale_tracking {
                    verdict = Some(format!("MERGED (ancestry into {})", short(t)));
                } else if let Some(s) = squash {
                    verdict = Some(format!("MERGED (squash target {})", short(s)));
                }
            }
        }
        let key = gh_key(b);
        let in_gh = gh_merged.contains(key);
        let _ = writeln!(out, "  gh: key '{key}' in merged-PR set: {in_gh}");
        if verdict.is_none() && in_gh {
            if tips.iter().any(|&t| branch_content_in_base(repo, b.tip_oid, t)) {
                verdict = Some("MERGED (gh signal + content contained)".into());
            } else {
                let _ = writeln!(out, "  gh signal present but content NOT contained (conflict-resolved landing, or novel work)");
            }
        }
        if verdict.is_some() {
            direct_merged.insert(&b.name);
        }
        let _ = writeln!(out, "  => {}", verdict.unwrap_or_else(|| "visible (not classified merged)".into()));
    }

    // The real classifier iterates to a fixed point, re-testing unclassified
    // branches against the tips of branches already classified (transitive
    // merges). Run it and report what only that pass caught, naming a merged
    // branch whose tip carries the landing when one is identifiable.
    let final_merged = classify_merged_branches(repo, branches, base.tip_oid, &base.name, gh_merged);
    let transitive: Vec<&BranchInfo> = branches
        .iter()
        .filter(|b| final_merged.contains(&b.name) && !direct_merged.contains(b.name.as_str()))
        .collect();
    if !transitive.is_empty() {
        let _ = writeln!(out, "\ntransitive pass (merged into an already-merged branch):");
        for b in transitive {
            let via = branches.iter().find(|a| {
                a.name != b.name
                    && a.tip_oid != b.tip_oid
                    && final_merged.contains(&a.name)
                    && (is_ancestor_merged(repo, b.tip_oid, a.tip_oid)
                        || squash_merge_target(repo, b.tip_oid, a.tip_oid).is_some())
            });
            let _ = writeln!(
                out,
                "  {} => MERGED transitively{}",
                b.name,
                via.map(|a| format!(" (landed in merged branch {} @ {})", a.name, short(a.tip_oid)))
                    .unwrap_or_default()
            );
        }
    }
    out
}

/// Names of the branches — **local or remote** — merged into the base branch,
/// combining local git detection (ancestry + patch-id squash detection) with the GitHub
/// merged-PR signal (cross-checked locally via [`branch_content_in_base`]).
/// Remote refs are included because a GitHub squash-merge often leaves the
/// *remote* branch as the surviving ref (issue #100). See [`branch_is_merged`].
/// Transitive: a branch merged into a branch in the set is in the set (see
/// [`classify_merged_branches_with_targets`]).
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
    let mut tips = base_tips(branches, base_tip, base_name);
    let trunk_line = trunk_first_parent_line(repo, &tips);
    let mut merged = HashSet::new();
    let mut targets = HashMap::new();
    // Fixed point over "merged into a merged branch": work that landed in an
    // already-merged branch has itself landed (a stacked PR squashed into its
    // parent branch, a sub-branch merged into a feature before the feature was
    // squashed to trunk). Each round tests the still-unclassified branches
    // against the trunk tips PLUS every tip classified so far, and stops when a
    // round classifies nothing new — chains resolve one level per round, so the
    // round count is bounded by the deepest merge chain (≤ #branches).
    // `trunk_line` stays trunk-only on purpose: an old pointer into a *merged*
    // branch's own line is dead work that landed, not a live "behind" trunk
    // state, so it classifies via ancestry instead of being refused.
    loop {
        let mut grew = false;
        for b in branches {
            if merged.contains(&b.name)
                || !branch_is_merged(repo, b, branches, &tips, &trunk_line, base_name, gh_merged)
            {
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
            grew = true;
        }
        if !grew {
            break;
        }
        // Newly-merged tips become additional "landed" tips for the next round.
        // Extended only BETWEEN rounds, so within a round every branch is judged
        // against the same tip set (same-tip twins classify together before the
        // tip-equality guard in `branch_is_merged` could refuse the second one).
        for b in branches {
            if merged.contains(&b.name) && !tips.contains(&b.tip_oid) {
                tips.push(b.tip_oid);
            }
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

/// Every locally-known tip of the branch named `base_ref` (a PR's `baseRefName`,
/// always unqualified): the local branch of that name plus each remote's
/// `<remote>/<base_ref>`. All tips are returned rather than picking one because
/// local and remote may disagree (unpushed / unfetched base commits) and a
/// back-merge's second parent need only be reachable from *some* tip of the
/// base — testing each costs one bounded walk per tip.
pub fn base_ref_tips(branches: &[BranchInfo], base_ref: &str) -> Vec<Oid> {
    let mut tips: Vec<Oid> = branches
        .iter()
        .filter(|b| {
            if b.is_remote {
                // "<remote>/<branch>": the segment after the remote must equal
                // the base name exactly — `ends_with` would also claim
                // `origin/feature/dev` for base `dev`.
                b.name.split_once('/').is_some_and(|(_, rest)| rest == base_ref)
            } else {
                b.name == base_ref
            }
        })
        .map(|b| b.tip_oid)
        .collect();
    tips.sort();
    tips.dedup();
    tips
}

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
/// Pure over a `git2::Repository`; each pair is an open PR's head-commit OID
/// (from `PrInfo::head_oid`) with the tip of **that PR's own base branch** — a
/// PR targeting `dev` back-merges `dev`, and testing its merges against the
/// repo-wide trunk instead silently matched nothing (#103). A head may appear
/// in several pairs (one per locally-known tip of its base). Cheap: for each
/// pair it walks only that branch's own commits (the revwalk hides `base_tip`),
/// bounded by [`BASE_UPDATE_SCAN_LIMIT`], with one ancestry query per merge
/// encountered.
pub fn classify_base_update_merges(
    repo: &Repository,
    head_base_pairs: &[(Oid, Oid)],
) -> HashSet<Oid> {
    let mut out = HashSet::new();
    for &(head, base_tip) in head_base_pairs {
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
        // main advances to s (squash of feature). topic points at an older main
        // commit — ON the trunk's own line, so it is behind, not merged (#112).
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
        assert!(
            !merged.contains("topic"),
            "a tip on the trunk's own line is behind, not merged (#112): {merged:?}"
        );
        assert!(merged.contains("feature"), "squash merge");
        assert!(!merged.contains("gone"), "never merged");
        assert!(!merged.contains("main"), "base/HEAD is never classified");
        assert_eq!(merged.len(), 1);

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

    // ── Squash onto an ADVANCED base (issue #97) ─────────────────────────────

    #[test]
    fn squash_with_advanced_base_overlapping_context_is_detected() {
        // Scenario (b): a multi-commit feature squashed onto a base that ADVANCED
        // since the fork, where the advancing commit edits a line within the diff
        // context window of the feature's own change (context drift). With default
        // (3-line) context the branch's cumulative diff and the squash's diff carry
        // DIFFERENT surrounding context, so a context-sensitive patch-id misses the
        // match; the zero-context patch-id keys only on the changed lines and hits.
        //
        // file.txt starts "L1..L5"; feature inserts FEAT after L2; the base then
        // changes L1 -> L1x (adjacent to FEAT) before the feature squashes in.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("file.txt", "L1\nL2\nL3\nL4\nL5\n")]);
        let f1 = commit(&repo, "refs/heads/feature", 1100, &[a], &[("file.txt", "L1\nL2\nFEAT\nL3\nL4\nL5\n")]);
        let f2 = commit(&repo, "refs/heads/feature", 1200, &[f1], &[("file.txt", "L1\nL2\nFEAT\nL3\nL4\nL5\n"), ("note.txt", "n")]);
        // Base advances (L1 -> L1x), then the squash lands feature on top of it.
        let adv = commit(&repo, "refs/heads/main", 2000, &[a], &[("file.txt", "L1x\nL2\nL3\nL4\nL5\n")]);
        let s = commit(&repo, "refs/heads/main", 3000, &[adv], &[("file.txt", "L1x\nL2\nFEAT\nL3\nL4\nL5\n"), ("note.txt", "n")]);

        assert!(!is_ancestor_merged(&repo, f2, s), "no shared commit after a squash");
        assert!(
            is_squash_merged(&repo, f2, s),
            "squash onto an advanced base (context drift) must be detected"
        );
        // The link-line target (#81) is still nameable: the squash commit `s`.
        assert_eq!(squash_merge_target(&repo, f2, s), Some(s));
    }

    #[test]
    fn squash_survives_later_trunk_edits_to_the_landed_lines() {
        // After a genuine squash, the trunk keeps evolving — including editing
        // the very lines the branch introduced. Containment against the base
        // TIP three-way-conflicts then (branch adds FEAT where the tip has
        // FEAT-v2), so the cross-check must run at the matched squash commit,
        // where the work is contained by construction.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("file.txt", "L1\nL2\nL3\n")]);
        let f = commit(&repo, "refs/heads/feature", 1100, &[a], &[("file.txt", "L1\nL2\nFEAT\nL3\n")]);
        let s = commit(&repo, "refs/heads/main", 2000, &[a], &[("file.txt", "L1\nL2\nFEAT\nL3\n")]);
        // Trunk later rewrites the landed line.
        let t = commit(&repo, "refs/heads/main", 3000, &[s], &[("file.txt", "L1\nL2\nFEAT-v2\nL3\n")]);

        // Sanity: the gap this test pins down — at the tip the branch is no
        // longer cleanly contained, so a tip-anchored gate would miss it.
        assert!(!branch_content_in_base(&repo, f, t));
        assert!(
            is_squash_merged(&repo, f, t),
            "a squashed branch must stay classified after trunk edits its lines"
        );
        assert_eq!(squash_merge_target(&repo, f, t), Some(s), "link target is the squash commit");
    }

    #[test]
    fn advanced_base_but_unlanded_branch_is_not_squash_detected() {
        // Precision guard for the zero-context patch-id: the same advanced-base
        // shape, but the feature's change NEVER landed on the base. Dropping diff
        // context must not make an unrelated branch collide.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("file.txt", "L1\nL2\nL3\nL4\nL5\n")]);
        let f = commit(&repo, "refs/heads/feature", 1100, &[a], &[("file.txt", "L1\nL2\nFEAT\nL3\nL4\nL5\n")]);
        // Base advances but never picks up FEAT.
        let adv = commit(&repo, "refs/heads/main", 2000, &[a], &[("file.txt", "L1x\nL2\nL3\nL4\nL5\n")]);
        assert!(!is_squash_merged(&repo, f, adv), "an unlanded branch must never be squash-detected");
        assert!(!is_merged_into(&repo, f, adv));
    }

    #[test]
    fn same_line_inserted_at_a_different_place_does_not_collide() {
        // Precision guard for the zero-context patch-id: an unrelated trunk commit
        // and a feature branch each insert the SAME line into the SAME file but at
        // DIFFERENT places, and the feature never landed. Zero-context patch-ids
        // collide (identical added-line set), so the three-way containment
        // cross-check is what must keep the branch visible.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("file.txt", "A\nB\nC\nD\nE\n")]);
        // Trunk commit inserts INSERTED after A.
        let p = commit(&repo, "refs/heads/main", 2000, &[a], &[("file.txt", "A\nINSERTED\nB\nC\nD\nE\n")]);
        // Feature inserts the SAME line after D — genuinely different, unlanded work.
        let f = commit(&repo, "refs/heads/feature", 2100, &[a], &[("file.txt", "A\nB\nC\nD\nINSERTED\nE\n")]);

        assert!(!branch_content_in_base(&repo, f, p), "the two insertions differ — not contained");
        assert!(
            !is_squash_merged(&repo, f, p),
            "a zero-context patch-id collision must not classify unlanded work as merged"
        );
        assert_eq!(squash_merge_target(&repo, f, p), None, "no genuine squash to name");
        assert!(!is_merged_into(&repo, f, p));
    }

    #[test]
    fn conflict_resolved_squash_needs_the_gh_signal() {
        // Scenario (c): the landed squash differs slightly from the branch diff (a
        // maintainer resolved a conflict on the way in), so no local signal — not
        // patch-id, and not the three-way content check (the divergent lines
        // conflict) — can classify it. Only the GitHub PR signal can, and it must
        // NOT be rescued locally: local precision stays intact.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("file.txt", "L1\nL2\nL3\n")]);
        let f = commit(&repo, "refs/heads/feature", 1100, &[a], &[("file.txt", "L1\nBRANCH\nL3\n")]);
        // The landed version differs (maintainer tweaked BRANCH -> BRANCH-fixed);
        // the base also advanced the same line, so a merge would conflict.
        let s = commit(&repo, "refs/heads/main", 3000, &[a], &[("file.txt", "L1\nBRANCH-fixed\nL3\n")]);
        assert!(!is_squash_merged(&repo, f, s), "content drift: patch-id cannot match");
        assert!(!branch_content_in_base(&repo, f, s), "divergent lines conflict, so not contained");
        // Without gh it stays visible; the gh signal alone can't rescue it either,
        // because the content cross-check (correctly) rejects the conflict.
        let branches = vec![local("main", s, true), local("feature", f, false)];
        assert!(!classify_merged_branches(&repo, &branches, s, "main", &gh(&["feature"])).contains("feature"));
    }

    #[test]
    fn gh_signal_classifies_multi_commit_landing_onto_an_advanced_base() {
        // Scenario (b) via the GitHub fallback: the branch's work landed across
        // MULTIPLE base commits (so no single squash commit's patch-id matches),
        // AND the base advanced by editing a file the branch also carries an older
        // copy of. The old file-delta cross-check read that behind-file as novel
        // work and missed it; the three-way content check attributes the base's
        // edit to the base side and correctly sees the branch as contained.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("shared.txt", "S1\nS2\nS3\n")]);
        // Feature adds x.txt and y.txt across two commits (no single squash equals
        // its combined diff), keeping shared.txt at the forked content.
        let f1 = commit(&repo, "refs/heads/feature", 1100, &[a], &[("shared.txt", "S1\nS2\nS3\n"), ("x.txt", "x")]);
        let f2 = commit(&repo, "refs/heads/feature", 1200, &[f1], &[("shared.txt", "S1\nS2\nS3\n"), ("x.txt", "x"), ("y.txt", "y")]);
        // Base lands x and y as two separate commits AND advances shared.txt.
        let b1 = commit(&repo, "refs/heads/main", 2000, &[a], &[("shared.txt", "S1x\nS2\nS3\n"), ("x.txt", "x")]);
        let b2 = commit(&repo, "refs/heads/main", 2100, &[b1], &[("shared.txt", "S1x\nS2\nS3\n"), ("x.txt", "x"), ("y.txt", "y")]);

        assert!(!is_ancestor_merged(&repo, f2, b2));
        assert!(!is_squash_merged(&repo, f2, b2), "no single squash commit matches");
        assert!(
            branch_content_in_base(&repo, f2, b2),
            "three-way check: branch content is contained despite the advanced shared file"
        );
        let branches = vec![local("main", b2, true), local("feature", f2, false)];
        // Without gh: content match alone isn't trusted → stays visible.
        assert!(!classify_merged_branches(&repo, &branches, b2, "main", &HashSet::new()).contains("feature"));
        // With gh: classified.
        assert!(classify_merged_branches(&repo, &branches, b2, "main", &gh(&["feature"])).contains("feature"));
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
    fn squash_on_the_working_trunks_remote_lands_before_the_local_pull() {
        // #109, reproduced live via PR #84 on keifu itself: a squash-merge PR
        // against the working trunk (chong-dev) lands on origin/chong-dev and
        // GitHub deletes the PR branch; the LOCAL trunk lags until the next
        // pull. The landed branch must classify in that window — the working
        // trunk's remote counterpart is a trunk tip too.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let d1 = commit(&repo, "refs/heads/dev", 2000, &[a], &[("base.txt", "base"), ("d.txt", "d")]);
        // Feature off the working trunk, two commits.
        let f1 = commit(&repo, "refs/heads/feature", 3000, &[d1], &[("base.txt", "base"), ("d.txt", "d"), ("f.txt", "one")]);
        let f2 = commit(&repo, "refs/heads/feature", 3100, &[f1], &[("base.txt", "base"), ("d.txt", "d"), ("f.txt", "one\ntwo")]);
        // The squash lands on origin/dev only; local dev stays at d1.
        let s = commit(&repo, "refs/remotes/origin/dev", 4000, &[d1], &[("base.txt", "base"), ("d.txt", "d"), ("f.txt", "one\ntwo")]);

        let dev = BranchInfo {
            name: "dev".into(),
            is_head: true,
            is_remote: false,
            upstream: Some("origin/dev".into()),
            tip_oid: d1,
            ahead: 0,
            behind: 1,
        };
        let branches = vec![
            local("main", a, false),
            dev,
            remote("origin/dev", s),
            local("feature", f2, false),
        ];
        let base = base_branch(&branches).unwrap();
        assert_eq!(base.name, "main");
        let (set, targets) = classify_merged_branches_with_targets(
            &repo,
            &branches,
            base.tip_oid,
            &base.name,
            &HashSet::new(),
        );
        assert!(set.contains("feature"), "landed branch classifies pre-pull: {set:?}");
        assert_eq!(targets.get("feature"), Some(&s), "squash target on origin/dev");
        assert!(!set.contains("origin/dev"), "the working trunk's remote counterpart is a trunk");
        assert!(!set.contains("dev"), "HEAD never classifies");
    }

    #[test]
    fn remote_mirror_behind_its_local_counterpart_is_never_merged() {
        // #107 user repro: chong-dev is checked out with unpushed commits, so
        // origin/chong-dev lags the local tip. Since the checked-out tip is a
        // trunk tip (#103), the mirror's tip is an ancestor of it and the
        // ancestry signal would dim/hide the remote copy of the very line the
        // user is working on.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let r = commit(&repo, "refs/remotes/origin/dev", 2000, &[a], &[("base.txt", "base"), ("d.txt", "one")]);
        let l = commit(&repo, "refs/heads/dev", 2100, &[r], &[("base.txt", "base"), ("d.txt", "one\ntwo")]);

        let branches = vec![
            local("main", a, false),
            local("dev", l, true), // checked out, ahead of its remote
            remote("origin/dev", r),
        ];
        let base = base_branch(&branches).unwrap();
        let set = classify_merged_branches(&repo, &branches, base.tip_oid, &base.name, &HashSet::new());
        assert!(
            !set.contains("origin/dev"),
            "the remote mirror of the live local line is stale, not merged: {set:?}"
        );
    }

    #[test]
    fn non_selected_trunk_name_behind_the_checked_out_branch_is_never_merged() {
        // With both main and master present, main is the selected base — but
        // master (upstream-less, fully contained in the checked-out feature)
        // must not classify either: trunk-by-convention names are trunks.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let m = commit(&repo, "refs/heads/master", 1100, &[a], &[("base.txt", "base"), ("m.txt", "m")]);
        let f = commit(&repo, "refs/heads/feature", 2000, &[m], &[("base.txt", "base"), ("m.txt", "m"), ("f.txt", "f")]);

        let branches = vec![
            local("main", a, false),
            local("master", m, false),
            local("feature", f, true),
        ];
        let base = base_branch(&branches).unwrap();
        assert_eq!(base.name, "main");
        let set = classify_merged_branches(&repo, &branches, base.tip_oid, &base.name, &HashSet::new());
        assert!(!set.contains("master"), "a conventional trunk name never classifies: {set:?}");
        assert!(!set.contains("main"));
    }

    #[test]
    fn local_branch_behind_its_upstream_is_never_marked_merged() {
        // #105 user repro: local `dev` lags `origin/dev`, and the checked-out
        // branch forked from origin/dev — so dev's tip is an ancestor of the
        // HEAD trunk tip. That is staleness (the remote counterpart is the live
        // line), not landed work; the ancestry signal must not classify it.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let d1 = commit(&repo, "refs/heads/dev", 2000, &[a], &[("base.txt", "base"), ("d.txt", "one")]);
        let d2 = commit(&repo, "refs/remotes/origin/dev", 2100, &[d1], &[("base.txt", "base"), ("d.txt", "one\ntwo")]);
        let f = commit(&repo, "refs/heads/feature", 3000, &[d2], &[("base.txt", "base"), ("d.txt", "one\ntwo"), ("f.txt", "f")]);

        let dev = BranchInfo {
            name: "dev".into(),
            is_head: false,
            is_remote: false,
            upstream: Some("origin/dev".into()),
            tip_oid: d1,
            ahead: 0,
            behind: 1,
        };
        let branches = vec![
            local("main", a, false),
            dev,
            remote("origin/dev", d2),
            local("feature", f, true), // checked out — its tip contains origin/dev
        ];
        let base = base_branch(&branches).unwrap();
        let set = classify_merged_branches(&repo, &branches, base.tip_oid, &base.name, &HashSet::new());
        assert!(
            !set.contains("dev"),
            "a branch strictly behind its upstream is stale, not merged: {set:?}"
        );
        // The live remote line the checked-out branch grew FROM sits on HEAD's
        // own first-parent chain — an older state of the line in view, so it is
        // behind, not merged (#112).
        assert!(
            !set.contains("origin/dev"),
            "origin/dev is on the checked-out line — behind, not merged (#112): {set:?}"
        );
    }

    #[test]
    fn branch_behind_on_the_checked_out_line_is_not_merged() {
        // #112 user repro: an upstream-less local branch pointing at an OLDER
        // commit of the very line that is checked out. Its tip is an ancestor
        // of the HEAD trunk tip (#103), so ancestry read it as "merged" and it
        // was hidden/dimmed — but it is merely behind on the same line.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let d1 = commit(&repo, "refs/heads/dev", 2000, &[a], &[("base.txt", "base"), ("d.txt", "one")]);
        let d2 = commit(&repo, "refs/heads/dev", 2100, &[d1], &[("base.txt", "base"), ("d.txt", "one\ntwo")]);
        repo.reference("refs/heads/marker", d1, true, "marker").unwrap();

        let branches = vec![
            local("main", a, false),
            local("dev", d2, true), // checked out; marker sits on its line
            local("marker", d1, false),
        ];
        let base = base_branch(&branches).unwrap();
        let set = classify_merged_branches(&repo, &branches, base.tip_oid, &base.name, &HashSet::new());
        assert!(
            !set.contains("marker"),
            "a branch behind on the checked-out line is not merged (#112): {set:?}"
        );
        // Not even a merged-PR name match may override it: for an on-line tip
        // the containment cross-check is trivially true, so the gh name alone
        // would decide — and behind must never read as merged.
        let set = classify_merged_branches(&repo, &branches, base.tip_oid, &base.name, &gh(&["marker"]));
        assert!(
            !set.contains("marker"),
            "the gh signal must not classify an on-line (behind) tip: {set:?}"
        );
    }

    #[test]
    fn branch_behind_on_the_trunk_line_is_not_merged_but_a_merge_landing_is() {
        // #112 against the primary trunk, with a control: `old` points at an
        // older main commit (on the first-parent line → behind, not merged),
        // while `topic` landed via a merge commit — its tip reachable only
        // through the merge's SECOND parent — and must keep classifying.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "main")]);
        let t = commit(&repo, "refs/heads/topic", 2100, &[a], &[("base.txt", "base"), ("t.txt", "t")]);
        let m = commit(
            &repo,
            "refs/heads/main",
            3000,
            &[b, t],
            &[("base.txt", "base"), ("main.txt", "main"), ("t.txt", "t")],
        );
        repo.reference("refs/heads/old", b, true, "old").unwrap();

        let branches = vec![
            local("main", m, true),
            local("old", b, false),
            local("topic", t, false),
        ];
        let set = classify_merged_branches(&repo, &branches, m, "main", &HashSet::new());
        assert!(
            !set.contains("old"),
            "a tip on the trunk's first-parent line is behind, not merged (#112): {set:?}"
        );
        assert!(
            set.contains("topic"),
            "a merge-commit landing (tip off the line) still classifies: {set:?}"
        );
    }

    #[test]
    fn branches_merged_into_the_checked_out_trunk_are_classified() {
        // #100 (found via --explain-merged on keifu's own repo): the working
        // trunk is a long-lived non-main branch (chong-dev style). Branches are
        // merged INTO the checked-out branch; local main lags far behind and
        // knows nothing about them. They must still classify — the checked-out
        // tip is a trunk tip — while neither main nor the checked-out branch
        // itself ever classifies.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        // Feature branched from main, merged into dev (real merge commit).
        let f = commit(&repo, "refs/heads/feature", 2000, &[a], &[("base.txt", "base"), ("f.txt", "f")]);
        let d1 = commit(&repo, "refs/heads/dev", 2100, &[a], &[("base.txt", "base"), ("d.txt", "d")]);
        let d2 = commit(&repo, "refs/heads/dev", 2200, &[d1, f], &[("base.txt", "base"), ("d.txt", "d"), ("f.txt", "f")]);
        // A second feature squash-merged into dev.
        let g = commit(&repo, "refs/heads/feature2", 3000, &[a], &[("base.txt", "base"), ("g.txt", "g")]);
        let s = commit(&repo, "refs/heads/dev", 3100, &[d2], &[("base.txt", "base"), ("d.txt", "d"), ("f.txt", "f"), ("g.txt", "g")]);
        // An unlanded branch.
        let u = commit(&repo, "refs/heads/wip", 4000, &[a], &[("base.txt", "base"), ("u.txt", "u")]);

        let branches = vec![
            local("main", a, false),
            local("dev", s, true), // checked out; d1..s never reached main
            local("feature", f, false),
            local("feature2", g, false),
            local("wip", u, false),
        ];
        let base = base_branch(&branches).unwrap();
        assert_eq!(base.name, "main", "main still selected as primary base");
        let (set, targets) = classify_merged_branches_with_targets(
            &repo,
            &branches,
            base.tip_oid,
            &base.name,
            &HashSet::new(),
        );
        assert!(set.contains("feature"), "merge-commit landing on the checked-out trunk: {set:?}");
        assert!(set.contains("feature2"), "squash landing on the checked-out trunk: {set:?}");
        assert_eq!(targets.get("feature2"), Some(&s), "squash target on the checked-out trunk");
        assert!(!set.contains("wip"), "unlanded work stays visible");
        assert!(!set.contains("main"), "the lagging primary trunk is never classified");
        assert!(!set.contains("dev"), "the checked-out branch is never classified");
    }

    #[test]
    fn user_flow_local_survivor_remote_branch_deleted_squash_on_drifted_remote_trunk() {
        // The exact #100 user shape: GitHub deletes the REMOTE branch after the
        // squash-merge, the LOCAL feature ref survives; the squash exists only
        // on origin/main (local main never pulled); origin/main also drifted
        // (another commit landed near the feature's edit before the squash);
        // HEAD sits on an unrelated dev branch.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a1 = commit(&repo, "refs/heads/main", 1000, &[], &[("file.txt", "L1\nL2\nL3\nL4\n")]);
        let a2 = commit(&repo, "refs/heads/main", 1100, &[a1], &[("file.txt", "L1\nL2\nL3\nL4\n"), ("readme.md", "r")]);
        // Local feature: two commits, ref survives the merge.
        let f1 = commit(&repo, "refs/heads/feature", 2000, &[a2], &[("file.txt", "L1\nL2\nFEAT\nL3\nL4\n"), ("readme.md", "r")]);
        let f2 = commit(&repo, "refs/heads/feature", 2100, &[f1], &[("file.txt", "L1\nL2\nFEAT\nL3\nL4\n"), ("readme.md", "r"), ("new.txt", "n")]);
        // origin/main: a drift commit editing a line adjacent to the feature's
        // change, then the squash. No origin/feature ref exists (deleted).
        let m3 = commit(&repo, "refs/remotes/origin/main", 3000, &[a2], &[("file.txt", "L1x\nL2\nL3\nL4\n"), ("readme.md", "r")]);
        let s = commit(&repo, "refs/remotes/origin/main", 4000, &[m3], &[("file.txt", "L1x\nL2\nFEAT\nL3\nL4\n"), ("readme.md", "r"), ("new.txt", "n")]);
        let d = commit(&repo, "refs/heads/dev", 5000, &[a2], &[("file.txt", "L1\nL2\nL3\nL4\n"), ("readme.md", "r"), ("dev.txt", "d")]);

        let branches = vec![
            local("main", a2, false),
            local("feature", f2, false),
            local("dev", d, true),
            remote("origin/main", s),
        ];
        let base = base_branch(&branches).unwrap();
        assert_eq!(base.name, "main", "local main preferred even though behind");
        let (set, targets) = classify_merged_branches_with_targets(
            &repo,
            &branches,
            base.tip_oid,
            &base.name,
            &HashSet::new(),
        );
        assert!(
            set.contains("feature"),
            "surviving local ref must classify via the drifted origin/main tip: {set:?}"
        );
        assert_eq!(targets.get("feature"), Some(&s), "link target is the squash on origin/main");
        assert!(!set.contains("dev"), "unlanded work stays visible");
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
        // The remote-tip reach must also cover a plain merge-COMMIT landing that
        // has been fetched into origin/main but not pulled to local main. The
        // topic tip hangs off origin/main's first-parent line (reachable only
        // via the merge's second parent), so #112's behind-guard does not fire.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let t = commit(&repo, "refs/heads/topic", 2000, &[a], &[("base.txt", "base"), ("t.txt", "t")]);
        // origin/main advanced on its own line, then merged topic in; local
        // main lags at `a`. Parents [x (trunk, first), t (topic, second)].
        let x = commit(&repo, "refs/remotes/origin/main", 2400, &[a], &[("base.txt", "base"), ("x.txt", "x")]);
        let u = commit(
            &repo,
            "refs/remotes/origin/main",
            2500,
            &[x, t],
            &[("base.txt", "base"), ("x.txt", "x"), ("t.txt", "t")],
        );
        let branches = vec![local("main", a, true), remote("origin/main", u), local("topic", t, false)];
        let base = base_branch(&branches).unwrap();
        let merged = merged_local_branches(&repo, &branches, base.tip_oid, &base.name);
        assert!(merged.contains("topic"), "merge landed only on the remote trunk must classify: {merged:?}");
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

    // ── Remote-branch classification (issue #100 H1) ─────────────────────────
    //
    // After a GitHub squash-merge the surviving ref is frequently the *remote*
    // branch (`origin/feature`): the local copy was deleted, or the branch was
    // only ever fetched. These must be classified (hidden + link-lined) exactly
    // like a surviving local ref. The real-repo suite in
    // `tests/squash_real_world_test.rs` first exposed that they were not.

    #[test]
    fn remote_only_squash_merged_branch_is_classified() {
        // The wild-repo failure shape, minimised: `origin/feature` is the only
        // surviving ref of a squash-merged PR (no local counterpart). It must be
        // classified merged AND name the squash landing commit for the #81 link.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "main")]);
        // Feature (two commits) lives only on the remote after the merge.
        let f1 = commit(&repo, "refs/remotes/origin/feature", 2100, &[a], &[("base.txt", "base"), ("feat.txt", "one")]);
        let f2 = commit(&repo, "refs/remotes/origin/feature", 2200, &[f1], &[("base.txt", "base"), ("feat.txt", "one\ntwo")]);
        // Squash lands the feature's net diff on main.
        let s = commit(
            &repo,
            "refs/heads/main",
            3000,
            &[b],
            &[("base.txt", "base"), ("main.txt", "main"), ("feat.txt", "one\ntwo")],
        );

        let branches = vec![
            local("main", s, true),
            remote("origin/feature", f2),
        ];
        let (merged, targets) =
            classify_merged_branches_with_targets(&repo, &branches, s, "main", &HashSet::new());
        assert!(
            merged.contains("origin/feature"),
            "a squash-merged remote-only branch must be classified (issue #100)"
        );
        assert_eq!(
            targets.get("origin/feature"),
            Some(&s),
            "the remote squash branch links to the squash commit"
        );
    }

    #[test]
    fn remote_trunk_ref_is_never_classified_even_though_remotes_are_eligible() {
        // Precision guard for the #100 remote extension: `origin/main` (the trunk's
        // own remote counterpart) must never be classified as merged into itself,
        // whether it is equal to, ahead of, or behind the local trunk. It is always
        // a base tip, so the base-tip guard drops it.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        // origin/main ahead of local main by one commit.
        let u = commit(&repo, "refs/remotes/origin/main", 2000, &[a], &[("base.txt", "base"), ("u.txt", "u")]);
        let branches = vec![local("main", a, true), remote("origin/main", u)];
        let base = base_branch(&branches).unwrap();
        let merged = merged_local_branches(&repo, &branches, base.tip_oid, &base.name);
        assert!(!merged.contains("origin/main"), "the remote trunk is never classified as merged");
        assert!(merged.is_empty(), "no branch to classify here");
    }

    #[test]
    fn second_remote_trunk_mirror_that_lags_the_base_is_not_classified() {
        // Review guard (#100): a *second* remote's trunk mirror (`upstream/main`)
        // that lags the base is an ANCESTOR of it, so without a trunk-name guard it
        // would read as a landed feature branch and be hidden/dimmed. It is a trunk,
        // not a feature — it must stay visible. (`base_tips` only exempts the
        // `origin/` mirror, so this needs the short-name guard.)
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        // Local main advances past `a`.
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "main")]);
        // upstream/main lags at `a` — a strict ancestor of local main's tip.
        repo.reference("refs/remotes/upstream/main", a, true, "upstream/main").unwrap();
        // A genuinely-merged feature to prove the guard didn't over-suppress.
        let f = commit(&repo, "refs/remotes/origin/feature", 2100, &[a], &[("base.txt", "base"), ("main.txt", "main")]);

        let branches = vec![
            local("main", b, true),
            remote("upstream/main", a),
            remote("origin/feature", f),
        ];
        let merged = merged_local_branches(&repo, &branches, b, "main");
        assert!(
            !merged.contains("upstream/main"),
            "a second remote's trunk mirror must not be classified as a merged feature"
        );
        assert!(
            merged.contains("origin/feature"),
            "the guard is targeted: a genuinely-landed remote feature still classifies"
        );
    }

    #[test]
    fn gh_signal_classifies_a_remote_branch_by_prefix_stripped_name() {
        // GitHub reports `headRefName` without a remote prefix ("feature"), but the
        // surviving ref is `origin/feature`. The gh cross-check must strip the
        // remote segment so the name matches — and the content check must still
        // guard it (name reuse safety carries over to remotes).
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        // Branch content lands across two base commits (no single squash matches),
        // so only the gh signal + content check can classify it.
        let f1 = commit(&repo, "refs/remotes/origin/feature", 1100, &[a], &[("base.txt", "base"), ("x.txt", "x")]);
        let f2 = commit(&repo, "refs/remotes/origin/feature", 1200, &[f1], &[("base.txt", "base"), ("x.txt", "x"), ("y.txt", "y")]);
        let b1 = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("x.txt", "x")]);
        let b2 = commit(&repo, "refs/heads/main", 2100, &[b1], &[("base.txt", "base"), ("x.txt", "x"), ("y.txt", "y")]);

        let branches = vec![local("main", b2, true), remote("origin/feature", f2)];
        assert!(!is_squash_merged(&repo, f2, b2), "no single squash commit matches");
        // Without gh: content match alone is not trusted → stays visible.
        assert!(!classify_merged_branches(&repo, &branches, b2, "main", &HashSet::new()).contains("origin/feature"));
        // With gh reporting head "feature" (no prefix): the remote ref classifies.
        assert!(classify_merged_branches(&repo, &branches, b2, "main", &gh(&["feature"])).contains("origin/feature"));
    }

    #[test]
    fn remote_name_reuse_with_novel_content_is_not_classified() {
        // Name-reuse safety, remote edition: gh says a PR with head "feature"
        // merged, but the surviving `origin/feature` carries novel content the base
        // lacks (a new branch reused the name upstream). It must stay visible.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "main")]);
        let f = commit(&repo, "refs/remotes/origin/feature", 2100, &[a], &[("base.txt", "base"), ("novel.txt", "new work")]);
        let branches = vec![local("main", b, true), remote("origin/feature", f)];
        assert!(
            !classify_merged_branches(&repo, &branches, b, "main", &gh(&["feature"])).contains("origin/feature"),
            "remote name reuse with novel content must not be classified"
        );
    }

    // ── Update-merge / back-merge PR branches (issue #100 H2) ─────────────────

    #[test]
    fn squash_detected_after_advanced_base_was_back_merged_into_the_branch() {
        // A real PR often has the advanced base merged INTO it ("Update branch")
        // before it squash-lands. The merge_base then advances to that sync point;
        // the branch's cumulative diff from there must still equal the squash's
        // diff. Verifies classification survives the back-merge (issue #100 H2).
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        // Feature: two commits building feat.txt.
        let f1 = commit(&repo, "refs/heads/feature", 1100, &[a], &[("base.txt", "base"), ("feat.txt", "one")]);
        let f2 = commit(&repo, "refs/heads/feature", 1200, &[f1], &[("base.txt", "base"), ("feat.txt", "one\ntwo")]);
        // Base advances (adds main.txt) while the PR is open.
        let adv = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "main")]);
        // "Update branch": merge the advanced base INTO the feature branch. First
        // parent = feature (f2), second = base (adv). Tree carries both sides.
        let m = commit(
            &repo,
            "refs/heads/feature",
            2500,
            &[f2, adv],
            &[("base.txt", "base"), ("feat.txt", "one\ntwo"), ("main.txt", "main")],
        );
        // Squash lands the feature's net diff on top of the advanced base.
        let s = commit(
            &repo,
            "refs/heads/main",
            3000,
            &[adv],
            &[("base.txt", "base"), ("main.txt", "main"), ("feat.txt", "one\ntwo")],
        );

        assert_eq!(repo.merge_base(m, s).unwrap(), adv, "merge_base is the back-merge sync point");
        assert!(!is_ancestor_merged(&repo, m, s), "no shared commit after the squash");
        assert!(
            is_squash_merged(&repo, m, s),
            "a squash must still be detected after the base was back-merged into the branch"
        );
        assert_eq!(squash_merge_target(&repo, m, s), Some(s), "link target is the squash commit");
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
        assert!(!branch_content_in_base(&repo, f, b), "branch carries content the base lacks");
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
        assert!(branch_content_in_base(&repo, f2, b2), "all content is present in the base");

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

        assert!(branch_content_in_base(&repo, f, b), "net-empty branch has nothing to land");
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
        let set = classify_base_update_merges(&repo, &[(m, c)]);
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
        let set = classify_base_update_merges(&repo, &[(t, m)]);
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
        let set = classify_base_update_merges(&repo, &[(m, c)]);
        assert!(set.is_empty(), "merging a non-base sibling is not a base-update");
    }

    #[test]
    fn no_pr_heads_or_no_merges_yields_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let f = commit(&repo, "refs/heads/feature", 1500, &[a], &[("base.txt", "base"), ("x.txt", "x")]);
        // No PR heads at all.
        assert!(classify_base_update_merges(&repo, &[]).is_empty());
        // A PR head whose branch has no merge commit.
        assert!(classify_base_update_merges(&repo, &[(f, a)]).is_empty());
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
        let set = classify_base_update_merges(&repo, &[(g, c)]);
        assert!(set.contains(&m), "a mid-branch back-merge is still found");
    }

    #[test]
    fn back_merge_classifies_against_the_prs_own_base_not_the_trunk() {
        // The #103 shape: the repo's trunk is `main`, but the PR targets `dev`.
        // main: a
        // dev:  a <- d1 <- d2
        // feature (off dev): d1 <- f, then back-merge m = [f, d2].
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let d1 = commit(&repo, "refs/heads/dev", 1500, &[a], &[("base.txt", "base"), ("d.txt", "1")]);
        let f = commit(&repo, "refs/heads/feature", 2000, &[d1], &[("base.txt", "base"), ("d.txt", "1"), ("f.txt", "f")]);
        let d2 = commit(&repo, "refs/heads/dev", 2500, &[d1], &[("base.txt", "base"), ("d.txt", "2")]);
        let m = commit(
            &repo,
            "refs/heads/feature",
            3000,
            &[f, d2],
            &[("base.txt", "base"), ("d.txt", "2"), ("f.txt", "f")],
        );
        // Paired with the PR's real base (dev), the back-merge is found.
        let set = classify_base_update_merges(&repo, &[(m, d2)]);
        assert!(set.contains(&m), "dev back-merge classifies against dev's tip");
        // Paired with the repo-wide trunk (the pre-#103 behavior), it is
        // invisible: m's second parent is on dev, not on main.
        assert!(
            classify_base_update_merges(&repo, &[(m, a)]).is_empty(),
            "the trunk pairing cannot see a dev back-merge — the old bug"
        );
    }

    // ── base_ref_tips: resolving a PR's baseRefName locally (#103) ───

    #[test]
    fn base_ref_tips_collects_local_and_every_remote_mirror() {
        let o = |n: u32| Oid::from_str(&format!("{n:040x}")).unwrap();
        let branches = vec![
            local("dev", o(1), false),
            remote("origin/dev", o(2)),
            remote("upstream/dev", o(3)),
            local("main", o(4), true),
            remote("origin/main", o(5)),
        ];
        let tips = base_ref_tips(&branches, "dev");
        assert_eq!(tips, vec![o(1), o(2), o(3)]);
    }

    #[test]
    fn base_ref_tips_requires_an_exact_branch_segment_match() {
        let o = |n: u32| Oid::from_str(&format!("{n:040x}")).unwrap();
        // `origin/feature/dev` is the branch `feature/dev`, not `dev` — a
        // suffix match would wrongly claim it.
        let branches = vec![
            remote("origin/feature/dev", o(1)),
            local("feature/dev", o(2), false),
        ];
        assert!(base_ref_tips(&branches, "dev").is_empty());
        assert_eq!(base_ref_tips(&branches, "feature/dev"), vec![o(1), o(2)]);
    }

    #[test]
    fn base_ref_tips_dedups_agreeing_local_and_remote() {
        let o = |n: u32| Oid::from_str(&format!("{n:040x}")).unwrap();
        let branches = vec![local("dev", o(7), false), remote("origin/dev", o(7))];
        assert_eq!(base_ref_tips(&branches, "dev"), vec![o(7)]);
        assert!(base_ref_tips(&[], "dev").is_empty());
    }

    // ── transitive classification: merged into a merged branch ───────

    #[test]
    fn branch_merged_into_a_squash_merged_branch_is_classified_transitively() {
        // sub was merge-committed into feature, then feature was SQUASHED to
        // main — so no commit of sub is an ancestor of the trunk, and sub's own
        // cumulative diff (+sub.txt) matches no single trunk commit (the squash
        // carries feature's whole diff). Only testing sub against the tip of
        // the already-classified feature branch catches it.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "main")]);
        let s1 = commit(&repo, "refs/heads/sub", 2100, &[a], &[("base.txt", "base"), ("sub.txt", "s")]);
        let f1 = commit(&repo, "refs/heads/feature", 2200, &[a], &[("base.txt", "base"), ("feat.txt", "f")]);
        let f2 = commit(
            &repo,
            "refs/heads/feature",
            2300,
            &[f1, s1],
            &[("base.txt", "base"), ("feat.txt", "f"), ("sub.txt", "s")],
        );
        let sq = commit(
            &repo,
            "refs/heads/main",
            3000,
            &[b],
            &[("base.txt", "base"), ("main.txt", "main"), ("feat.txt", "f"), ("sub.txt", "s")],
        );

        let branches = vec![
            local("main", sq, true),
            local("feature", f2, false),
            local("sub", s1, false),
        ];
        let merged = merged_local_branches(&repo, &branches, sq, "main");
        assert!(merged.contains("feature"), "feature squashed to trunk");
        assert!(merged.contains("sub"), "sub landed through feature (transitive)");
        assert!(!merged.contains("main"));
    }

    #[test]
    fn chain_of_squashes_is_classified_recursively() {
        // c squashed into bb, bb squashed into aa, aa squashed into main: each
        // level shares no commit with the level above, so every branch except
        // aa needs the fixed point — bb resolves in round two against aa's tip,
        // c in round three against bb's tip.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let base = ("base.txt", "base");
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[base]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[base, ("main.txt", "m")]);
        let c1 = commit(&repo, "refs/heads/c", 2100, &[a], &[base, ("c.txt", "c")]);
        let b1 = commit(&repo, "refs/heads/bb", 2200, &[a], &[base, ("bb.txt", "b")]);
        let b2 = commit(&repo, "refs/heads/bb", 2300, &[b1], &[base, ("bb.txt", "b"), ("c.txt", "c")]);
        let a1 = commit(&repo, "refs/heads/aa", 2400, &[a], &[base, ("aa.txt", "a")]);
        let a2 = commit(
            &repo,
            "refs/heads/aa",
            2500,
            &[a1],
            &[base, ("aa.txt", "a"), ("bb.txt", "b"), ("c.txt", "c")],
        );
        let sq = commit(
            &repo,
            "refs/heads/main",
            3000,
            &[b],
            &[base, ("main.txt", "m"), ("aa.txt", "a"), ("bb.txt", "b"), ("c.txt", "c")],
        );

        let branches = vec![
            local("main", sq, true),
            local("aa", a2, false),
            local("bb", b2, false),
            local("c", c1, false),
        ];
        let merged = merged_local_branches(&repo, &branches, sq, "main");
        assert!(merged.contains("aa"), "direct squash to trunk");
        assert!(merged.contains("bb"), "squashed into merged aa");
        assert!(merged.contains("c"), "squashed into transitively-merged bb");
        assert!(!merged.contains("main"));
    }

    #[test]
    fn branch_merged_into_an_unmerged_branch_stays_visible() {
        // sub landed in feature, but feature itself never landed on main — no
        // link to the trunk exists, so the fixed point must classify neither.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "m")]);
        let s1 = commit(&repo, "refs/heads/sub", 2100, &[a], &[("base.txt", "base"), ("sub.txt", "s")]);
        let f1 = commit(&repo, "refs/heads/feature", 2200, &[a], &[("base.txt", "base"), ("feat.txt", "f")]);
        let f2 = commit(
            &repo,
            "refs/heads/feature",
            2300,
            &[f1, s1],
            &[("base.txt", "base"), ("feat.txt", "f"), ("sub.txt", "s")],
        );

        let branches = vec![
            local("main", b, true),
            local("feature", f2, false),
            local("sub", s1, false),
        ];
        let merged = merged_local_branches(&repo, &branches, b, "main");
        assert!(merged.is_empty(), "no branch reached the trunk: {merged:?}");
    }

    #[test]
    fn old_pointer_into_a_merged_branchs_line_is_classified() {
        // backup points at an intermediate commit of a squash-merged branch.
        // On the TRUNK an on-line pointer is "behind, not merged" (#112) —
        // that guard protects live lines. A merged branch's line is dead,
        // landed work, so an old pointer into it has landed too: classified
        // via ancestry against the merged branch's tip in round two.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", 1000, &[], &[("base.txt", "base")]);
        let b = commit(&repo, "refs/heads/main", 2000, &[a], &[("base.txt", "base"), ("main.txt", "m")]);
        let f1 = commit(&repo, "refs/heads/feature", 2100, &[a], &[("base.txt", "base"), ("feat.txt", "one")]);
        let f2 = commit(&repo, "refs/heads/feature", 2200, &[f1], &[("base.txt", "base"), ("feat.txt", "two")]);
        repo.reference("refs/heads/backup", f1, true, "backup").unwrap();
        let sq = commit(
            &repo,
            "refs/heads/main",
            3000,
            &[b],
            &[("base.txt", "base"), ("main.txt", "m"), ("feat.txt", "two")],
        );

        let branches = vec![
            local("main", sq, true),
            local("feature", f2, false),
            local("backup", f1, false),
        ];
        let merged = merged_local_branches(&repo, &branches, sq, "main");
        assert!(merged.contains("feature"));
        assert!(merged.contains("backup"), "old state of a landed line is landed");
    }
}
