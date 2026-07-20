//! Branch info structure and operations

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use git2::{BranchType, Oid, Repository};

#[derive(Debug, Clone)]
pub struct BranchInfo {
    pub name: String,
    pub is_head: bool,
    pub is_remote: bool,
    pub upstream: Option<String>,
    pub tip_oid: Oid,
    /// Commits this branch is ahead of its upstream (0 when no upstream).
    pub ahead: usize,
    /// Commits this branch is behind its upstream (0 when no upstream).
    pub behind: usize,
}

impl BranchInfo {
    /// The branch name with any remote prefix stripped: `origin/main` -> `main`,
    /// `feature/x` (local) -> `feature/x`. Only the first path segment of a
    /// remote ref is the remote name, so the rest is the branch's own name.
    fn short_name(&self) -> &str {
        if self.is_remote {
            self.name.split_once('/').map_or(&self.name, |(_, rest)| rest)
        } else {
            &self.name
        }
    }

    pub fn list_all(repo: &Repository) -> Result<Vec<Self>> {
        let mut branches = Vec::new();

        // Get HEAD
        let head_oid = repo.head().ok().and_then(|r| r.target());

        // Local branches
        for branch_result in repo.branches(Some(BranchType::Local))? {
            let (branch, _) = branch_result?;
            if let Some(name) = branch.name()? {
                let reference = branch.get();
                if let Some(oid) = reference.target() {
                    let is_head = head_oid.map(|h| h == oid).unwrap_or(false)
                        && repo
                            .head()
                            .ok()
                            .and_then(|h| h.shorthand().map(|s| s == name))
                            .unwrap_or(false);

                    let upstream_branch = branch.upstream().ok();
                    let upstream = upstream_branch
                        .as_ref()
                        .and_then(|u| u.name().ok().flatten().map(|s| s.to_string()));

                    // Ahead/behind vs the upstream tip, when tracking one.
                    let (ahead, behind) = upstream_branch
                        .as_ref()
                        .and_then(|u| u.get().target())
                        .and_then(|up_oid| repo.graph_ahead_behind(oid, up_oid).ok())
                        .unwrap_or((0, 0));

                    branches.push(BranchInfo {
                        name: name.to_string(),
                        is_head,
                        is_remote: false,
                        upstream,
                        tip_oid: oid,
                        ahead,
                        behind,
                    });
                }
            }
        }

        // Remote branches
        for branch_result in repo.branches(Some(BranchType::Remote))? {
            let (branch, _) = branch_result?;
            if let Some(name) = branch.name()? {
                let reference = branch.get();
                if let Some(oid) = reference.target() {
                    branches.push(BranchInfo {
                        name: name.to_string(),
                        is_head: false,
                        is_remote: true,
                        upstream: None,
                        tip_oid: oid,
                        ahead: 0,
                        behind: 0,
                    });
                }
            }
        }

        sort_for_display(&mut branches);

        Ok(branches)
    }
}

/// Sort branches into a deterministic, stable badge/label order: the
/// checked-out HEAD branch first, then local branches alphabetically, then
/// remote branches alphabetically. This is the single source of truth for
/// branch badge order — `graph::build_graph` pushes branch names into
/// `oid_to_branches` in this order, so no other place (in particular, no UI
/// rendering code reacting to navigation/selection state) may reorder badges.
fn sort_for_display(branches: &mut [BranchInfo]) {
    branches.sort_by(|a, b| {
        b.is_head
            .cmp(&a.is_head)
            .then(a.is_remote.cmp(&b.is_remote))
            .then(a.name.cmp(&b.name))
    });
}

/// Names of the remote branches in `branches` that have no matching local
/// branch — the "remote-only" refs that the show/hide-remotes toggle targets.
///
/// A remote branch is considered *tracked* (and therefore kept visible, even
/// when remotes are hidden) when some local branch:
///   - declares it as its upstream, or
///   - shares its short name (`origin/main` ↔ local `main`), or
///   - points at the same commit.
///
/// In any of those cases a local branch already represents the same work, so
/// hiding remotes must not drop it from the graph. Everything else — refs that
/// live only on the remote — is returned here.
pub fn remote_only_branch_names(branches: &[BranchInfo]) -> HashSet<String> {
    let locals: Vec<&BranchInfo> = branches.iter().filter(|b| !b.is_remote).collect();

    branches
        .iter()
        .filter(|b| b.is_remote)
        .filter(|remote| {
            !locals.iter().any(|local| {
                local.upstream.as_deref() == Some(remote.name.as_str())
                    || local.short_name() == remote.short_name()
                    || local.tip_oid == remote.tip_oid
            })
        })
        .map(|b| b.name.clone())
        .collect()
}

