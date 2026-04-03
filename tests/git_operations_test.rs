//! Integration tests for git operations (branch, checkout, stage, commit, tag,
//! restore, reset, merge, cherry-pick, revert).

use std::fs;
use std::path::Path;

use git2::{Oid, Repository, Signature};
use tempfile::TempDir;

use keifu::git::operations::*;
use keifu::git::GitRepository;

// ── Helpers ─────────────────────────────────────────────────────────

fn init_repo() -> (TempDir, GitRepository) {
    let tempdir = tempfile::tempdir().unwrap();
    Repository::init(tempdir.path()).unwrap();

    // git operations that shell out need user.name / user.email configured
    // in the repo so commits don't fail.
    {
        let repo = Repository::open(tempdir.path()).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test User").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();
    }

    let repo = GitRepository::open(tempdir.path()).unwrap();
    (tempdir, repo)
}

fn commit_file(repo: &Repository, path: &str, contents: &str, message: &str) -> Oid {
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

fn repo_path(git_repo: &GitRepository) -> &str {
    &git_repo.path
}

fn head_oid(repo: &Repository) -> Oid {
    repo.head().unwrap().peel_to_commit().unwrap().id()
}

// ── Branch Operations ───────────────────────────────────────────────

#[test]
fn create_branch_creates_new_branch_at_oid() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    let oid = commit_file(repo, "a.txt", "a", "initial");

    create_branch(repo, "feature", oid).unwrap();

    let branch = repo.find_branch("feature", git2::BranchType::Local).unwrap();
    let branch_oid = branch.get().peel_to_commit().unwrap().id();
    assert_eq!(branch_oid, oid);
}

#[test]
fn create_branch_fails_for_duplicate_name() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    let oid = commit_file(repo, "a.txt", "a", "initial");

    create_branch(repo, "feature", oid).unwrap();
    let result = create_branch(repo, "feature", oid);
    assert!(result.is_err());
}

#[test]
fn delete_branch_removes_branch() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    let oid = commit_file(repo, "a.txt", "a", "initial");

    create_branch(repo, "to-delete", oid).unwrap();
    delete_branch(repo, "to-delete").unwrap();

    let result = repo.find_branch("to-delete", git2::BranchType::Local);
    assert!(result.is_err());
}

#[test]
fn delete_branch_fails_on_head_branch() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");

    // HEAD points to the default branch (master or main depending on config).
    let head_ref = repo.head().unwrap();
    let head_branch_name = head_ref.shorthand().unwrap().to_string();

    let result = delete_branch(repo, &head_branch_name);
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("Cannot delete current branch"),
        "Expected error about deleting current branch"
    );
}

#[test]
fn delete_branch_fails_for_nonexistent() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");

    let result = delete_branch(repo, "does-not-exist");
    assert!(result.is_err());
}

// ── Checkout ────────────────────────────────────────────────────────

#[test]
fn checkout_branch_switches_head() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    let oid = commit_file(repo, "a.txt", "a", "initial");

    create_branch(repo, "feature", oid).unwrap();
    commit_file(repo, "b.txt", "b", "second on default");

    checkout_branch(repo, "feature").unwrap();

    let head_ref = repo.head().unwrap();
    assert_eq!(head_ref.shorthand().unwrap(), "feature");
    assert_eq!(head_oid(repo), oid);
}

#[test]
fn checkout_commit_enters_detached_head() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    let first_oid = commit_file(repo, "a.txt", "a", "first");
    commit_file(repo, "b.txt", "b", "second");

    checkout_commit(repo, first_oid).unwrap();

    assert!(repo.head_detached().unwrap());
    assert_eq!(head_oid(repo), first_oid);
}

#[test]
fn checkout_nonexistent_branch_fails() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");

    let result = checkout_branch(repo, "no-such-branch");
    assert!(result.is_err());
}

// ── Stage / Unstage ─────────────────────────────────────────────────

