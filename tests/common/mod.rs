//! Shared test harness for git integration tests.
//!
//! Each integration-test binary compiles its own copy of this module and only
//! uses a subset of the helpers, so unused-code warnings here are expected.
#![allow(dead_code)]
//!
//! Provides a parameterized [`init_repo`] that *always* configures
//! `user.name` / `user.email` (so shell-out `git commit`s don't fail on
//! unconfigured identity) plus small helpers for building and inspecting repo
//! state.

use std::fs;
use std::path::Path;
use std::process::Command;

use git2::{Oid, Repository, Signature};
use keifu::git::GitRepository;
use tempfile::TempDir;

/// What to seed a freshly-initialised repository with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seed {
    /// No commits — an unborn HEAD.
    Empty,
    /// A single initial commit adding `tracked.txt` with contents `"tracked\n"`.
    TrackedFile,
}

/// Initialise a temporary git repository.
///
/// `user.name` / `user.email` are always configured in the repo so that
/// operations that shell out to `git commit` don't fail on an unknown
/// identity. The repo is seeded according to `seed`.
pub fn init_repo(seed: Seed) -> (TempDir, GitRepository) {
    let tempdir = tempfile::tempdir().unwrap();
    Repository::init(tempdir.path()).unwrap();

    {
        let repo = Repository::open(tempdir.path()).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test User").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();

        if seed == Seed::TrackedFile {
            fs::write(tempdir.path().join("tracked.txt"), "tracked\n").unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("tracked.txt")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let signature = Signature::now("Test User", "test@example.com").unwrap();
            repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
                .unwrap();
        }
    }

    let repo = GitRepository::open(tempdir.path()).unwrap();
    (tempdir, repo)
}

/// Commit `contents` to `path` on top of the current HEAD (creating the root
/// commit when HEAD is unborn). Returns the new commit's OID.
pub fn commit_file(repo: &Repository, path: &str, contents: &str, message: &str) -> Oid {
    let workdir = repo.workdir().unwrap();
    let full_path = workdir.join(path);
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&full_path, contents).unwrap();

    let mut index = repo.index().unwrap();
    index.add_path(Path::new(path)).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let signature = Signature::now("Test User", "test@example.com").unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &signature, &signature, message, &tree, &parents)
        .unwrap()
}

/// Working-directory path of the repository, as the operations layer expects.
pub fn repo_path(git_repo: &GitRepository) -> &str {
    &git_repo.path
}

/// OID of the commit HEAD currently resolves to.
pub fn head_oid(repo: &Repository) -> Oid {
    repo.head().unwrap().peel_to_commit().unwrap().id()
}

/// Run a `git` CLI command for test *setup* (remote plumbing, stash inspection,
/// …). Panics on failure — setup is expected to succeed. Returns stdout.
pub fn git_cli(repo_path: &str, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_path)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// `git stash list` lines, newest (`stash@{0}`) first.
pub fn stash_list(repo_path: &str) -> Vec<String> {
    git_cli(repo_path, &["stash", "list"])
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Number of entries in the stash list.
pub fn stash_count(repo_path: &str) -> usize {
    stash_list(repo_path).len()
}

/// Create a bare repository and register it as `origin` of `repo_path`.
/// Returns the bare repo's `TempDir`, which the caller must keep alive.
pub fn add_bare_origin(repo_path: &str) -> TempDir {
    let origin = tempfile::tempdir().unwrap();
    let status = Command::new("git")
        .args(["init", "--bare"])
        .arg(origin.path())
        .output()
        .expect("failed to init bare origin");
    assert!(status.status.success(), "git init --bare failed");
    git_cli(
        repo_path,
        &["remote", "add", "origin", origin.path().to_str().unwrap()],
    );
    origin
}