/// Attribute an author (name) to each branch in `branches`.
///
/// A branch's author is the author of the **oldest commit unique to that
/// branch** — a commit reachable from the branch tip but from no *other*
/// branch tip. That commit is the earliest work that exists only on this
/// branch, so its author is the person who started the branch. When a branch
/// has no unique commits (its tip is shared with, or fully merged into,
/// another branch), the author of the **tip commit** is used instead.
///
/// Runs one revwalk per branch (push the tip, hide every other tip), so it is
/// O(branches × history). Intended to be computed lazily — e.g. when the
/// branch picker opens — not on every refresh.
///
/// Never panics: any git error degrades to the tip author, and a commit
/// without a readable author name degrades to an empty string.
pub fn branch_authors(repo: &Repository, branches: &[BranchInfo]) -> HashMap<String, String> {
    branches
        .iter()
        .map(|b| (b.name.clone(), branch_author(repo, b, branches).unwrap_or_default()))
        .collect()
}

/// Author name for a single branch, applying the oldest-unique-commit rule
/// with a tip-commit fallback. `None` only when even the tip commit can't be
/// read (treated as an empty author by the caller).
fn branch_author(repo: &Repository, branch: &BranchInfo, all: &[BranchInfo]) -> Option<String> {
    let oid = oldest_unique_commit(repo, branch, all).unwrap_or(branch.tip_oid);
    let commit = repo.find_commit(oid).ok()?;
    let name = commit.author().name().unwrap_or_default().to_string();
    Some(name)
}