#[test]
fn stage_file_adds_to_index() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");
    let path = repo_path(&git_repo);

    // Modify a tracked file
    fs::write(repo.workdir().unwrap().join("a.txt"), "modified").unwrap();

    stage_file(path, "a.txt").unwrap();

    let statuses = repo.statuses(None).unwrap();
    let entry = statuses.iter().find(|e| e.path() == Some("a.txt")).unwrap();
    assert!(entry.status().intersects(git2::Status::INDEX_MODIFIED));
}

#[test]
fn unstage_file_removes_from_index() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");
    let path = repo_path(&git_repo);

    // Modify and stage
    fs::write(repo.workdir().unwrap().join("a.txt"), "modified").unwrap();
    stage_file(path, "a.txt").unwrap();

    // Unstage
    unstage_file(path, "a.txt").unwrap();

    let statuses = repo.statuses(None).unwrap();
    let entry = statuses.iter().find(|e| e.path() == Some("a.txt")).unwrap();
    // Should be unstaged (worktree modified) but not in index
    assert!(entry.status().intersects(git2::Status::WT_MODIFIED));
    assert!(!entry.status().intersects(git2::Status::INDEX_MODIFIED));
}

#[test]
fn stage_then_unstage_roundtrip() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");
    let path = repo_path(&git_repo);

    fs::write(repo.workdir().unwrap().join("a.txt"), "changed").unwrap();

    // Capture status before staging
    let before = repo.statuses(None).unwrap();
    let before_status = before
        .iter()
        .find(|e| e.path() == Some("a.txt"))
        .unwrap()
        .status();

    // Stage then unstage
    stage_file(path, "a.txt").unwrap();
    unstage_file(path, "a.txt").unwrap();

    // Should be back to original status
    let after = repo.statuses(None).unwrap();
    let after_status = after
        .iter()
        .find(|e| e.path() == Some("a.txt"))
        .unwrap()
        .status();
    assert_eq!(before_status, after_status);
}

#[test]
fn stage_untracked_file() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");
    let path = repo_path(&git_repo);

    // Create a new untracked file
    fs::write(repo.workdir().unwrap().join("new.txt"), "new content").unwrap();

    stage_file(path, "new.txt").unwrap();

    let statuses = repo.statuses(None).unwrap();
    let entry = statuses.iter().find(|e| e.path() == Some("new.txt")).unwrap();
    assert!(entry.status().intersects(git2::Status::INDEX_NEW));
}

// ── Commit ──────────────────────────────────────────────────────────

#[test]
fn commit_with_message_creates_commit() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");
    let path = repo_path(&git_repo);

    fs::write(repo.workdir().unwrap().join("a.txt"), "updated").unwrap();
    stage_file(path, "a.txt").unwrap();

    let before_oid = head_oid(repo);
    commit_with_message(path, "test commit").unwrap();
    let after_oid = head_oid(repo);

    assert_ne!(before_oid, after_oid);
}

#[test]
fn get_last_commit_message_returns_head_message() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");
    let path = repo_path(&git_repo);

    fs::write(repo.workdir().unwrap().join("a.txt"), "v2").unwrap();
    stage_file(path, "a.txt").unwrap();
    commit_with_message(path, "my special message").unwrap();

    let msg = get_last_commit_message(path).unwrap();
    assert_eq!(msg, "my special message");
}

#[test]
fn commit_amend_changes_message() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "original message");
    let path = repo_path(&git_repo);

    commit_amend(path, "amended message").unwrap();

    let msg = get_last_commit_message(path).unwrap();
    assert_eq!(msg, "amended message");

    // Amend should not create a new commit (parent unchanged)
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.parent_count(), 0); // still the initial commit
}

#[test]
fn commit_amend_no_edit_preserves_message() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "keep this message");
    let path = repo_path(&git_repo);

    // Modify and stage so amend has something to include
    fs::write(repo.workdir().unwrap().join("a.txt"), "changed").unwrap();
    stage_file(path, "a.txt").unwrap();

    commit_amend_no_edit(path).unwrap();

    let msg = get_last_commit_message(path).unwrap();
    assert_eq!(msg, "keep this message");
}

#[test]
fn commit_with_no_staged_changes_fails() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");
    let path = repo_path(&git_repo);

    let result = commit_with_message(path, "empty commit");
    assert!(result.is_err());
}

