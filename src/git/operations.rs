//! Git operations (checkout, merge, rebase, branch operations)

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use git2::{BranchType, Oid, Repository, Status};

use super::repository::OperationState;

/// Outcome of a history-integrating operation (merge/rebase/cherry-pick/revert).
///
/// A conflict is a first-class outcome, not an error: the operation genuinely
/// started and left the repo mid-way with resolvable conflicts. Callers show a
/// guided "resolve then continue / abort" flow instead of a raw error popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpOutcome {
    /// The operation finished cleanly.
    Completed,
    /// The operation stopped on conflicts; the repo is left in-progress.
    Conflicts { count: usize },
}

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

/// Count unmerged (conflicted) paths in the repo at `repo_path`.
fn count_conflicts(repo_path: &str) -> usize {
    Repository::open(repo_path)
        .and_then(|repo| {
            Ok(repo
                .statuses(None)?
                .iter()
                .filter(|e| e.status().contains(Status::CONFLICTED))
                .count())
        })
        .unwrap_or(0)
}

/// Run a git command that may legitimately stop on conflicts (cherry-pick,
/// revert, …). On non-zero exit, a still-present conflict is reported as
/// `Conflicts` rather than an error; a genuine failure bails with stderr.
fn run_git_allow_conflict(repo_path: &str, args: &[&str]) -> Result<OpOutcome> {
    let subcommand = args.first().copied().unwrap_or("");
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_path)
        // Never block the TUI on an editor prompt.
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .stdin(Stdio::null())
        .output()
        .context(format!("Failed to execute git {subcommand}"))?;
    if output.status.success() {
        return Ok(OpOutcome::Completed);
    }
    let count = count_conflicts(repo_path);
    if count > 0 {
        return Ok(OpOutcome::Conflicts { count });
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("git {} failed: {}", subcommand, stderr.trim());
}

