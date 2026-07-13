//! Integration tests for git operations (branch, checkout, stage, commit, tag,
//! restore, reset, merge, rebase, stash, cherry-pick, revert).

use std::fs;
use std::path::Path;

use git2::Oid;

use keifu::git::operations::*;

mod common;
use common::{
    add_bare_origin, commit_file, git_cli, head_oid, init_repo, repo_path, stash_count,
    stash_list, Seed,
};

// ── Branch Operations ───────────────────────────────────────────────

#[test]
fn create_branch_creates_new_branch_at_oid() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let oid = commit_file(repo, "a.txt", "a", "initial");

    create_branch(repo, "feature", oid).unwrap();

    let branch = repo.find_branch("feature", git2::BranchType::Local).unwrap();
    let branch_oid = branch.get().peel_to_commit().unwrap().id();
    assert_eq!(branch_oid, oid);
}

#[test]
fn create_branch_fails_for_duplicate_name() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let oid = commit_file(repo, "a.txt", "a", "initial");

    create_branch(repo, "feature", oid).unwrap();
    let result = create_branch(repo, "feature", oid);
    assert!(result.is_err());
}

#[test]
fn delete_branch_removes_branch() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let oid = commit_file(repo, "a.txt", "a", "initial");

    create_branch(repo, "to-delete", oid).unwrap();
    delete_branch(repo, "to-delete").unwrap();

    let result = repo.find_branch("to-delete", git2::BranchType::Local);
    assert!(result.is_err());
}

#[test]
fn delete_branch_fails_on_head_branch() {
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");

    let result = delete_branch(repo, "does-not-exist");
    assert!(result.is_err());
}

// ── Checkout ────────────────────────────────────────────────────────

#[test]
fn checkout_branch_switches_head() {
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let first_oid = commit_file(repo, "a.txt", "a", "first");
    commit_file(repo, "b.txt", "b", "second");

    checkout_commit(repo, first_oid).unwrap();

    assert!(repo.head_detached().unwrap());
    assert_eq!(head_oid(repo), first_oid);
}

#[test]
fn checkout_nonexistent_branch_fails() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");

    let result = checkout_branch(repo, "no-such-branch");
    assert!(result.is_err());
}

// ── Stage / Unstage ─────────────────────────────────────────────────

#[test]
fn stage_file_adds_to_index() {
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "keep this message");
    let path = repo_path(&git_repo);

    // Modify and stage so amend has something to include
    fs::write(repo.workdir().unwrap().join("a.txt"), "changed").unwrap();
    stage_file(path, "a.txt").unwrap();

    commit_amend_no_edit(path).unwrap();

    // Message is preserved.
    let msg = get_last_commit_message(path).unwrap();
    assert_eq!(msg, "keep this message");

    // The amend folded the staged change into HEAD's tree.
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    let entry = tree.get_name("a.txt").unwrap();
    let blob = repo.find_blob(entry.id()).unwrap();
    assert_eq!(
        blob.content(),
        b"changed",
        "amended commit's tree should contain the staged content"
    );

    // The amend *replaced* the commit rather than appending a new one: HEAD is
    // still the root commit (no parent) and history holds exactly one commit.
    assert_eq!(head.parent_count(), 0);
    let mut revwalk = repo.revwalk().unwrap();
    revwalk.push_head().unwrap();
    assert_eq!(
        revwalk.count(),
        1,
        "amend must not append a second commit"
    );
}

#[test]
fn commit_with_no_staged_changes_fails() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");
    let path = repo_path(&git_repo);

    let result = commit_with_message(path, "empty commit");
    assert!(result.is_err());
}

// ── Tag ─────────────────────────────────────────────────────────────

#[test]
fn add_tag_creates_tag_at_commit() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let oid = commit_file(repo, "a.txt", "a", "initial");

    add_tag(repo, "v1.0", oid).unwrap();

    // The tag must resolve (peel) to the exact commit it was created at, not
    // merely exist by name.
    let resolved = repo
        .revparse_single("v1.0")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert_eq!(resolved.id(), oid);
}

#[test]
fn add_duplicate_tag_fails() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let oid = commit_file(repo, "a.txt", "a", "initial");

    add_tag(repo, "v1.0", oid).unwrap();
    let result = add_tag(repo, "v1.0", oid);
    assert!(result.is_err());
}

