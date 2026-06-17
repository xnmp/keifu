//! Git operations (checkout, merge, rebase, branch operations)

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use git2::{BranchType, Oid, Repository};

/// Run a git CLI command and return its output, or bail with stderr on failure.
fn run_git(repo_path: &str, args: &[&str]) -> Result<std::process::Output> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_path)
        .stdin(Stdio::null())
        .output()
        .context(format!("Failed to execute git {}", args.first().unwrap_or(&"")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.first().unwrap_or(&""), stderr.trim());
    }
    Ok(output)
}

/// Checkout a branch
pub fn checkout_branch(repo: &Repository, branch_name: &str) -> Result<()> {
    let branch = repo
        .find_branch(branch_name, BranchType::Local)
        .context(format!("Branch '{}' not found", branch_name))?;

    let reference = branch.get();
    let commit = reference.peel_to_commit()?;
    let tree = commit.tree()?;

    repo.checkout_tree(tree.as_object(), None)?;
    repo.set_head(reference.name().unwrap())?;

    Ok(())
}

/// Checkout a commit (detached HEAD)
pub fn checkout_commit(repo: &Repository, oid: Oid) -> Result<()> {
    let commit = repo.find_commit(oid).context("Commit not found")?;
    let tree = commit.tree()?;

    repo.checkout_tree(tree.as_object(), None)?;
    repo.set_head_detached(oid)?;

    Ok(())
}

/// Checkout a remote branch (create and track a local branch)
pub fn checkout_remote_branch(repo: &Repository, remote_branch: &str) -> Result<()> {
    // Extract "branch-name" from "origin/branch-name"
    let local_name = remote_branch
        .strip_prefix("origin/")
        .context("Invalid remote branch format")?;

    // Look up the remote branch
    let remote_ref = repo
        .find_branch(remote_branch, BranchType::Remote)
        .context(format!("Remote branch '{}' not found", remote_branch))?;

    let remote_commit = remote_ref.get().peel_to_commit()?;
    let remote_oid = remote_commit.id();
    let tree = remote_commit.tree()?;

    // Check if a local branch with the same name exists
    if let Ok(local_branch) = repo.find_branch(local_name, BranchType::Local) {
        // Get OIDs via peel_to_commit() for a reliable comparison
        let local_commit = local_branch.get().peel_to_commit()?;
        let local_oid = local_commit.id();
        if local_oid == remote_oid {
            // Local and remote point to the same commit -> checkout local branch
            return checkout_branch(repo, local_name);
        } else {
            // Pointing to different commits -> update local branch and checkout
            // Equivalent to: git checkout -B local_name origin/xxx
            let is_current_branch = local_branch.is_head();
            drop(local_branch); // Release the branch reference

            let refname = format!("refs/heads/{}", local_name);
            if is_current_branch {
                // Cannot force update current branch with repo.branch()
                // Update the reference directly after checkout
                repo.checkout_tree(tree.as_object(), None)?;
                repo.reference(&refname, remote_oid, true, "Update to remote")?;
            } else {
                repo.branch(local_name, &remote_commit, true)?; // Overwrite with force=true
                repo.checkout_tree(tree.as_object(), None)?;
                repo.set_head(&refname)?;
            }
            return Ok(());
        }
    }

    // No local branch -> create and track
    let mut local_branch = repo
        .branch(local_name, &remote_commit, false)
        .context(format!("Failed to create local branch '{}'", local_name))?;

    // Set upstream
    local_branch.set_upstream(Some(remote_branch))?;

    // Checkout
    repo.checkout_tree(tree.as_object(), None)?;
    repo.set_head(&format!("refs/heads/{}", local_name))?;

    Ok(())
}

/// Create a new branch
pub fn create_branch(repo: &Repository, branch_name: &str, from_oid: Oid) -> Result<()> {
    let commit = repo.find_commit(from_oid).context("Commit not found")?;

    repo.branch(branch_name, &commit, false)
        .context(format!("Failed to create branch '{}'", branch_name))?;

    Ok(())
}

/// Delete a branch
pub fn delete_branch(repo: &Repository, branch_name: &str) -> Result<()> {
    let mut branch = repo
        .find_branch(branch_name, BranchType::Local)
        .context(format!("Branch '{}' not found", branch_name))?;

    if branch.is_head() {
        bail!("Cannot delete current branch");
    }

    branch.delete()?;
    Ok(())
}

