//! Integration tests for pull, multi-remote fetch/push, publish (push -u),
//! remote enumeration, remote-branch deletion, and prune.
//!
//! The network functions run on background threads in the app; here we exercise
//! the synchronous operations layer directly against local bare remotes, which
//! is deterministic and fast.

use std::path::Path;

use keifu::git::operations::{
    create_branch, delete_remote_branch, fetch_remote, is_divergent_pull_error, prune_remote, pull,
    push_current, push_set_upstream, OpOutcome, PullMode,
};
use keifu::git::{GitRepository, OperationState};

mod common;
use common::{
    add_bare_origin, add_bare_remote, clone_from, commit_file, current_branch, git_cli, head_oid,
    init_repo, Seed,
};

/// Repo with one commit on its default branch, a bare `origin`, and the branch
/// pushed with upstream tracking. Returns (repo tempdir, GitRepository, origin
/// tempdir, branch name) — keep the tempdirs alive for the test's duration.
fn repo_with_tracked_origin() -> (tempfile::TempDir, GitRepository, tempfile::TempDir, String) {
    let (td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let branch = current_branch(git_repo.repo());
    let origin = add_bare_origin(&path);
    git_cli(&path, &["push", "-u", "origin", &branch]);
    (td, git_repo, origin, branch)
}

/// Advance `origin` from a fresh clone: commit `file`=`contents` on `branch` and
/// push. Simulates another collaborator moving the remote forward.
fn advance_origin(origin: &Path, branch: &str, file: &str, contents: &str, msg: &str) {
    let work = clone_from(origin);
    let wp = work.path().to_str().unwrap();
    git_cli(wp, &["checkout", "-B", branch, &format!("origin/{branch}")]);
    std::fs::write(work.path().join(file), contents).unwrap();
    git_cli(wp, &["add", file]);
    git_cli(wp, &["commit", "-m", msg]);
    git_cli(wp, &["push", "origin", branch]);
}

// ── Pull ────────────────────────────────────────────────────────────

#[test]
fn pull_fast_forward_advances_head_and_worktree() {
    let (_td, git_repo, origin, branch) = repo_with_tracked_origin();
    let path = git_repo.path.clone();

    advance_origin(origin.path(), &branch, "remote.txt", "from remote\n", "remote change");

    // A fast-forward succeeds even under the strict --ff-only default.
    let outcome = pull(&path, None, None, PullMode::FfOnly, None).unwrap();
    assert_eq!(outcome, OpOutcome::Completed);

    // The worktree gained the remote file and HEAD is the remote commit.
    assert_eq!(
        std::fs::read_to_string(Path::new(&path).join("remote.txt")).unwrap(),
        "from remote\n"
    );
    assert_eq!(git_cli(&path, &["log", "-1", "--format=%s"]).trim(), "remote change");
    assert_eq!(git_repo.operation_state(), OperationState::Clean);
}

#[test]
fn pull_divergent_remote_creates_merge_commit() {
    let (_td, git_repo, origin, branch) = repo_with_tracked_origin();
    let path = git_repo.path.clone();

    // Remote and local advance on disjoint files → divergence, no conflict.
    advance_origin(origin.path(), &branch, "fileR.txt", "R\n", "remote work");
    commit_file(git_repo.repo(), "fileL.txt", "L\n", "local work");
    let before = head_oid(git_repo.repo());

    // The merge strategy is explicit (--no-rebase), independent of git config.
    let outcome = pull(&path, None, None, PullMode::Merge, None).unwrap();
    assert_eq!(outcome, OpOutcome::Completed);

    // HEAD is a fresh merge commit (two parents) with both sides' files.
    let parents = git_cli(&path, &["rev-list", "--parents", "-n", "1", "HEAD"]);
    let parent_count = parents.split_whitespace().count() - 1;
    assert_eq!(parent_count, 2, "pull should create a merge commit: {parents:?}");
    assert_ne!(head_oid(git_repo.repo()), before);
    assert!(Path::new(&path).join("fileR.txt").exists());
    assert!(Path::new(&path).join("fileL.txt").exists());
    assert_eq!(git_repo.operation_state(), OperationState::Clean);
}

#[test]
fn pull_conflict_leaves_repo_in_merge_state() {
    let (_td, git_repo, origin, branch) = repo_with_tracked_origin();
    let path = git_repo.path.clone();

    // Shared base line both sides edit differently.
    commit_file(git_repo.repo(), "conflict.txt", "base\n", "add conflict base");
    git_cli(&path, &["push", "origin", &branch]);
    advance_origin(origin.path(), &branch, "conflict.txt", "remote\n", "remote edit");

    // Local edits the same line the other way and commits.
    std::fs::write(Path::new(&path).join("conflict.txt"), "local\n").unwrap();
    git_cli(&path, &["commit", "-am", "local edit"]);

    // A conflicting merge-pull is a typed outcome, not an error.
    let outcome = pull(&path, None, None, PullMode::Merge, None).unwrap();
    assert!(
        matches!(outcome, OpOutcome::Conflicts { count } if count >= 1),
        "expected Conflicts, got {outcome:?}"
    );
    // The repo is left mid-merge so the guided resolve flow can take over.
    assert_eq!(git_repo.operation_state(), OperationState::Merge);
}

#[test]
fn pull_ff_only_fails_on_divergence_with_a_recognized_error() {
    let (_td, git_repo, origin, branch) = repo_with_tracked_origin();
    let path = git_repo.path.clone();

    // Divergent history (disjoint files): a fast-forward is impossible.
    advance_origin(origin.path(), &branch, "fileR.txt", "R\n", "remote work");
    commit_file(git_repo.repo(), "fileL.txt", "L\n", "local work");

    // The default --ff-only pull fails loudly, and the failure is classified as
    // divergence (which drives the merge/rebase prompt) rather than a hard error.
    let err = pull(&path, None, None, PullMode::FfOnly, None).unwrap_err().to_string();
    assert!(
        is_divergent_pull_error(&err),
        "expected a divergence error, got: {err}"
    );
    // No merge was started — the repo is untouched.
    assert_eq!(git_repo.operation_state(), OperationState::Clean);
}

// ── Push / publish ──────────────────────────────────────────────────

#[test]
fn push_sets_upstream_when_absent() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let branch = current_branch(git_repo.repo());
    let _origin = add_bare_origin(&path);

    // Precondition: no upstream yet.
    assert!(git_repo
        .repo()
        .find_branch(&branch, git2::BranchType::Local)
        .unwrap()
        .upstream()
        .is_err());

    push_set_upstream(&path, "origin", &branch, None).unwrap();

    // @{u} now resolves to origin/<branch>.
    let up = git_cli(&path, &["rev-parse", "--abbrev-ref", &format!("{branch}@{{u}}")]);
    assert_eq!(up.trim(), format!("origin/{branch}"));
}