#[test]
fn get_tags_resolves_lightweight_and_annotated_tags() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let c1 = commit_file(repo, "a.txt", "a", "first");
    let c2 = commit_file(repo, "b.txt", "b", "second");

    // Lightweight tag on c1; annotated tag (its own tag object) on c2.
    add_tag(repo, "light", c1).unwrap();
    git_cli(
        repo_path(&git_repo),
        &["tag", "-a", "annot", "-m", "release", &c2.to_string()],
    );

    let tags = git_repo.get_tags();
    let target = |name: &str| tags.iter().find(|t| t.name == name).map(|t| t.target_oid);

    // The annotated tag must be peeled through its tag object to the commit
    // it references (c2), not report the tag object's own oid.
    assert_eq!(target("light"), Some(c1));
    assert_eq!(target("annot"), Some(c2));
}

// ── Restore ─────────────────────────────────────────────────────────

#[test]
fn restore_files_discards_tracked_changes() {
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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
    let (_td, git_repo) = init_repo(Seed::Empty);
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

// ── Rebase ──────────────────────────────────────────────────────────

#[test]
fn rebase_branch_replays_commits_onto_advanced_base() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let base1 = commit_file(repo, "a.txt", "a", "base 1");
    let default = repo.head().unwrap().shorthand().unwrap().to_string();

    // feature branches off base1 with two commits
    create_branch(repo, "feature", base1).unwrap();
    checkout_branch(repo, "feature").unwrap();
    commit_file(repo, "f1.txt", "f1", "feature 1");
    let old_tip = commit_file(repo, "f2.txt", "f2", "feature 2");

    // base advances with an independent commit
    checkout_branch(repo, &default).unwrap();
    let base2 = commit_file(repo, "b2.txt", "b2", "base 2");

    // rebase feature onto the advanced base
    checkout_branch(repo, "feature").unwrap();
    rebase_branch(repo, &default).unwrap();

    // The branch ref moved to a brand-new tip (commits were replayed).
    let new_tip = repo
        .find_branch("feature", git2::BranchType::Local)
        .unwrap()
        .get()
        .peel_to_commit()
        .unwrap();
    assert_ne!(new_tip.id(), old_tip, "feature tip should be rewritten");

    // Parent chain: f2' -> f1' -> base2 (rebased onto the advanced base).
    assert_eq!(new_tip.parent_count(), 1);
    let f1_prime = new_tip.parent(0).unwrap();
    assert_eq!(f1_prime.parent(0).unwrap().id(), base2);

    // Replayed commits keep their messages...
    assert_eq!(new_tip.message().unwrap().trim(), "feature 2");
    assert_eq!(f1_prime.message().unwrap().trim(), "feature 1");

    // ...and the final tree carries both the base and feature changes.
    let tree = new_tip.tree().unwrap();
    for name in ["a.txt", "b2.txt", "f1.txt", "f2.txt"] {
        assert!(
            tree.get_name(name).is_some(),
            "rebased tree should contain {name}"
        );
    }

    // HEAD followed the branch to its new tip.
    assert_eq!(head_oid(repo), new_tip.id());
}

#[test]
fn rebase_branch_conflict_errors_and_leaves_rebase_in_progress() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let base = commit_file(repo, "f.txt", "base\n", "base");
    let default = repo.head().unwrap().shorthand().unwrap().to_string();

    create_branch(repo, "feature", base).unwrap();
    checkout_branch(repo, "feature").unwrap();
    commit_file(repo, "f.txt", "feature\n", "feature edit");

    checkout_branch(repo, &default).unwrap();
    commit_file(repo, "f.txt", "main\n", "main edit");

    checkout_branch(repo, "feature").unwrap();
    let result = rebase_branch(repo, &default);
    assert!(result.is_err(), "conflicting rebase should error");

    // documents current behavior: rebase_branch does not abort on conflict —
    // there is no cleanup/abort in operations.rs, so the repo is left in a
    // mid-rebase state.
    assert_ne!(
        repo.state(),
        git2::RepositoryState::Clean,
        "conflicting rebase leaves an in-progress rebase state"
    );
}

// ── Stash ───────────────────────────────────────────────────────────

/// Stage a modification of `a.txt` to `contents` so a subsequent stash has
/// staged content to capture.
fn stage_change(repo: &git2::Repository, path: &str, contents: &str) {
    fs::write(repo.workdir().unwrap().join("a.txt"), contents).unwrap();
    stage_file(path, "a.txt").unwrap();
}