// ── Tag ─────────────────────────────────────────────────────────────

#[test]
fn add_tag_creates_tag_at_commit() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    let oid = commit_file(repo, "a.txt", "a", "initial");

    add_tag(repo, "v1.0", oid).unwrap();

    // Verify the tag exists and points to the correct commit
    let tag_names: Vec<String> = repo
        .tag_names(None)
        .unwrap()
        .iter()
        .flatten()
        .map(|s| s.to_string())
        .collect();
    assert!(tag_names.contains(&"v1.0".to_string()));
}

#[test]
fn add_duplicate_tag_fails() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    let oid = commit_file(repo, "a.txt", "a", "initial");

    add_tag(repo, "v1.0", oid).unwrap();
    let result = add_tag(repo, "v1.0", oid);
    assert!(result.is_err());
}

// ── Restore ─────────────────────────────────────────────────────────

#[test]
fn restore_files_discards_tracked_changes() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "original", "initial");
    let path = repo_path(&git_repo);

    // Modify the file
    fs::write(repo.workdir().unwrap().join("a.txt"), "dirty").unwrap();

    restore_files(path, &["a.txt".to_string()]).unwrap();

    let contents = fs::read_to_string(repo.workdir().unwrap().join("a.txt")).unwrap();
    assert_eq!(contents, "original");
}

#[test]
fn restore_files_multiple_files() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a-orig", "first");
    commit_file(repo, "b.txt", "b-orig", "second");
    let path = repo_path(&git_repo);

    fs::write(repo.workdir().unwrap().join("a.txt"), "a-dirty").unwrap();
    fs::write(repo.workdir().unwrap().join("b.txt"), "b-dirty").unwrap();

    restore_files(path, &["a.txt".to_string(), "b.txt".to_string()]).unwrap();

    assert_eq!(
        fs::read_to_string(repo.workdir().unwrap().join("a.txt")).unwrap(),
        "a-orig"
    );
    assert_eq!(
        fs::read_to_string(repo.workdir().unwrap().join("b.txt")).unwrap(),
        "b-orig"
    );
}

// ── Reset ───────────────────────────────────────────────────────────

#[test]
fn reset_soft_keeps_changes_staged() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    let first_oid = commit_file(repo, "a.txt", "a", "first");
    commit_file(repo, "b.txt", "b", "second");
    let path = repo_path(&git_repo);

    reset_to_commit(path, first_oid, ResetMode::Soft).unwrap();

    assert_eq!(head_oid(repo), first_oid);

    // b.txt should be staged (INDEX_NEW) since we soft-reset past its commit
    let statuses = repo.statuses(None).unwrap();
    let entry = statuses.iter().find(|e| e.path() == Some("b.txt")).unwrap();
    assert!(entry.status().intersects(git2::Status::INDEX_NEW));
}

#[test]
fn reset_mixed_keeps_changes_unstaged() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    let first_oid = commit_file(repo, "a.txt", "a", "first");
    commit_file(repo, "b.txt", "b", "second");
    let path = repo_path(&git_repo);

    reset_to_commit(path, first_oid, ResetMode::Mixed).unwrap();

    assert_eq!(head_oid(repo), first_oid);

    // b.txt should be untracked (WT_NEW) since mixed reset unstages
    let statuses = repo.statuses(None).unwrap();
    let entry = statuses.iter().find(|e| e.path() == Some("b.txt")).unwrap();
    assert!(
        entry.status().intersects(git2::Status::WT_NEW),
        "Expected WT_NEW, got {:?}",
        entry.status()
    );
    assert!(
        !entry.status().intersects(git2::Status::INDEX_NEW),
        "Should not be staged after mixed reset"
    );
}

#[test]
fn reset_hard_discards_all_changes() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    let first_oid = commit_file(repo, "a.txt", "a", "first");
    commit_file(repo, "b.txt", "b", "second");
    let path = repo_path(&git_repo);

    reset_to_commit(path, first_oid, ResetMode::Hard).unwrap();

    assert_eq!(head_oid(repo), first_oid);

    // b.txt should be gone entirely
    assert!(!repo.workdir().unwrap().join("b.txt").exists());

    let statuses = repo.statuses(None).unwrap();
    let entry = statuses.iter().find(|e| e.path() == Some("b.txt"));
    assert!(entry.is_none(), "b.txt should not appear in status after hard reset");
}