/// Perform a merge
pub fn merge_branch(repo: &Repository, branch_name: &str) -> Result<()> {
    let branch = repo
        .find_branch(branch_name, BranchType::Local)
        .context(format!("Branch '{}' not found", branch_name))?;

    let reference = branch.get();
    let annotated_commit = repo.reference_to_annotated_commit(reference)?;

    let (analysis, _) = repo.merge_analysis(&[&annotated_commit])?;

    if analysis.is_up_to_date() {
        return Ok(());
    }

    if analysis.is_fast_forward() {
        // Fast-forward merge
        let target_oid = reference.target().unwrap();
        let target_commit = repo.find_commit(target_oid)?;
        let tree = target_commit.tree()?;

        repo.checkout_tree(tree.as_object(), None)?;

        let mut head_ref = repo.head()?;
        head_ref.set_target(target_oid, &format!("Fast-forward merge: {}", branch_name))?;

        return Ok(());
    }

    if analysis.is_normal() {
        // Normal merge
        repo.merge(&[&annotated_commit], None, None)?;

        if repo.index()?.has_conflicts() {
            bail!("Merge conflict occurred. Please resolve manually.");
        }

        // Create a merge commit
        let signature = repo.signature()?;
        let head = repo.head()?;
        let head_commit = head.peel_to_commit()?;
        let merge_commit = repo.find_commit(annotated_commit.id())?;
        let tree_oid = repo.index()?.write_tree()?;
        let tree = repo.find_tree(tree_oid)?;

        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            &format!("Merge branch '{}'", branch_name),
            &tree,
            &[&head_commit, &merge_commit],
        )?;

        repo.cleanup_state()?;
    }

    Ok(())
}

/// Perform a rebase (simple implementation)
pub fn rebase_branch(repo: &Repository, onto_branch: &str) -> Result<()> {
    let onto = repo
        .find_branch(onto_branch, BranchType::Local)
        .context(format!("Branch '{}' not found", onto_branch))?;

    let onto_annotated = repo.reference_to_annotated_commit(onto.get())?;

    let mut rebase = repo.rebase(None, Some(&onto_annotated), None, None)?;

    while let Some(op) = rebase.next() {
        let _operation = op?;
        let signature = repo.signature()?;
        rebase.commit(None, &signature, None)?;
    }

    rebase.finish(None)?;

    Ok(())
}

/// Fetch from origin remote using git command
pub fn fetch_origin(repo_path: &str) -> Result<()> {
    run_git(repo_path, &["fetch", "origin"])?;
    Ok(())
}

/// Cherry-pick a commit
pub fn cherry_pick(repo_path: &str, commit_oid: Oid) -> Result<()> {
    run_git(repo_path, &["cherry-pick", &commit_oid.to_string()])?;
    Ok(())
}

/// Reset mode for `reset_to_commit`
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
}

/// Reset HEAD to a commit with the specified mode
pub fn reset_to_commit(repo_path: &str, commit_oid: Oid, mode: ResetMode) -> Result<()> {
    let mode_flag = match mode {
        ResetMode::Soft => "--soft",
        ResetMode::Mixed => "--mixed",
        ResetMode::Hard => "--hard",
    };

    run_git(repo_path, &["reset", mode_flag, &commit_oid.to_string()])?;
    Ok(())
}

/// Create a lightweight tag at the specified commit
pub fn add_tag(repo: &Repository, tag_name: &str, commit_oid: Oid) -> Result<()> {
    let obj = repo
        .find_object(commit_oid, None)
        .context("Commit not found")?;
    let signature = repo.signature()?;

    repo.tag(tag_name, &obj, &signature, "", false)
        .context(format!("Failed to create tag '{}'", tag_name))?;

    Ok(())
}

/// Revert a commit without opening an editor
pub fn revert_commit(repo_path: &str, commit_oid: Oid) -> Result<()> {
    run_git(repo_path, &["revert", "--no-edit", &commit_oid.to_string()])?;
    Ok(())
}

/// Push current branch to origin
pub fn push_to_origin(repo_path: &str) -> Result<()> {
    run_git(repo_path, &["push", "origin", "HEAD"])?;
    Ok(())
}