#[test]
fn stash_staged_clears_index_and_creates_stash() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "committed\n", "initial");
    let path = repo_path(&git_repo);

    stage_change(repo, path, "staged change\n");

    // empty message -> exercises the `stash push --staged` (no -m) branch
    stash_staged(path, "").unwrap();

    // Working tree + index reverted to the committed state...
    assert_eq!(
        fs::read_to_string(repo.workdir().unwrap().join("a.txt")).unwrap(),
        "committed\n"
    );
    assert!(
        repo.statuses(None).unwrap().is_empty(),
        "index and working tree should be clean after stashing"
    );
    // ...and exactly one stash was created.
    assert_eq!(stash_count(path), 1);
}

#[test]
fn stash_staged_records_custom_message() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "committed\n", "initial");
    let path = repo_path(&git_repo);

    stage_change(repo, path, "staged change\n");
    stash_staged(path, "my stash message").unwrap();

    let list = stash_list(path);
    assert_eq!(list.len(), 1);
    assert!(
        list[0].contains("my stash message"),
        "stash entry should carry the custom message: {}",
        list[0]
    );
}

#[test]
fn stash_apply_restores_changes_and_retains_stash() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "committed\n", "initial");
    let path = repo_path(&git_repo);

    stage_change(repo, path, "staged change\n");
    stash_staged(path, "wip").unwrap();

    stash_apply(path, 0).unwrap();

    // The stashed change is back in the working tree...
    assert_eq!(
        fs::read_to_string(repo.workdir().unwrap().join("a.txt")).unwrap(),
        "staged change\n"
    );
    // ...and apply keeps the stash around.
    assert_eq!(stash_count(path), 1);
}

#[test]
fn stash_pop_restores_changes_and_drops_stash() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "committed\n", "initial");
    let path = repo_path(&git_repo);

    stage_change(repo, path, "staged change\n");
    stash_staged(path, "wip").unwrap();

    stash_pop(path, 0).unwrap();

    // The change is restored...
    assert_eq!(
        fs::read_to_string(repo.workdir().unwrap().join("a.txt")).unwrap(),
        "staged change\n"
    );
    // ...and the stash is gone.
    assert_eq!(stash_count(path), 0);
}

#[test]
fn stash_drop_removes_only_the_targeted_stash() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "committed\n", "initial");
    let path = repo_path(&git_repo);

    // Two stashes: "first" (older, ends up at stash@{1}), "second" (newer, @{0}).
    stage_change(repo, path, "v1\n");
    stash_staged(path, "first").unwrap();
    stage_change(repo, path, "v2\n");
    stash_staged(path, "second").unwrap();
    assert_eq!(stash_count(path), 2);

    // Drop the newest; the older "first" must remain.
    stash_drop(path, 0).unwrap();

    let list = stash_list(path);
    assert_eq!(list.len(), 1);
    assert!(
        list[0].contains("first"),
        "the older stash should be the survivor: {}",
        list[0]
    );
    assert!(
        !list[0].contains("second"),
        "the dropped stash must not remain: {}",
        list[0]
    );
}

#[test]
fn stash_apply_without_stash_errors() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    commit_file(git_repo.repo(), "a.txt", "a", "initial");
    assert!(stash_apply(repo_path(&git_repo), 0).is_err());
}

#[test]
fn stash_pop_without_stash_errors() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    commit_file(git_repo.repo(), "a.txt", "a", "initial");
    assert!(stash_pop(repo_path(&git_repo), 0).is_err());
}

#[test]
fn stash_drop_without_stash_errors() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    commit_file(git_repo.repo(), "a.txt", "a", "initial");
    assert!(stash_drop(repo_path(&git_repo), 0).is_err());
}

// ── Remote checkout / fetch / push ──────────────────────────────────

#[test]
fn checkout_remote_branch_checks_out_matching_local_branch() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let path = repo_path(&git_repo);
    let c1 = commit_file(repo, "a.txt", "a", "initial");

    // Local branch "same" and origin/same both point at c1.
    create_branch(repo, "same", c1).unwrap();
    let _origin = add_bare_origin(path);
    git_cli(path, &["push", "origin", "same"]);
    git_cli(path, &["fetch", "origin"]);

    checkout_remote_branch(repo, "origin/same").unwrap();

    // Since local and remote agree, it simply checks out the existing branch.
    assert_eq!(repo.head().unwrap().shorthand().unwrap(), "same");
    assert_eq!(head_oid(repo), c1);
}

