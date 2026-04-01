//! Git operations (checkout, merge, rebase, branch operations)

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use git2::{BranchType, Oid, Repository};

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
    let output = Command::new("git")
        .args(["fetch", "origin"])
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git fetch")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git fetch failed: {}", stderr.trim());
    }

    Ok(())
}

/// Cherry-pick a commit
pub fn cherry_pick(repo_path: &str, commit_oid: Oid) -> Result<()> {
    let output = Command::new("git")
        .args(["cherry-pick", &commit_oid.to_string()])
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git cherry-pick")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git cherry-pick failed: {}", stderr.trim());
    }

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

    let output = Command::new("git")
        .args(["reset", mode_flag, &commit_oid.to_string()])
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git reset")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git reset failed: {}", stderr.trim());
    }

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
    let output = Command::new("git")
        .args(["revert", "--no-edit", &commit_oid.to_string()])
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git revert")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git revert failed: {}", stderr.trim());
    }

    Ok(())
}

/// Push current branch to origin
pub fn push_to_origin(repo_path: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["push", "origin", "HEAD"])
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git push")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git push failed: {}", stderr.trim());
    }

    Ok(())
}

/// Stage a file
pub fn stage_file(repo_path: &str, file_path: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["add", "--", file_path])
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git add failed: {}", stderr.trim());
    }

    Ok(())
}

/// Unstage a file
pub fn unstage_file(repo_path: &str, file_path: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["reset", "HEAD", "--", file_path])
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git reset")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git reset failed: {}", stderr.trim());
    }

    Ok(())
}

/// Add a pattern to .gitignore at the repository root.
/// Returns Ok(false) if the pattern already exists, Ok(true) if it was added.
pub fn add_to_gitignore(repo_path: &str, pattern: &str) -> Result<bool> {
    let gitignore_path = Path::new(repo_path).join(".gitignore");

    // Check if pattern already exists
    if gitignore_path.exists() {
        let contents = std::fs::read_to_string(&gitignore_path)
            .context("Failed to read .gitignore")?;
        if contents.lines().any(|line| line.trim() == pattern.trim()) {
            return Ok(false);
        }
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&gitignore_path)
        .context("Failed to open .gitignore")?;

    // Ensure we start on a new line if file doesn't end with one
    if gitignore_path.exists() {
        let contents = std::fs::read_to_string(&gitignore_path)
            .context("Failed to read .gitignore")?;
        if !contents.is_empty() && !contents.ends_with('\n') {
            writeln!(file)?;
        }
    }

    writeln!(file, "{}", pattern).context("Failed to write to .gitignore")?;

    Ok(true)
}

/// Move a file or folder to `.archive/` at the repository root.
/// Creates the `.archive/` directory if it doesn't exist.
/// Preserves the relative path structure inside `.archive/`.
pub fn archive_path(repo_path: &str, relative_path: &str) -> Result<()> {
    let repo = Path::new(repo_path);
    let source = repo.join(relative_path);

    if !source.exists() {
        bail!("Path does not exist: {}", relative_path);
    }

    let dest = repo.join(".archive").join(relative_path);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .context("Failed to create .archive directory structure")?;
    }

    fs::rename(&source, &dest).context(format!(
        "Failed to move '{}' to '.archive/{}'",
        relative_path, relative_path
    ))?;

    Ok(())
}

/// Remove a pattern from .gitignore at the repository root.
/// Returns Ok(false) if the pattern was not found, Ok(true) if removed.
pub fn remove_from_gitignore(repo_path: &str, pattern: &str) -> Result<bool> {
    let gitignore_path = Path::new(repo_path).join(".gitignore");

    if !gitignore_path.exists() {
        return Ok(false);
    }

    let contents =
        fs::read_to_string(&gitignore_path).context("Failed to read .gitignore")?;

    let trimmed = pattern.trim();
    let filtered: Vec<&str> = contents
        .lines()
        .filter(|line| line.trim() != trimmed)
        .collect();

    if filtered.len() == contents.lines().count() {
        return Ok(false);
    }

    let mut new_contents = filtered.join("\n");
    if !new_contents.is_empty() {
        new_contents.push('\n');
    }

    fs::write(&gitignore_path, new_contents).context("Failed to write .gitignore")?;

    Ok(true)
}

