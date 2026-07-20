//! Integration tests for trunk detection (issue #1): the graph pins the
//! detected trunk branch to the leftmost lane. This exercises the detection
//! cascade in `GitRepository::detect_trunk_tip` against real repositories.

use git2::{Repository, Signature};
use keifu::git::GitRepository;

/// A committer identity so commits succeed.
fn sig() -> Signature<'static> {
    Signature::now("Test User", "test@example.com").unwrap()
}

/// Commit an empty tree onto `refname`, returning the new oid. Parent is the
/// current tip of `refname` when it exists.
fn commit_on(repo: &Repository, refname: &str, message: &str) -> git2::Oid {
    let tree = {
        let builder = repo.treebuilder(None).unwrap();
        repo.find_tree(builder.write().unwrap()).unwrap()
    };
    let s = sig();
    let parent = repo.find_reference(refname).ok().and_then(|r| r.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some(refname), &s, &s, message, &tree, &parents).unwrap()
}

#[test]
fn detects_trunk_from_origin_head_symref() {
    // origin/HEAD -> origin/main is authoritative even when HEAD is elsewhere
    // and a differently-named local branch exists.
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let main_tip = commit_on(&repo, "refs/remotes/origin/main", "m");
    // A local feature branch that is NOT the trunk.
    commit_on(&repo, "refs/heads/feature", "f");
    repo.set_head("refs/heads/feature").unwrap();
    // origin/HEAD is a symbolic ref to origin/main (what `git remote set-head` writes).
    repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        true,
        "set-head",
    )
    .unwrap();
    drop(repo);

    let git_repo = GitRepository::open(dir.path()).unwrap();
    let branches = git_repo.get_branches().unwrap();
    assert_eq!(
        git_repo.detect_trunk_tip(&branches),
        Some(main_tip),
        "origin/HEAD symref selects origin/main's tip as trunk"
    );
}

#[test]
fn falls_back_to_name_heuristic_without_symref() {
    // No origin/HEAD: prefer a local `main` by name, even though HEAD is `dev`.
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let main_tip = commit_on(&repo, "refs/heads/main", "m");
    commit_on(&repo, "refs/heads/dev", "d");
    repo.set_head("refs/heads/dev").unwrap();
    drop(repo);

    let git_repo = GitRepository::open(dir.path()).unwrap();
    let branches = git_repo.get_branches().unwrap();
    assert_eq!(
        git_repo.detect_trunk_tip(&branches),
        Some(main_tip),
        "name heuristic prefers `main`"
    );
}

#[test]
fn master_used_when_no_main() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let master_tip = commit_on(&repo, "refs/heads/master", "m");
    repo.set_head("refs/heads/master").unwrap();
    drop(repo);

    let git_repo = GitRepository::open(dir.path()).unwrap();
    let branches = git_repo.get_branches().unwrap();
    assert_eq!(git_repo.detect_trunk_tip(&branches), Some(master_tip));
}

#[test]
fn none_when_no_trunk_like_branch() {
    // Only oddly-named branches: no symref, no main/master/develop/trunk.
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    commit_on(&repo, "refs/heads/wip-experiment", "w");
    repo.set_head("refs/heads/wip-experiment").unwrap();
    drop(repo);

    let git_repo = GitRepository::open(dir.path()).unwrap();
    let branches = git_repo.get_branches().unwrap();
    assert_eq!(
        git_repo.detect_trunk_tip(&branches),
        None,
        "no trunk identified -> None (graph stays unpinned)"
    );
}