#[test]
fn checkout_remote_branch_creates_tracking_branch() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let path = repo_path(&git_repo);
    let c1 = commit_file(repo, "a.txt", "a", "initial");
    let default = repo.head().unwrap().shorthand().unwrap().to_string();
    let _origin = add_bare_origin(path);

    // Build a commit and publish it to origin as "feature", leaving no local
    // branch of that name.
    create_branch(repo, "feat_src", c1).unwrap();
    checkout_branch(repo, "feat_src").unwrap();
    let c2 = commit_file(repo, "feat.txt", "feat", "feature work");
    git_cli(path, &["push", "origin", "feat_src:feature"]);
    checkout_branch(repo, &default).unwrap();
    delete_branch(repo, "feat_src").unwrap();
    git_cli(path, &["fetch", "origin"]);
    assert!(
        repo.find_branch("feature", git2::BranchType::Local).is_err(),
        "precondition: no local 'feature' branch yet"
    );

    checkout_remote_branch(repo, "origin/feature").unwrap();

    // A local tracking branch was created, pointing at the remote commit...
    let branch = repo
        .find_branch("feature", git2::BranchType::Local)
        .unwrap();
    assert_eq!(branch.get().peel_to_commit().unwrap().id(), c2);
    // ...with its upstream configured to origin/feature...
    let upstream = branch.upstream().unwrap();
    assert_eq!(upstream.name().unwrap().unwrap(), "origin/feature");
    // ...and HEAD moved onto it.
    assert_eq!(repo.head().unwrap().shorthand().unwrap(), "feature");
    assert_eq!(head_oid(repo), c2);
}

#[test]
fn checkout_remote_branch_force_updates_diverged_local_branch() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let path = repo_path(&git_repo);
    let c1 = commit_file(repo, "a.txt", "a", "initial");
    let default = repo.head().unwrap().shorthand().unwrap().to_string();
    let _origin = add_bare_origin(path);

    // Local "div" stays at c1; origin/div is advanced to a different commit q.
    create_branch(repo, "div", c1).unwrap();
    create_branch(repo, "div_src", c1).unwrap();
    checkout_branch(repo, "div_src").unwrap();
    let q = commit_file(repo, "b.txt", "b", "remote work");
    git_cli(path, &["push", "origin", "div_src:div"]);
    checkout_branch(repo, &default).unwrap();
    git_cli(path, &["fetch", "origin"]);

    // Precondition: local and remote diverged.
    assert_eq!(
        repo.find_branch("div", git2::BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap()
            .id(),
        c1
    );

    checkout_remote_branch(repo, "origin/div").unwrap();

    // The local branch was force-moved to the remote OID and checked out.
    assert_eq!(
        repo.find_branch("div", git2::BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap()
            .id(),
        q
    );
    assert_eq!(head_oid(repo), q);
}

#[test]
fn fetch_origin_without_remote_errors() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    commit_file(git_repo.repo(), "a.txt", "a", "initial");
    assert!(fetch_origin(repo_path(&git_repo)).is_err());
}

#[test]
fn push_to_origin_without_remote_errors() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    commit_file(git_repo.repo(), "a.txt", "a", "initial");
    assert!(push_to_origin(repo_path(&git_repo)).is_err());
}

// ── Merge conflict ──────────────────────────────────────────────────

#[test]
fn merge_branch_conflict_errors_and_leaves_repo_mid_merge() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let base = commit_file(repo, "f.txt", "base\n", "base");
    let default = repo.head().unwrap().shorthand().unwrap().to_string();

    // Two branches edit the same line differently -> guaranteed conflict.
    create_branch(repo, "feature", base).unwrap();
    commit_file(repo, "f.txt", "main\n", "main edit");
    checkout_branch(repo, "feature").unwrap();
    commit_file(repo, "f.txt", "feature\n", "feature edit");
    checkout_branch(repo, &default).unwrap();

    let err = merge_branch(repo, "feature").unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("conflict"),
        "error should mention the conflict: {err}"
    );

    // documents current behavior: merge_branch bails *after* repo.merge() has
    // already written conflicts, leaving the repo mid-merge (index conflicts +
    // MERGE_HEAD). This is a known product gap; the test pins current behavior.
    assert!(
        repo.index().unwrap().has_conflicts(),
        "conflicted index is left behind"
    );
    assert_eq!(repo.state(), git2::RepositoryState::Merge);
}

// ── Cherry-pick / revert conflicts ──────────────────────────────────