/// Move a file or folder from `.archive/` back to its original location.
pub fn unarchive_path(repo_path: &str, relative_path: &str) -> Result<()> {
    let repo = Path::new(repo_path);
    let source = repo.join(".archive").join(relative_path);

    if !source.exists() {
        bail!(
            "Archived path does not exist: .archive/{}",
            relative_path
        );
    }

    let dest = repo.join(relative_path);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).context("Failed to create directory structure")?;
    }

    fs::rename(&source, &dest).context(format!(
        "Failed to move '.archive/{}' back to '{}'",
        relative_path, relative_path
    ))?;

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

    // Restore tracked files
    if !tracked.is_empty() {
        let mut args = vec!["checkout".to_string(), "--".to_string()];
        args.extend(tracked);
        let output = Command::new("git")
            .args(&args)
            .current_dir(repo_path)
            .output()
            .context("Failed to execute git checkout")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git checkout failed: {}", stderr.trim());
        }
    }

    // Trash untracked files
    for path in &untracked {
        let full = Path::new(repo_path).join(path);
        trash::delete(&full).context(format!("Failed to trash '{}'", path))?;
    }

    Ok(())
}

/// Create a commit with the given message
pub fn commit_with_message(repo_path: &str, message: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git commit")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git commit failed: {}", stderr.trim());
    }

    Ok(())
}

/// Amend the last commit with a new message.
pub fn commit_amend(repo_path: &str, message: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["commit", "--amend", "-m", message])
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git commit --amend")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git commit --amend failed: {}", stderr.trim());
    }

    Ok(())
}

/// Amend the last commit without changing the message.
pub fn commit_amend_no_edit(repo_path: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["commit", "--amend", "--no-edit"])
        .current_dir(repo_path)
        .output()
        .context("Failed to execute git commit --amend --no-edit")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git commit --amend --no-edit failed: {}", stderr.trim());
    }

    Ok(())
}