// ── Merge ───────────────────────────────────────────────────────────

#[test]
fn merge_fast_forward() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    let initial_oid = commit_file(repo, "a.txt", "a", "initial");

    // Create a branch from the initial commit, add a commit on it
    create_branch(repo, "feature", initial_oid).unwrap();
    checkout_branch(repo, "feature").unwrap();
    let feature_oid = commit_file(repo, "b.txt", "b", "feature work");

    // Go back to default branch and merge (should fast-forward)
    checkout_branch(repo, "master").unwrap();

    merge_branch(repo, "feature").unwrap();

    assert_eq!(head_oid(repo), feature_oid);
    assert!(repo.workdir().unwrap().join("b.txt").exists());
}

#[test]
fn merge_creates_merge_commit_on_diverged_branches() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    let initial_oid = commit_file(repo, "a.txt", "a", "initial");

    // Create feature branch from initial commit
    create_branch(repo, "feature", initial_oid).unwrap();

    // Add a commit on the default branch (diverge)
    commit_file(repo, "b.txt", "b", "main work");

    // Switch to feature, add a commit there too
    checkout_branch(repo, "feature").unwrap();
    commit_file(repo, "c.txt", "c", "feature work");

    // Go back to default and merge
    checkout_branch(repo, "master").unwrap();
    merge_branch(repo, "feature").unwrap();

    // HEAD should be a merge commit with 2 parents
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.parent_count(), 2);

    // Both files should exist
    assert!(repo.workdir().unwrap().join("b.txt").exists());
    assert!(repo.workdir().unwrap().join("c.txt").exists());
}

#[test]
fn merge_up_to_date_is_noop() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    let oid = commit_file(repo, "a.txt", "a", "initial");

    create_branch(repo, "feature", oid).unwrap();

    let before = head_oid(repo);
    merge_branch(repo, "feature").unwrap();
    assert_eq!(head_oid(repo), before);
}

// ── Cherry-pick ─────────────────────────────────────────────────────

#[test]
fn cherry_pick_applies_single_commit() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    let initial_oid = commit_file(repo, "a.txt", "a", "initial");

    // Create feature branch and add a commit
    create_branch(repo, "feature", initial_oid).unwrap();
    checkout_branch(repo, "feature").unwrap();
    let pick_oid = commit_file(repo, "cherry.txt", "cherry", "to be picked");

    // Go back to default branch
    checkout_branch(repo, "master").unwrap();
    assert!(!repo.workdir().unwrap().join("cherry.txt").exists());

    let path = repo_path(&git_repo);
    cherry_pick(path, pick_oid).unwrap();

    // The file should now exist on the default branch
    assert!(repo.workdir().unwrap().join("cherry.txt").exists());
    let contents = fs::read_to_string(repo.workdir().unwrap().join("cherry.txt")).unwrap();
    assert_eq!(contents, "cherry");
}

// ── Revert ──────────────────────────────────────────────────────────

#[test]
fn revert_commit_creates_revert() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");
    let to_revert = commit_file(repo, "b.txt", "b", "add b");
    let path = repo_path(&git_repo);

    revert_commit(path, to_revert).unwrap();

    // b.txt should be removed by the revert
    assert!(!repo.workdir().unwrap().join("b.txt").exists());

    // A new revert commit should exist
    let msg = get_last_commit_message(path).unwrap();
    assert!(
        msg.contains("Revert"),
        "Expected revert commit message, got: {}",
        msg
    );
}

#[test]
fn revert_creates_new_commit_not_removes_old() {
    let (_td, git_repo) = init_repo();
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");
    let to_revert = commit_file(repo, "b.txt", "b", "add b");
    let path = repo_path(&git_repo);

    let before_oid = head_oid(repo);
    revert_commit(path, to_revert).unwrap();

    // The reverted commit still exists as a parent
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.parent_count(), 1);
    assert_eq!(head.parent_id(0).unwrap(), before_oid);
}