/// Run a git CLI command, feeding `stdin_bytes` to its standard input.
/// Used for commands that read a patch from stdin (`git apply`).
fn run_git_with_stdin(
    repo_path: &str,
    args: &[&str],
    stdin_bytes: &[u8],
) -> Result<std::process::Output> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(repo_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context(format!("Failed to execute git {}", args.first().unwrap_or(&"")))?;

    child
        .stdin
        .take()
        .context("Failed to open git stdin")?
        .write_all(stdin_bytes)
        .context("Failed to write patch to git stdin")?;

    let output = child
        .wait_with_output()
        .context(format!("Failed to run git {}", args.first().unwrap_or(&"")))?;
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

/// Perform a merge.
///
/// On a conflicting normal merge this returns `Ok(OpOutcome::Conflicts)` and
/// deliberately leaves the repo mid-merge (conflicted index + MERGE_HEAD), so
/// the caller can offer resolve/continue/abort. It is NOT an error.
pub fn merge_branch(repo: &Repository, branch_name: &str) -> Result<OpOutcome> {
    let branch = repo
        .find_branch(branch_name, BranchType::Local)
        .context(format!("Branch '{}' not found", branch_name))?;

    let reference = branch.get();
    let annotated_commit = repo.reference_to_annotated_commit(reference)?;

    let (analysis, _) = repo.merge_analysis(&[&annotated_commit])?;

    if analysis.is_up_to_date() {
        return Ok(OpOutcome::Completed);
    }

    if analysis.is_fast_forward() {
        // Fast-forward merge
        let target_oid = reference.target().unwrap();
        let target_commit = repo.find_commit(target_oid)?;
        let tree = target_commit.tree()?;

        repo.checkout_tree(tree.as_object(), None)?;

        let mut head_ref = repo.head()?;
        head_ref.set_target(target_oid, &format!("Fast-forward merge: {}", branch_name))?;

        return Ok(OpOutcome::Completed);
    }

    if analysis.is_normal() {
        // Normal merge
        repo.merge(&[&annotated_commit], None, None)?;

        if repo.index()?.has_conflicts() {
            // Leave MERGE_HEAD + conflicted index in place; the user resolves
            // then continues (or aborts) from the UI.
            let count = repo
                .statuses(None)
                .map(|s| {
                    s.iter()
                        .filter(|e| e.status().contains(Status::CONFLICTED))
                        .count()
                })
                .unwrap_or(0);
            return Ok(OpOutcome::Conflicts { count });
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

    Ok(OpOutcome::Completed)
}

/// Perform a rebase (simple implementation).
///
/// On a conflicting step this returns `Ok(OpOutcome::Conflicts)` and leaves the
/// rebase in progress (REBASE_HEAD etc.) for resolve/continue/abort — it does
/// NOT abort automatically.
pub fn rebase_branch(repo: &Repository, onto_branch: &str) -> Result<OpOutcome> {
    let onto = repo
        .find_branch(onto_branch, BranchType::Local)
        .context(format!("Branch '{}' not found", onto_branch))?;

    let onto_annotated = repo.reference_to_annotated_commit(onto.get())?;

    let mut rebase = repo.rebase(None, Some(&onto_annotated), None, None)?;

    while let Some(op) = rebase.next() {
        let _operation = op?;
        // A conflicting patch leaves unmerged entries in the index; committing
        // now would fail. Stop and leave the rebase in progress instead.
        if repo.index()?.has_conflicts() {
            let count = repo
                .statuses(None)
                .map(|s| {
                    s.iter()
                        .filter(|e| e.status().contains(Status::CONFLICTED))
                        .count()
                })
                .unwrap_or(0);
            return Ok(OpOutcome::Conflicts { count });
        }
        let signature = repo.signature()?;
        rebase.commit(None, &signature, None)?;
    }

    rebase.finish(None)?;

    Ok(OpOutcome::Completed)
}

/// Fetch from a named remote using the git CLI.
pub fn fetch_remote(repo_path: &str, remote: &str) -> Result<()> {
    run_git(repo_path, &["fetch", remote])?;
    Ok(())
}

/// Fetch from the `origin` remote (thin wrapper over [`fetch_remote`]).
pub fn fetch_origin(repo_path: &str) -> Result<()> {
    fetch_remote(repo_path, "origin")
}

/// Fetch and integrate from a remote (`git pull`), honoring the user's
/// `pull.rebase` config.
///
/// With `remote`/`branch` = `None`, runs a bare `git pull`, which resolves the
/// current branch's configured upstream. With an explicit remote (+ branch),
/// runs `git pull <remote> <branch>` — used when no upstream is set yet.
///
/// A conflicting merge/rebase is reported as `OpOutcome::Conflicts` (leaving the
/// repo mid-operation for the guided resolve flow), not an error. The editor is
/// disabled so a merge-commit prompt never blocks the TUI.
pub fn pull(repo_path: &str, remote: Option<&str>, branch: Option<&str>) -> Result<OpOutcome> {
    let mut args = vec!["pull"];
    if let Some(r) = remote {
        args.push(r);
        if let Some(b) = branch {
            args.push(b);
        }
    }
    run_git_allow_conflict(repo_path, &args)
}

/// Prune stale remote-tracking refs for a remote (`git remote prune <remote>`).
pub fn prune_remote(repo_path: &str, remote: &str) -> Result<()> {
    run_git(repo_path, &["remote", "prune", remote])?;
    Ok(())
}

/// Cherry-pick a commit.
///
/// A conflict is reported as `OpOutcome::Conflicts` (leaving CHERRY_PICK_HEAD in
/// place), not an error.
pub fn cherry_pick(repo_path: &str, commit_oid: Oid) -> Result<OpOutcome> {
    run_git_allow_conflict(repo_path, &["cherry-pick", &commit_oid.to_string()])
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

/// Revert a commit without opening an editor.
///
/// A conflict is reported as `OpOutcome::Conflicts` (leaving REVERT_HEAD in
/// place), not an error.
pub fn revert_commit(repo_path: &str, commit_oid: Oid) -> Result<OpOutcome> {
    run_git_allow_conflict(repo_path, &["revert", "--no-edit", &commit_oid.to_string()])
}

/// Push the current branch to its configured upstream (bare `git push`).
///
/// Fails if the branch has no upstream — callers should route to
/// [`push_set_upstream`] (publish) in that case.
pub fn push_current(repo_path: &str) -> Result<()> {
    run_git(repo_path, &["push"])?;
    Ok(())
}

/// Publish `branch` to `remote`, setting it as the branch's upstream
/// (`git push -u <remote> <branch>`).
pub fn push_set_upstream(repo_path: &str, remote: &str, branch: &str) -> Result<()> {
    run_git(repo_path, &["push", "-u", remote, branch])?;
    Ok(())
}

/// Push current branch to origin (thin wrapper; `git push origin HEAD`).
pub fn push_to_origin(repo_path: &str) -> Result<()> {
    run_git(repo_path, &["push", "origin", "HEAD"])?;
    Ok(())
}

/// Delete a branch on a remote (`git push <remote> --delete <branch>`).
pub fn delete_remote_branch(repo_path: &str, remote: &str, branch: &str) -> Result<()> {
    run_git(repo_path, &["push", remote, "--delete", branch])?;
    Ok(())
}

/// Resolve a conflicted path by taking "our" side (stage 2) and staging it.
pub fn accept_ours(repo_path: &str, path: &str) -> Result<()> {
    run_git(repo_path, &["checkout", "--ours", "--", path])?;
    run_git(repo_path, &["add", "--", path])?;
    Ok(())
}

/// Resolve a conflicted path by taking "their" side (stage 3) and staging it.
pub fn accept_theirs(repo_path: &str, path: &str) -> Result<()> {
    run_git(repo_path, &["checkout", "--theirs", "--", path])?;
    run_git(repo_path, &["add", "--", path])?;
    Ok(())
}

/// Abort the in-progress operation, restoring the pre-operation state.
///
/// Rebase is aborted through libgit2 (`Rebase::abort`) because `rebase_branch`
/// starts it via libgit2, whose `.git/rebase-merge` layout the `git` CLI can't
/// drive. Merge/cherry-pick/revert use `git <op> --abort`.
pub fn abort_operation(repo_path: &str, op: OperationState) -> Result<()> {
    match op {
        OperationState::Rebase => {
            let repo = Repository::open(repo_path)?;
            let mut rebase = repo.open_rebase(None)?;
            rebase.abort()?;
            Ok(())
        }
        _ => {
            let Some(sub) = op.git_subcommand() else {
                bail!("No operation in progress to abort");
            };
            run_git(repo_path, &[sub, "--abort"])?;
            Ok(())
        }
    }
}

/// Continue the in-progress operation after conflicts are resolved.
///
/// If conflicts remain unresolved, returns `Ok(OpOutcome::Conflicts)` so the
/// caller can surface the shortfall; other failures bail with git's message.
/// Rebase continues through libgit2 (see `abort_operation`); the rest use
/// `git <op> --continue` with the editor disabled so it never blocks the TUI.
pub fn continue_operation(repo_path: &str, op: OperationState) -> Result<OpOutcome> {
    match op {
        OperationState::Rebase => continue_rebase(repo_path),
        _ => {
            let Some(sub) = op.git_subcommand() else {
                bail!("No operation in progress to continue");
            };
            run_git_allow_conflict(repo_path, &[sub, "--continue"])
        }
    }
}

/// Resume a libgit2 rebase left in progress: commit the resolved current patch,
/// then replay the rest until finished or the next conflict.
fn continue_rebase(repo_path: &str) -> Result<OpOutcome> {
    let repo = Repository::open(repo_path)?;
    // Unresolved conflicts still sitting in the index — nothing to commit yet.
    if repo.index()?.has_conflicts() {
        return Ok(OpOutcome::Conflicts {
            count: count_conflicts(repo_path),
        });
    }
    let mut rebase = repo.open_rebase(None)?;
    let signature = repo.signature()?;
    // Commit the patch that previously conflicted (now resolved + staged).
    rebase.commit(None, &signature, None)?;
    while let Some(op) = rebase.next() {
        op?;
        if repo.index()?.has_conflicts() {
            return Ok(OpOutcome::Conflicts {
                count: count_conflicts(repo_path),
            });
        }
        rebase.commit(None, &signature, None)?;
    }
    rebase.finish(None)?;
    Ok(OpOutcome::Completed)
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

/// Stage every change in the working tree, including untracked files and
/// deletions (`git add -A`).
pub fn stage_all(repo_path: &str) -> Result<()> {
    run_git(repo_path, &["add", "-A"])?;
    Ok(())
}

/// Unstage everything (`git reset`), leaving working-tree changes intact.
pub fn unstage_all(repo_path: &str) -> Result<()> {
    run_git(repo_path, &["reset"])?;
    Ok(())
}

/// Apply a unified-diff patch to the index (`git apply --cached`) — stages the
/// change described by `patch`.
pub fn apply_patch_cached(repo_path: &str, patch: &str) -> Result<()> {
    run_git_with_stdin(repo_path, &["apply", "--cached", "-"], patch.as_bytes())?;
    Ok(())
}

/// Reverse-apply a unified-diff patch to the index (`git apply --cached -R`) —
/// unstages the change described by `patch`.
pub fn apply_patch_cached_reverse(repo_path: &str, patch: &str) -> Result<()> {
    run_git_with_stdin(repo_path, &["apply", "--cached", "-R", "-"], patch.as_bytes())?;
    Ok(())
}

/// Reverse-apply a unified-diff patch to the working tree (`git apply -R`) —
/// discards the change described by `patch`. Destructive.
pub fn apply_patch_worktree_reverse(repo_path: &str, patch: &str) -> Result<()> {
    run_git_with_stdin(repo_path, &["apply", "-R", "-"], patch.as_bytes())?;
    Ok(())
}

/// Restore (discard changes to) the given files.
/// Tracked files are restored via `git checkout HEAD -- <path>`, which discards
/// both staged (index) and unstaged (working tree) changes — a hard reset of the file.
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

/// Stash all working-tree changes (`git stash push`), optionally including
/// untracked files (`-u`), with an optional message. Staged and unstaged
/// tracked changes are both captured; the working tree is left clean.
pub fn stash_all(repo_path: &str, message: &str, include_untracked: bool) -> Result<()> {
    let mut args: Vec<&str> = vec!["stash", "push"];
    if include_untracked {
        args.push("-u");
    }
    if !message.is_empty() {
        args.push("-m");
        args.push(message);
    }
    run_git(repo_path, &args)?;
    Ok(())
}

/// Create a branch from a stash entry and drop it (`git stash branch <name>
/// stash@{n}`). Git checks the new branch out at the stash's base commit,
/// re-applies the stashed changes, and drops the stash once it applies cleanly.
pub fn stash_branch(repo_path: &str, branch_name: &str, index: usize) -> Result<()> {
    let ref_name = format!("stash@{{{index}}}");
    run_git(repo_path, &["stash", "branch", branch_name, &ref_name])?;
    Ok(())
}

/// Rename a local branch (`git branch -m <old> <new>`). Works on the current
/// branch too — git moves HEAD to follow the rename. A name collision surfaces
/// as an error from git.
pub fn rename_branch(repo_path: &str, old_name: &str, new_name: &str) -> Result<()> {
    run_git(repo_path, &["branch", "-m", old_name, new_name])?;
    Ok(())
}

/// Delete a tag (`git tag -d <name>`).
pub fn delete_tag(repo_path: &str, tag_name: &str) -> Result<()> {
    run_git(repo_path, &["tag", "-d", tag_name])?;
    Ok(())
}

/// Push a tag to a remote (`git push <remote> <tag>`), making it visible there.
pub fn push_tag(repo_path: &str, remote: &str, tag_name: &str) -> Result<()> {
    run_git(repo_path, &["push", remote, tag_name])?;
    Ok(())
}

/// Get the message of the last commit.
pub fn get_last_commit_message(repo_path: &str) -> Result<String> {
    let output = run_git(repo_path, &["log", "-1", "--format=%B"])?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// List commit OIDs that touched `path`, newest first, following renames
/// (`git log --follow`). Capped at `limit` entries so a long-lived file's
/// history stays bounded.
pub fn file_history(repo_path: &str, path: &str, limit: usize) -> Result<Vec<Oid>> {
    let limit_str = limit.to_string();
    let output = run_git(
        repo_path,
        &["log", "--follow", "-n", &limit_str, "--format=%H", "--", path],
    )?;
    let text = String::from_utf8_lossy(&output.stdout);
    let oids = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| Oid::from_str(line).ok())
        .collect();
    Ok(oids)
}

/// Query the GPG signature status of a commit via `git log -1 --format=%G?`.
/// Returns the raw `%G?` status code (one of G/B/U/X/Y/R/E/N), defaulting to
/// `'N'` (unsigned) when git prints nothing.
pub fn commit_signature_status(repo_path: &str, oid: Oid) -> Result<char> {
    let oid_str = oid.to_string();
    let output = run_git(repo_path, &["log", "-1", "--format=%G?", &oid_str])?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.trim().chars().next().unwrap_or('N'))
}

/// Map a `%G?` signature status code to a short human-readable label.
/// Pure function (no I/O) — the display layer renders whatever this returns.
pub fn signature_status_label(code: char) -> &'static str {
    match code {
        'G' => "signed (valid)",
        'B' => "signed (BAD)",
        'U' => "signed (valid, unknown trust)",
        'X' => "signed (valid, expired)",
        'Y' => "signed (expired key)",
        'R' => "signed (revoked key)",
        'E' => "signature unverifiable",
        'N' => "unsigned",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::signature_status_label;

    #[test]
    fn signature_labels_cover_all_git_codes() {
        assert_eq!(signature_status_label('G'), "signed (valid)");
        assert_eq!(signature_status_label('B'), "signed (BAD)");
        assert_eq!(signature_status_label('U'), "signed (valid, unknown trust)");
        assert_eq!(signature_status_label('X'), "signed (valid, expired)");
        assert_eq!(signature_status_label('Y'), "signed (expired key)");
        assert_eq!(signature_status_label('R'), "signed (revoked key)");
        assert_eq!(signature_status_label('E'), "signature unverifiable");
        assert_eq!(signature_status_label('N'), "unsigned");
    }

    #[test]
    fn signature_label_unknown_code_is_labelled_unknown() {
        assert_eq!(signature_status_label('Z'), "unknown");
        assert_eq!(signature_status_label(' '), "unknown");
    }
}