/// Get the message of the last commit.
pub fn get_last_commit_message(repo_path: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(repo_path)
        .output()
        .context("Failed to get last commit message")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git log failed: {}", stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_temp_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn add_to_gitignore_creates_file_if_missing() {
        let dir = setup_temp_dir();
        let result = add_to_gitignore(dir.path().to_str().unwrap(), "target/").unwrap();
        assert!(result);
        let contents = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(contents, "target/\n");
    }

    #[test]
    fn add_to_gitignore_appends_to_existing() {
        let dir = setup_temp_dir();
        fs::write(dir.path().join(".gitignore"), "node_modules/\n").unwrap();

        let result = add_to_gitignore(dir.path().to_str().unwrap(), "target/").unwrap();
        assert!(result);
        let contents = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(contents, "node_modules/\ntarget/\n");
    }

    #[test]
    fn add_to_gitignore_appends_newline_if_missing() {
        let dir = setup_temp_dir();
        fs::write(dir.path().join(".gitignore"), "node_modules/").unwrap();

        let result = add_to_gitignore(dir.path().to_str().unwrap(), "target/").unwrap();
        assert!(result);
        let contents = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(contents, "node_modules/\ntarget/\n");
    }

    #[test]
    fn add_to_gitignore_skips_duplicate() {
        let dir = setup_temp_dir();
        fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();

        let result = add_to_gitignore(dir.path().to_str().unwrap(), "target/").unwrap();
        assert!(!result);
        let contents = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(contents, "target/\n");
    }

    #[test]
    fn add_to_gitignore_handles_empty_file() {
        let dir = setup_temp_dir();
        fs::write(dir.path().join(".gitignore"), "").unwrap();

        let result = add_to_gitignore(dir.path().to_str().unwrap(), "*.log").unwrap();
        assert!(result);
        let contents = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(contents, "*.log\n");
    }

    #[test]
    fn archive_path_moves_file() {
        let dir = setup_temp_dir();
        fs::write(dir.path().join("old.txt"), "content").unwrap();

        archive_path(dir.path().to_str().unwrap(), "old.txt").unwrap();

        assert!(!dir.path().join("old.txt").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join(".archive/old.txt")).unwrap(),
            "content"
        );
    }

    #[test]
    fn archive_path_preserves_directory_structure() {
        let dir = setup_temp_dir();
        fs::create_dir_all(dir.path().join("src/utils")).unwrap();
        fs::write(dir.path().join("src/utils/helper.rs"), "fn help() {}").unwrap();

        archive_path(dir.path().to_str().unwrap(), "src/utils/helper.rs").unwrap();

        assert!(!dir.path().join("src/utils/helper.rs").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join(".archive/src/utils/helper.rs")).unwrap(),
            "fn help() {}"
        );
    }

    #[test]
    fn archive_path_moves_folder() {
        let dir = setup_temp_dir();
        fs::create_dir_all(dir.path().join("src/old")).unwrap();
        fs::write(dir.path().join("src/old/a.rs"), "a").unwrap();
        fs::write(dir.path().join("src/old/b.rs"), "b").unwrap();

        archive_path(dir.path().to_str().unwrap(), "src/old").unwrap();

        assert!(!dir.path().join("src/old").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join(".archive/src/old/a.rs")).unwrap(),
            "a"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join(".archive/src/old/b.rs")).unwrap(),
            "b"
        );
    }

    #[test]
    fn archive_path_errors_on_missing_source() {
        let dir = setup_temp_dir();
        let result = archive_path(dir.path().to_str().unwrap(), "nonexistent.txt");
        assert!(result.is_err());
    }

    #[test]
    fn remove_from_gitignore_removes_matching_line() {
        let dir = setup_temp_dir();
        fs::write(dir.path().join(".gitignore"), "target/\nnode_modules/\n*.log\n").unwrap();

        let result = remove_from_gitignore(dir.path().to_str().unwrap(), "node_modules/").unwrap();
        assert!(result);
        let contents = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(contents, "target/\n*.log\n");
    }

    #[test]
    fn remove_from_gitignore_returns_false_if_not_found() {
        let dir = setup_temp_dir();
        fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();

        let result = remove_from_gitignore(dir.path().to_str().unwrap(), "missing").unwrap();
        assert!(!result);
        let contents = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(contents, "target/\n");
    }

    #[test]
    fn remove_from_gitignore_handles_missing_file() {
        let dir = setup_temp_dir();
        let result = remove_from_gitignore(dir.path().to_str().unwrap(), "target/").unwrap();
        assert!(!result);
    }

    #[test]
    fn unarchive_path_moves_file_back() {
        let dir = setup_temp_dir();
        fs::create_dir_all(dir.path().join(".archive")).unwrap();
        fs::write(dir.path().join(".archive/old.txt"), "content").unwrap();

        unarchive_path(dir.path().to_str().unwrap(), "old.txt").unwrap();

        assert!(!dir.path().join(".archive/old.txt").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("old.txt")).unwrap(),
            "content"
        );
    }

    #[test]
    fn unarchive_path_preserves_directory_structure() {
        let dir = setup_temp_dir();
        fs::create_dir_all(dir.path().join(".archive/src/utils")).unwrap();
        fs::write(dir.path().join(".archive/src/utils/helper.rs"), "fn help() {}").unwrap();

        unarchive_path(dir.path().to_str().unwrap(), "src/utils/helper.rs").unwrap();

        assert!(!dir.path().join(".archive/src/utils/helper.rs").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("src/utils/helper.rs")).unwrap(),
            "fn help() {}"
        );
    }

    #[test]
    fn unarchive_path_errors_on_missing_source() {
        let dir = setup_temp_dir();
        let result = unarchive_path(dir.path().to_str().unwrap(), "nonexistent.txt");
        assert!(result.is_err());
    }
}