/// OID of the oldest commit reachable from `branch.tip_oid` but from no other
/// branch tip in `all`. `None` when the walk can't be built or the branch
/// contributes no unique commits (tip shared or fully merged elsewhere).
fn oldest_unique_commit(repo: &Repository, branch: &BranchInfo, all: &[BranchInfo]) -> Option<Oid> {
    let mut walk = repo.revwalk().ok()?;
    // Oldest-first: the first commit the walk yields is the branch's earliest
    // own commit.
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME | git2::Sort::REVERSE)
        .ok()?;
    walk.push(branch.tip_oid).ok()?;
    // Hide every other branch's tip. If another branch shares this tip (or sits
    // ahead of it), the walk empties out — correctly yielding "no unique
    // commits" and falling back to the tip author.
    for other in all {
        if other.name != branch.name {
            let _ = walk.hide(other.tip_oid);
        }
    }
    walk.filter_map(Result::ok).next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(byte: u8) -> Oid {
        Oid::from_bytes(&[byte; 20]).unwrap()
    }

    fn local(name: &str, tip: Oid, upstream: Option<&str>) -> BranchInfo {
        BranchInfo {
            name: name.to_string(),
            is_head: false,
            is_remote: false,
            upstream: upstream.map(str::to_string),
            tip_oid: tip,
            ahead: 0,
            behind: 0,
        }
    }

    fn remote(name: &str, tip: Oid) -> BranchInfo {
        BranchInfo {
            name: name.to_string(),
            is_head: false,
            is_remote: true,
            upstream: None,
            tip_oid: tip,
            ahead: 0,
            behind: 0,
        }
    }

    #[test]
    fn remote_with_no_local_counterpart_is_remote_only() {
        let branches = vec![
            local("main", oid(1), None),
            remote("origin/agent-work", oid(2)),
        ];
        let names = remote_only_branch_names(&branches);
        assert_eq!(names.len(), 1);
        assert!(names.contains("origin/agent-work"));
    }

    #[test]
    fn remote_tracked_by_upstream_config_is_not_remote_only() {
        // Local `main` tracks `origin/main` but sits on a different commit
        // (ahead/behind). Same-name would also match, so also assert on a
        // differently-named upstream to isolate the upstream check.
        let branches = vec![
            local("feature", oid(1), Some("origin/renamed-on-remote")),
            remote("origin/renamed-on-remote", oid(2)),
        ];
        assert!(remote_only_branch_names(&branches).is_empty());
    }

    #[test]
    fn remote_sharing_short_name_with_local_is_not_remote_only() {
        // No upstream configured and tips differ, but the names line up.
        let branches = vec![
            local("main", oid(1), None),
            remote("origin/main", oid(2)),
        ];
        assert!(remote_only_branch_names(&branches).is_empty());
    }

    #[test]
    fn remote_sharing_tip_with_local_is_not_remote_only() {
        // Differently named, no upstream, but pointing at the same commit —
        // e.g. a just-pushed branch or `origin/HEAD` aliasing the default.
        let branches = vec![
            local("main", oid(7), None),
            remote("origin/HEAD", oid(7)),
        ];
        assert!(remote_only_branch_names(&branches).is_empty());
    }

    #[test]
    fn classifies_a_mixed_set() {
        let branches = vec![
            local("main", oid(1), Some("origin/main")),
            remote("origin/main", oid(1)),        // tracked (upstream + name + tip)
            remote("origin/dependabot", oid(2)),  // remote-only
            remote("origin/colleague", oid(3)),   // remote-only
            local("wip", oid(4), None),           // local-only, untouched
        ];
        let names = remote_only_branch_names(&branches);
        assert_eq!(names.len(), 2);
        assert!(names.contains("origin/dependabot"));
        assert!(names.contains("origin/colleague"));
    }

    // --- sort_for_display: badge order must be deterministic regardless of
    // insertion order (issue #50: badge order was flipping while navigating
    // because a later rendering step reordered by selection state instead of
    // the list carrying a stable order from the start). ---

    fn head_local(name: &str, tip: Oid) -> BranchInfo {
        BranchInfo {
            name: name.to_string(),
            is_head: true,
            is_remote: false,
            upstream: None,
            tip_oid: tip,
            ahead: 0,
            behind: 0,
        }
    }

    #[test]
    fn sort_for_display_orders_head_then_local_then_remote_alphabetically() {
        let mut branches = vec![
            remote("origin/zeta", oid(1)),
            local("zeta", oid(1), None),
            remote("origin/alpha", oid(1)),
            head_local("beta", oid(2)),
            local("alpha", oid(1), None),
        ];
        sort_for_display(&mut branches);
        let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["beta", "alpha", "zeta", "origin/alpha", "origin/zeta"],
            "HEAD first, then locals A-Z, then remotes A-Z"
        );
    }

    #[test]
    fn sort_for_display_is_deterministic_regardless_of_insertion_order() {
        // Same conceptual branch set, fed in every rotation of a fixed
        // ordering. The sorted result must be identical every time — no
        // HashMap/HashSet iteration or insertion-order leakage.
        let make = || {
            vec![
                remote("origin/mac", oid(1)),
                local("mac", oid(1), None),
                local("wip", oid(2), None),
                remote("origin/wip", oid(2)),
            ]
        };
        let baseline = {
            let mut b = make();
            sort_for_display(&mut b);
            b.iter().map(|x| x.name.clone()).collect::<Vec<_>>()
        };
        assert_eq!(baseline, vec!["mac", "wip", "origin/mac", "origin/wip"]);

        // Try every rotation of the insertion order and confirm the sorted
        // output never changes.
        let original = make();
        for start in 0..original.len() {
            let mut rotated: Vec<BranchInfo> = original
                .iter()
                .cloned()
                .cycle()
                .skip(start)
                .take(original.len())
                .collect();
            sort_for_display(&mut rotated);
            let names: Vec<String> = rotated.iter().map(|b| b.name.clone()).collect();
            assert_eq!(names, baseline, "insertion order must not affect sorted badge order");
        }
    }

    // --- branch_authors: exercised against real fixture repositories, since
    // authorship attribution needs a real object graph to walk. ---

    use git2::{Signature, Time};
    use tempfile::TempDir;

    /// Commit a single-file tree onto `refname` with the given author name,
    /// wall-clock `secs` (so walk order is deterministic) and parents. Returns
    /// the new commit OID. Pure object plumbing — no workdir/index churn.
    fn commit(
        repo: &Repository,
        refname: &str,
        author: &str,
        secs: i64,
        parents: &[Oid],
        content: &str,
    ) -> Oid {
        let blob = repo.blob(content.as_bytes()).unwrap();
        let mut tb = repo.treebuilder(None).unwrap();
        tb.insert("file.txt", blob, 0o100644).unwrap();
        let tree = repo.find_tree(tb.write().unwrap()).unwrap();
        let sig = Signature::new(author, "dev@example.com", &Time::new(secs, 0)).unwrap();
        let parent_commits: Vec<_> = parents.iter().map(|p| repo.find_commit(*p).unwrap()).collect();
        let parent_refs: Vec<&git2::Commit> = parent_commits.iter().collect();
        repo.commit(Some(refname), &sig, &sig, "msg", &tree, &parent_refs)
            .unwrap()
    }

    fn info(name: &str, tip: Oid) -> BranchInfo {
        local(name, tip, None)
    }

    fn info_remote(name: &str, tip: Oid) -> BranchInfo {
        remote(name, tip)
    }

    #[test]
    fn author_is_oldest_commit_unique_to_the_branch() {
        // main:    a <- b            (Root, Main Dev)
        // feature: a <- f1 <- f2     (Feat Dev, Feat Dev Jr)
        // `a` is shared. feature's oldest *unique* commit is f1 -> "Feat Dev",
        // not the newer f2 and not the shared root's author.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", "Root", 1000, &[], "a");
        let b = commit(&repo, "refs/heads/main", "Main Dev", 2000, &[a], "b");
        let f1 = commit(&repo, "refs/heads/feature", "Feat Dev", 3000, &[a], "f1");
        let _f2 = commit(&repo, "refs/heads/feature", "Feat Dev Jr", 4000, &[f1], "f2");

        let branches = vec![info("main", b), info("feature", _f2)];
        let authors = branch_authors(&repo, &branches);

        assert_eq!(authors.get("feature").map(String::as_str), Some("Feat Dev"));
        // main's only unique commit is b (a is shared with feature).
        assert_eq!(authors.get("main").map(String::as_str), Some("Main Dev"));
    }

    #[test]
    fn falls_back_to_tip_author_when_no_unique_commits() {
        // Two branches at the *same* tip: neither has a unique commit, so both
        // fall back to the tip commit's own author.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", "Root", 1000, &[], "a");
        let c = commit(&repo, "refs/heads/main", "Tip Author", 2000, &[a], "c");
        // `dup` points at the same commit as main.
        repo.reference("refs/heads/dup", c, true, "dup").unwrap();

        let branches = vec![info("main", c), info("dup", c)];
        let authors = branch_authors(&repo, &branches);

        assert_eq!(authors.get("dup").map(String::as_str), Some("Tip Author"));
        assert_eq!(authors.get("main").map(String::as_str), Some("Tip Author"));
    }

    #[test]
    fn merged_branch_with_no_exclusive_commits_uses_tip_author() {
        // main:  a <- b <- c
        // topic:  points at b, which is fully contained in main. topic has no
        // unique commits, so it uses b's author.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", "Root", 1000, &[], "a");
        let b = commit(&repo, "refs/heads/main", "Topic Author", 2000, &[a], "b");
        let c = commit(&repo, "refs/heads/main", "Main Dev", 3000, &[b], "c");
        repo.reference("refs/heads/topic", b, true, "topic").unwrap();

        let branches = vec![info("main", c), info("topic", b)];
        let authors = branch_authors(&repo, &branches);

        assert_eq!(authors.get("topic").map(String::as_str), Some("Topic Author"));
    }

    #[test]
    fn shared_history_is_not_attributed_to_the_other_branch() {
        // Two feature branches forking off a common root, each with its own
        // author. Neither should inherit the other's or the root's author.
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", "Root", 1000, &[], "a");
        let x = commit(&repo, "refs/heads/alice", "Alice", 2000, &[a], "x");
        let y = commit(&repo, "refs/heads/bob", "Bob", 3000, &[a], "y");

        let branches = vec![info("main", a), info("alice", x), info("bob", y)];
        let authors = branch_authors(&repo, &branches);

        assert_eq!(authors.get("alice").map(String::as_str), Some("Alice"));
        assert_eq!(authors.get("bob").map(String::as_str), Some("Bob"));
    }

    #[test]
    fn remote_ref_counts_as_another_tip() {
        // A remote ref sharing a local branch's tip must still be treated as
        // "another tip", so the local branch sees no unique commits and falls
        // back to its tip author rather than walking shared history.
        let dir: TempDir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let a = commit(&repo, "refs/heads/main", "Root", 1000, &[], "a");
        let c = commit(&repo, "refs/heads/main", "Local Tip", 2000, &[a], "c");
        repo.reference("refs/remotes/origin/main", c, true, "origin/main")
            .unwrap();

        let branches = vec![info("main", c), info_remote("origin/main", c)];
        let authors = branch_authors(&repo, &branches);

        // origin/main also has a unique-commit set of {} vs main -> tip author.
        assert_eq!(authors.get("main").map(String::as_str), Some("Local Tip"));
        assert_eq!(
            authors.get("origin/main").map(String::as_str),
            Some("Local Tip")
        );
    }
}
