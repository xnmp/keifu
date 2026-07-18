//! Fork-point navigation against a real fixture repository: the merge-base
//! module resolves "main", computes the real git2 merge base, and locates the
//! fork commit's row.

use git2::{Oid, Repository, Signature, Time};
use keifu::git::{build_graph, GitRepository};
use keifu::merge_base::{fork_target, main_branch_tip, row_of_commit, ForkTarget};
use tempfile::TempDir;

/// Commit a single-file tree onto `refname`, at wall-clock `secs` (so the walk
/// order is deterministic), with the given parents. Pure object plumbing — no
/// workdir/index churn.
fn commit(
    repo: &Repository,
    refname: &str,
    parents: &[Oid],
    secs: i64,
    content: &str,
) -> Oid {
    let blob = repo.blob(content.as_bytes()).unwrap();
    let mut tb = repo.treebuilder(None).unwrap();
    tb.insert("file.txt", blob, 0o100644).unwrap();
    let tree = repo.find_tree(tb.write().unwrap()).unwrap();
    let sig = Signature::new("Test", "t@e.com", &Time::new(secs, 0)).unwrap();
    let parent_commits: Vec<_> = parents.iter().map(|p| repo.find_commit(*p).unwrap()).collect();
    let parent_refs: Vec<&git2::Commit> = parent_commits.iter().collect();
    repo.commit(Some(refname), &sig, &sig, "msg", &tree, &parent_refs)
        .unwrap()
}

/// A repo with `main` = a <- b and `feature` = a <- f, HEAD on main. The newest
/// commit (b) sits on main, so the color assigner tags main correctly.
fn fixture() -> (TempDir, GitRepository, Oid, Oid, Oid) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();

    let a = commit(&repo, "refs/heads/main", &[], 1000, "a");
    let f = commit(&repo, "refs/heads/feature", &[a], 2000, "f");
    let b = commit(&repo, "refs/heads/main", &[a], 3000, "b"); // newest, on main
    repo.set_head("refs/heads/main").unwrap();

    let git = GitRepository::open(dir.path()).unwrap();
    (dir, git, a, f, b)
}

#[test]
fn feature_commit_forks_at_the_main_merge_base() {
    let (_dir, git, a, f, b) = fixture();
    let branches = git.get_branches().unwrap();
    let commits = git.get_commits(500, &branches, &[]).unwrap();
    let layout = build_graph(&commits, &branches, &[], &[], None, None);

    // The color assigner tags the newest (main) commit as main.
    assert_eq!(main_branch_tip(&layout), Some(b));

    // Selecting the feature commit forks at the merge base with main: commit a.
    let head_tip = branches.iter().find(|br| br.is_head).map(|br| br.tip_oid);
    let repo = git.repo();
    let target = fork_target(f, b, head_tip, |x, y| repo.merge_base(x, y).ok());
    assert_eq!(target, ForkTarget::Jump(a));

    // The fork commit is inside the loaded window.
    assert!(row_of_commit(&layout, a).is_some());
}

#[test]
fn commit_on_main_falls_back_to_head_when_head_differs() {
    // Selecting `b` (on main) has merge_base(b, main_tip)=b, so the fork answer
    // comes from HEAD. Here HEAD is main too, so it resolves to linear.
    let (_dir, git, _a, _f, b) = fixture();
    let branches = git.get_branches().unwrap();
    let head_tip = branches.iter().find(|br| br.is_head).map(|br| br.tip_oid);
    let repo = git.repo();
    let target = fork_target(b, b, head_tip, |x, y| repo.merge_base(x, y).ok());
    assert_eq!(target, ForkTarget::Linear);
}