#[test]
fn push_current_pushes_to_configured_upstream() {
    let (_td, git_repo, _origin, branch) = repo_with_tracked_origin();
    let path = git_repo.path.clone();

    commit_file(git_repo.repo(), "more.txt", "more\n", "more work");
    push_current(&path, None).unwrap();

    // The remote-tracking ref advanced to the new HEAD.
    let head = git_cli(&path, &["rev-parse", "HEAD"]);
    let remote_ref = git_cli(&path, &["rev-parse", &format!("origin/{branch}")]);
    assert_eq!(head.trim(), remote_ref.trim());
}

#[test]
fn push_publishes_to_explicit_second_remote() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let branch = current_branch(git_repo.repo());
    let _origin = add_bare_origin(&path);
    let _backup = add_bare_remote(&path, "backup");

    push_set_upstream(&path, "backup", &branch, None).unwrap();

    // Upstream points at backup, and backup received the branch tip.
    let up = git_cli(&path, &["rev-parse", "--abbrev-ref", &format!("{branch}@{{u}}")]);
    assert_eq!(up.trim(), format!("backup/{branch}"));
    let head = git_cli(&path, &["rev-parse", "HEAD"]);
    let backup_ref = git_cli(&path, &["rev-parse", &format!("backup/{branch}")]);
    assert_eq!(head.trim(), backup_ref.trim());
}

// ── Multi-remote enumeration ────────────────────────────────────────

#[test]
fn remotes_enumerates_configured_remotes() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();

    // Fresh repo: no remotes.
    assert!(GitRepository::open(&path).unwrap().remotes().is_empty());

    let _origin = add_bare_origin(&path);
    let _backup = add_bare_remote(&path, "backup");

    let mut remotes = GitRepository::open(&path).unwrap().remotes();
    remotes.sort();
    assert_eq!(remotes, vec!["backup".to_string(), "origin".to_string()]);
}

// ── Delete remote branch / prune ────────────────────────────────────

#[test]
fn delete_remote_branch_removes_it_from_remote() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let c0 = head_oid(git_repo.repo());
    let _origin = add_bare_origin(&path);

    create_branch(git_repo.repo(), "feature", c0).unwrap();
    git_cli(&path, &["push", "origin", "feature"]);
    assert!(!git_cli(&path, &["ls-remote", "--heads", "origin", "feature"]).trim().is_empty());

    delete_remote_branch(&path, "origin", "feature").unwrap();

    assert!(
        git_cli(&path, &["ls-remote", "--heads", "origin", "feature"]).trim().is_empty(),
        "feature should be gone from origin"
    );
}

#[test]
fn prune_removes_stale_remote_tracking_ref() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let c0 = head_oid(git_repo.repo());
    let origin = add_bare_origin(&path);

    // Publish feature and fetch so a local origin/feature tracking ref exists.
    create_branch(git_repo.repo(), "feature", c0).unwrap();
    git_cli(&path, &["push", "origin", "feature"]);
    fetch_remote(&path, "origin", None).unwrap();
    assert!(git_cli(&path, &["branch", "-r"]).contains("origin/feature"));

    // Delete feature directly in the bare origin — a plain fetch won't drop the
    // now-stale local tracking ref, but prune will.
    git_cli(
        origin.path().to_str().unwrap(),
        &["update-ref", "-d", "refs/heads/feature"],
    );

    prune_remote(&path, "origin").unwrap();

    assert!(
        !git_cli(&path, &["branch", "-r"]).contains("origin/feature"),
        "stale origin/feature should be pruned"
    );
}