/// Stage a file
pub fn stage_file(repo_path: &str, file_path: &str) -> Result<()> {
    run_git(repo_path, &["add", "--", file_path])?;
    Ok(())
}

/// Unstage a file
pub fn unstage_file(repo_path: &str, file_path: &str) -> Result<()> {
    run_git(repo_path, &["reset", "HEAD", "--", file_path])?;
    Ok(())
}

/// Restore (discard changes to) the given files.
/// Tracked files are restored via `git checkout -- <path>`.
/// Untracked files are moved to the system recycle bin.
pub fn restore_files(repo_path: &str, paths: &[String]) -> Result<()> {
    let repo = Repository::open(repo_path).context("Failed to open repository")?;
    let statuses = repo.statuses(None).context("Failed to get git status")?;

    let mut tracked = Vec::new();
    let mut untracked = Vec::new();

    for path in paths {
        let is_untracked = statuses.iter().any(|entry| {
            entry.path() == Some(path)
                && entry
                    .status()
                    .intersects(git2::Status::WT_NEW | git2::Status::INDEX_NEW)
                && !entry
                    .status()
                    .intersects(git2::Status::WT_MODIFIED | git2::Status::INDEX_MODIFIED)
        });

        if is_untracked {
            untracked.push(path.clone());
        } else {
            tracked.push(path.clone());
        }
    }

    // Restore tracked files (checkout from HEAD to unstage + discard in one step)
    if !tracked.is_empty() {
        let mut args: Vec<String> = vec!["checkout".into(), "HEAD".into(), "--".into()];
        args.extend(tracked);
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        run_git(repo_path, &args_ref)?;
    }

    // Trash untracked files
    for path in &untracked {
        let full = Path::new(repo_path).join(path);
        trash::delete(&full).context(format!("Failed to trash '{}'", path))?;
    }

    Ok(())
}

fn friendly_commit_error(e: anyhow::Error) -> anyhow::Error {
    let msg = e.to_string();
    if msg.contains("nothing to commit") || msg.contains("nothing added to commit") {
        anyhow::anyhow!("No files staged for commit (use 's' to stage files)")
    } else if msg.contains("empty commit message") || msg.contains("Aborting commit due to empty") {
        anyhow::anyhow!("Commit message cannot be empty")
    } else if msg.contains("Please tell me who you are") {
        anyhow::anyhow!("Git user not configured (run: git config user.email / user.name)")
    } else {
        e
    }
}

/// Create a commit with the given message
pub fn commit_with_message(repo_path: &str, message: &str) -> Result<()> {
    run_git(repo_path, &["commit", "-m", message])
        .map_err(friendly_commit_error)?;
    Ok(())
}

/// Amend the last commit with a new message.
pub fn commit_amend(repo_path: &str, message: &str) -> Result<()> {
    run_git(repo_path, &["commit", "--amend", "-m", message])
        .map_err(friendly_commit_error)?;
    Ok(())
}

/// Amend the last commit without changing the message.
pub fn commit_amend_no_edit(repo_path: &str) -> Result<()> {
    run_git(repo_path, &["commit", "--amend", "--no-edit"])
        .map_err(friendly_commit_error)?;
    Ok(())
}

pub fn stash_staged(repo_path: &str, message: &str) -> Result<()> {
    let args = if message.is_empty() {
        vec!["stash", "push", "--staged"]
    } else {
        vec!["stash", "push", "--staged", "-m", message]
    };
    run_git(repo_path, &args)?;
    Ok(())
}

pub fn stash_apply(repo_path: &str, index: usize) -> Result<()> {
    let ref_name = format!("stash@{{{index}}}");
    run_git(repo_path, &["stash", "apply", &ref_name])?;
    Ok(())
}

pub fn stash_pop(repo_path: &str, index: usize) -> Result<()> {
    let ref_name = format!("stash@{{{index}}}");
    run_git(repo_path, &["stash", "pop", &ref_name])?;
    Ok(())
}

pub fn stash_drop(repo_path: &str, index: usize) -> Result<()> {
    let ref_name = format!("stash@{{{index}}}");
    run_git(repo_path, &["stash", "drop", &ref_name])?;
    Ok(())
}

/// Get the message of the last commit.
pub fn get_last_commit_message(repo_path: &str) -> Result<String> {
    let output = run_git(repo_path, &["log", "-1", "--format=%B"])?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