#[test]
fn cherry_pick_conflict_errors_and_leaves_cherry_pick_head() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let path = repo_path(&git_repo);
    let base = commit_file(repo, "f.txt", "base\n", "base");
    let default = repo.head().unwrap().shorthand().unwrap().to_string();

    create_branch(repo, "feature", base).unwrap();
    checkout_branch(repo, "feature").unwrap();
    let pick = commit_file(repo, "f.txt", "feature\n", "feature edit");
    checkout_branch(repo, &default).unwrap();
    commit_file(repo, "f.txt", "main\n", "main edit");

    let result = cherry_pick(path, pick);
    assert!(result.is_err(), "conflicting cherry-pick should error");

    // documents current behavior: the failed shell-out leaves git's mid-
    // cherry-pick state (CHERRY_PICK_HEAD) in place.
    assert!(
        Path::new(path).join(".git/CHERRY_PICK_HEAD").exists(),
        "CHERRY_PICK_HEAD is left behind after a conflicting cherry-pick"
    );
}

#[test]
fn revert_commit_conflict_errors_and_leaves_revert_head() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let path = repo_path(&git_repo);
    commit_file(repo, "f.txt", "line1\n", "c1");
    let to_revert = commit_file(repo, "f.txt", "line2\n", "c2");
    commit_file(repo, "f.txt", "line3\n", "c3");

    // Reverting c2 (line1->line2) conflicts with c3's line3.
    let result = revert_commit(path, to_revert);
    assert!(result.is_err(), "conflicting revert should error");

    // documents current behavior: the failed shell-out leaves git's mid-revert
    // state (REVERT_HEAD) in place.
    assert!(
        Path::new(path).join(".git/REVERT_HEAD").exists(),
        "REVERT_HEAD is left behind after a conflicting revert"
    );
}

// ── Reset / restore / amend / checkout edge cases ───────────────────

#[test]
fn reset_to_nonexistent_commit_errors() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    commit_file(git_repo.repo(), "a.txt", "a", "initial");
    let bogus = Oid::from_str("0000000000000000000000000000000000000001").unwrap();
    assert!(reset_to_commit(repo_path(&git_repo), bogus, ResetMode::Hard).is_err());
}

#[test]
fn restore_files_trashes_untracked_file() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let path = repo_path(&git_repo);
    commit_file(repo, "a.txt", "a", "initial");

    let junk = repo.workdir().unwrap().join("junk.txt");
    fs::write(&junk, "junk").unwrap();

    // trash::delete may be unavailable in a headless environment; assert the
    // observable side effect for whichever outcome actually occurs.
    match restore_files(path, &["junk.txt".to_string()]) {
        Ok(()) => assert!(
            !junk.exists(),
            "untracked file should be removed from the working tree (trashed)"
        ),
        Err(_) => assert!(
            junk.exists(),
            "trash unavailable here; file is left in place (documents current behavior)"
        ),
    }
}

#[test]
fn commit_amend_on_unborn_head_errors() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    assert!(commit_amend(repo_path(&git_repo), "nope").is_err());
}

#[test]
fn commit_amend_no_edit_on_unborn_head_errors() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    assert!(commit_amend_no_edit(repo_path(&git_repo)).is_err());
}

#[test]
fn checkout_commit_nonexistent_oid_errors() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    commit_file(repo, "a.txt", "a", "initial");
    let bogus = Oid::from_str("0000000000000000000000000000000000000001").unwrap();
    assert!(checkout_commit(repo, bogus).is_err());
}

// ── friendly_commit_error mappings ──────────────────────────────────

#[test]
fn commit_with_empty_message_maps_to_friendly_error() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let path = repo_path(&git_repo);
    commit_file(repo, "a.txt", "a", "initial");

    // Stage a real change so the failure is the empty message, not "nothing to
    // commit".
    fs::write(repo.workdir().unwrap().join("a.txt"), "changed").unwrap();
    stage_file(path, "a.txt").unwrap();

    let err = commit_with_message(path, "").unwrap_err();
    assert!(
        err.to_string().contains("Commit message cannot be empty"),
        "empty message should map to a friendly error, got: {err}"
    );
}

#[test]
fn commit_without_configured_user_maps_to_friendly_error() {
    let (_td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let path = repo_path(&git_repo);
    commit_file(repo, "a.txt", "a", "initial");

    // Blank the identity so git refuses to author a commit ("Please tell me who
    // you are"). Empty local values override any global identity.
    git_cli(path, &["config", "user.name", ""]);
    git_cli(path, &["config", "user.email", ""]);

    fs::write(repo.workdir().unwrap().join("a.txt"), "changed").unwrap();
    stage_file(path, "a.txt").unwrap();

    let err = commit_with_message(path, "a message").unwrap_err();
    assert!(
        err.to_string().contains("Git user not configured"),
        "unconfigured identity should map to a friendly error, got: {err}"
    );
}
