//! Git operations (checkout, merge, rebase, branch operations)

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use git2::{BranchType, Oid, Repository, Status, StatusOptions};

use super::askpass::{self, Credentials};
use super::repository::OperationState;

/// Attach the askpass shim + credential env vars to `cmd` when `creds` is set,
/// so a retried HTTPS git op authenticates without a terminal prompt. A no-op
/// when `creds` is `None` (the normal, uncredentialed path).
fn apply_credentials(cmd: &mut Command, creds: Option<&Credentials>) -> Result<()> {
    if let Some(c) = creds {
        let shim = askpass::ensure_askpass_shim()?;
        cmd.env("GIT_ASKPASS", shim)
            .env(askpass::ENV_USER, &c.username)
            .env(askpass::ENV_PASS, &c.password);
    }
    Ok(())
}

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
    run_git_creds(repo_path, args, None)
}

/// As [`run_git`], but supplies HTTPS credentials to the child via `GIT_ASKPASS`
/// when `creds` is set. Used by the network ops that the credential-prompt flow
/// retries.
fn run_git_creds(
    repo_path: &str,
    args: &[&str],
    creds: Option<&Credentials>,
) -> Result<std::process::Output> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(repo_path)
        // Never let auth block on /dev/tty: a missing credential becomes an
        // instant "could not read Username" error instead of a silent hang that
        // wedges the background op forever.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    apply_credentials(&mut cmd, creds)?;
    let output = cmd
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
    run_git_allow_conflict_creds(repo_path, args, None)
}

/// As [`run_git_allow_conflict`], but supplies HTTPS credentials via
/// `GIT_ASKPASS` when set (used by the credential-prompt pull retry).
fn run_git_allow_conflict_creds(
    repo_path: &str,
    args: &[&str],
    creds: Option<&Credentials>,
) -> Result<OpOutcome> {
    let subcommand = args.first().copied().unwrap_or("");
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(repo_path)
        // Never block the TUI on an editor prompt.
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        // Never block on a credential prompt (see run_git).
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    apply_credentials(&mut cmd, creds)?;
    let output = cmd
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
    // Peel "<remote>/branch-name" into its branch part using the repo's actual
    // remotes, so a branch tracked from a non-`origin` remote (e.g. `upstream`)
    // resolves correctly instead of failing a hardcoded "origin/" strip.
    let remotes: Vec<String> = repo
        .remotes()
        .map(|arr| arr.iter().flatten().map(String::from).collect())
        .unwrap_or_default();
    let (_remote, local_name) = crate::git::split_remote_ref(&remotes, remote_branch)
        .context("Invalid remote branch format")?;
    let local_name = local_name.as_str();

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

/// Delete a *local* branch.
///
/// Intentionally `BranchType::Local` only: deleting a local branch
/// (`git branch -d`) and deleting a remote-tracking branch's upstream
/// (`git push <remote> --delete`) are different operations with different
/// blast radii, so the UI routes them separately before either function is
/// called — `App::confirm_delete_branch` sends remote-tracking names to
/// `ConfirmAction::DeleteRemoteBranch` (a push, not this function) and only
/// reaches this one for `is_remote == false`. If that ever changes and a
/// remote-tracking name reaches here, the "not found" error below is
/// accurate: it genuinely doesn't exist under `refs/heads/*`.
pub fn delete_branch(repo: &Repository, branch_name: &str) -> Result<()> {
    let mut branch = repo
        .find_branch(branch_name, BranchType::Local)
        .context(format!("Local branch '{}' not found", branch_name))?;

    if branch.is_head() {
        bail!("Cannot delete current branch");
    }

    branch.delete()?;
    Ok(())
}

/// Perform a merge.
///
/// `branch_type` picks which ref namespace `branch_name` resolves in
/// (`refs/heads/*` vs `refs/remotes/*`). It must be supplied explicitly by
/// the caller rather than guessed from the name — the UI already knows this
/// from the selected `BranchInfo::is_remote` at selection time (see
/// `ConfirmAction::Merge`). A remote-tracking name like `origin/dev` can
/// only ever resolve under `BranchType::Remote`; passing `BranchType::Local`
/// for one always fails with "not found" regardless of how fresh the repo
/// handle is (see the regression tests below for #46).
///
/// On a conflicting normal merge this returns `Ok(OpOutcome::Conflicts)` and
/// deliberately leaves the repo mid-merge (conflicted index + MERGE_HEAD), so
/// the caller can offer resolve/continue/abort. It is NOT an error.
pub fn merge_branch(repo: &Repository, branch_name: &str, branch_type: BranchType) -> Result<OpOutcome> {
    let branch = repo
        .find_branch(branch_name, branch_type)
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

        // Create a merge commit. Message follows git's own convention: a
        // remote-tracking source is called out distinctly from a local branch
        // (`git merge origin/dev` produces "Merge remote-tracking branch
        // 'origin/dev'"; `git merge dev` produces "Merge branch 'dev'").
        let signature = repo.signature()?;
        let head = repo.head()?;
        let head_commit = head.peel_to_commit()?;
        let merge_commit = repo.find_commit(annotated_commit.id())?;
        let tree_oid = repo.index()?.write_tree()?;
        let tree = repo.find_tree(tree_oid)?;
        let message = match branch_type {
            BranchType::Remote => format!("Merge remote-tracking branch '{}'", branch_name),
            BranchType::Local => format!("Merge branch '{}'", branch_name),
        };

        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            &message,
            &tree,
            &[&head_commit, &merge_commit],
        )?;

        repo.cleanup_state()?;
    }

    Ok(OpOutcome::Completed)
}

/// Summary of a [`fast_forward_behind_branches`] sweep.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FastForwardSummary {
    /// Local branch names moved up to their upstream tip.
    pub moved: Vec<String>,
    /// `(branch, error)` for branches whose fast-forward failed. Collected, not
    /// fatal — one bad branch never aborts the rest of the sweep.
    pub failed: Vec<(String, String)>,
}

impl FastForwardSummary {
    /// Nothing moved and nothing failed (so no summary is warranted).
    pub fn is_empty(&self) -> bool {
        self.moved.is_empty() && self.failed.is_empty()
    }
}

/// Whether the working tree is clean enough to fast-forward the checked-out
/// branch: no staged or unstaged changes to tracked files and no conflicts.
///
/// Untracked and ignored files are allowed — this mirrors `git merge --ff-only`,
/// which succeeds with untracked files present. Should a checkout actually need
/// to clobber an untracked file, libgit2's default SAFE checkout refuses and
/// fails that single branch, rather than this precheck blocking every branch
/// whenever any stray untracked file exists.
pub fn working_tree_clean_for_ff(repo: &Repository) -> Result<bool> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(false).include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts))?;
    Ok(statuses.is_empty())
}

/// Fast-forward every local branch that is strictly behind its upstream
/// (`ahead == 0 && behind > 0`), leaving diverged, ahead, and up-to-date
/// branches untouched.
///
/// - A non-checked-out branch moves by a pure ref update (retargeted to the
///   upstream tip; no working tree is involved, so it is always safe).
/// - The checked-out branch is fast-forwarded properly (ref update + working-
///   tree checkout, reusing [`merge_branch`]'s fast-forward path) only when the
///   working tree is clean; a dirty tree skips it silently.
///
/// Per-branch failures are collected into [`FastForwardSummary::failed`] and
/// never abort the remaining branches. Callers refresh afterward so the moved
/// branches render at their new tips.
pub fn fast_forward_behind_branches(repo: &Repository) -> FastForwardSummary {
    let mut summary = FastForwardSummary::default();

    // The checked-out branch's short name, when HEAD is on a branch (not detached).
    let head_branch = repo
        .head()
        .ok()
        .filter(|h| h.is_branch())
        .and_then(|h| h.shorthand().map(str::to_string));

    // Snapshot local branch names first so we don't hold the branches iterator
    // (an immutable repo borrow) across the mutating fast-forwards below.
    let names: Vec<String> = match repo.branches(Some(BranchType::Local)) {
        Ok(iter) => iter
            .filter_map(std::result::Result::ok)
            .filter_map(|(b, _)| b.name().ok().flatten().map(str::to_string))
            .collect(),
        Err(_) => return summary,
    };

    // Only relevant for the checked-out branch; computed once.
    let clean = working_tree_clean_for_ff(repo).unwrap_or(false);

    for name in names {
        let is_checked_out = head_branch.as_deref() == Some(name.as_str());
        match fast_forward_one_behind(repo, &name, is_checked_out, clean) {
            Ok(true) => summary.moved.push(name),
            Ok(false) => {}
            Err(e) => summary.failed.push((name, e.to_string())),
        }
    }
    summary
}

/// Fast-forward a single local branch iff it is strictly behind its upstream.
///
/// Returns `Ok(true)` when it moved, `Ok(false)` when it was ineligible (no
/// upstream, not strictly behind) or a dirty checked-out branch was skipped, and
/// `Err` on an actual failure.
fn fast_forward_one_behind(
    repo: &Repository,
    name: &str,
    is_checked_out: bool,
    working_tree_clean: bool,
) -> Result<bool> {
    let branch = repo.find_branch(name, BranchType::Local)?;
    let local_oid = branch
        .get()
        .target()
        .context("local branch has no target")?;

    // Only branches that track an upstream are eligible.
    let Ok(upstream) = branch.upstream() else {
        return Ok(false);
    };
    let Some(up_oid) = upstream.get().target() else {
        return Ok(false);
    };

    // Strictly behind: no local-only commits, at least one upstream-only commit.
    let (ahead, behind) = repo.graph_ahead_behind(local_oid, up_oid)?;
    if ahead != 0 || behind == 0 {
        return Ok(false);
    }

    if is_checked_out {
        // A dirty working tree is skipped silently, not reported as a failure.
        if !working_tree_clean {
            return Ok(false);
        }
        // Reuse the merge machinery's fast-forward path (ref + working-tree
        // checkout) rather than reimplementing it. For a strictly-behind branch
        // `git merge <upstream>` is always a fast-forward.
        let up_name = upstream
            .name()?
            .context("upstream ref has no name")?
            .to_string();
        let up_type = if upstream.get().is_remote() {
            BranchType::Remote
        } else {
            BranchType::Local
        };
        match merge_branch(repo, &up_name, up_type)? {
            OpOutcome::Completed => Ok(true),
            OpOutcome::Conflicts { .. } => {
                bail!("unexpected conflicts fast-forwarding '{name}'")
            }
        }
    } else {
        // Pure ref update: retarget the local branch to the upstream tip. No
        // working tree is touched, so this is always safe.
        let mut reference = branch.into_reference();
        reference.set_target(up_oid, &format!("fast-forward: {name} -> upstream"))?;
        Ok(true)
    }
}

/// Perform a rebase (simple implementation).
///
/// `branch_type` picks which ref namespace `onto_branch` resolves in, for the
/// same reason as [`merge_branch`] — a remote-tracking name like `origin/dev`
/// only exists under `BranchType::Remote`, and the caller (the UI) already
/// knows which from the selected `BranchInfo::is_remote`.
///
/// On a conflicting step this returns `Ok(OpOutcome::Conflicts)` and leaves the
/// rebase in progress (REBASE_HEAD etc.) for resolve/continue/abort — it does
/// NOT abort automatically.
pub fn rebase_branch(repo: &Repository, onto_branch: &str, branch_type: BranchType) -> Result<OpOutcome> {
    let onto = repo
        .find_branch(onto_branch, branch_type)
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

/// Fetch from a named remote using the git CLI. `creds` supplies HTTPS
/// credentials on a retry after an auth-failure prompt (`None` normally).
///
/// Uses `--prune` so remote-tracking refs (`<remote>/<branch>`) for branches
/// deleted upstream are removed locally, matching `git fetch --prune`. Without
/// it, stale `origin/<branch>` refs linger in the graph indefinitely after the
/// branch is gone from the remote.
pub fn fetch_remote(repo_path: &str, remote: &str, creds: Option<&Credentials>) -> Result<()> {
    run_git_creds(repo_path, &["fetch", "--prune", remote], creds)?;
    Ok(())
}

/// Fetch from the `origin` remote (thin wrapper over [`fetch_remote`]).
pub fn fetch_origin(repo_path: &str) -> Result<()> {
    fetch_remote(repo_path, "origin", None)
}

/// List every configured remote's name (`git remote`), in git's own order.
/// An empty vec means the repo has no remotes configured.
fn list_remotes(repo_path: &str) -> Result<Vec<String>> {
    let output = run_git(repo_path, &["remote"])?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Fetch from every configured remote, pruning stale remote-tracking refs.
///
/// Unlike `git fetch --all --prune` — a single process that exits non-zero if
/// *any* remote fails, collapsing partial success into one opaque error — this
/// fetches each remote independently and continues past failures. Every
/// reachable remote's tracking refs are updated on disk regardless of the
/// others, so one broken remote can no longer mask a fetch that a healthy
/// remote already completed (issue #91).
///
/// Returns `Ok(())` when every remote succeeded — and when none are configured.
/// On any failure, returns an error naming each failed remote and its trimmed
/// stderr (e.g. `fetch failed for upstream: <err>; for foo: <err>`); the
/// remotes that succeeded have already updated their refs on disk.
pub fn fetch_all(repo_path: &str, creds: Option<&Credentials>) -> Result<()> {
    let remotes = list_remotes(repo_path)?;
    let failures: Vec<String> = remotes
        .iter()
        .filter_map(|remote| {
            fetch_remote(repo_path, remote, creds)
                .err()
                .map(|e| format!("{remote}: {}", e.to_string().trim()))
        })
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        bail!("fetch failed for {}", failures.join("; for "));
    }
}

/// How a pull reconciles divergent branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullMode {
    /// `--ff-only`: refuse to merge/rebase; fail loudly on divergence (default).
    FfOnly,
    /// `--no-rebase`: create a merge commit.
    Merge,
    /// `--rebase`: replay local commits on top of the remote.
    Rebase,
}

impl PullMode {
    fn arg(self) -> &'static str {
        match self {
            PullMode::FfOnly => "--ff-only",
            PullMode::Merge => "--no-rebase",
            PullMode::Rebase => "--rebase",
        }
    }
}

/// Fetch and integrate from a remote (`git pull`) with an explicit `mode`.
///
/// The default is `PullMode::FfOnly`: with `pull.rebase`/`pull.ff` unset (git
/// 2.27+) a bare `git pull` on divergent branches aborts with "Need to specify
/// how to reconcile divergent branches". `--ff-only` makes that a clean,
/// catchable failure the caller turns into a merge/rebase prompt.
///
/// With `remote`/`branch` = `None`, resolves the current branch's configured
/// upstream. With an explicit remote (+ branch), runs `git pull <flag> <remote>
/// <branch>` — used when no upstream is set yet.
///
/// A conflicting merge/rebase is reported as `OpOutcome::Conflicts` (leaving the
/// repo mid-operation for the guided resolve flow), not an error. The editor is
/// disabled so a merge-commit prompt never blocks the TUI.
pub fn pull(
    repo_path: &str,
    remote: Option<&str>,
    branch: Option<&str>,
    mode: PullMode,
    creds: Option<&Credentials>,
) -> Result<OpOutcome> {
    let mut args = vec!["pull", mode.arg()];
    if let Some(r) = remote {
        args.push(r);
        if let Some(b) = branch {
            args.push(b);
        }
    }
    run_git_allow_conflict_creds(repo_path, &args, creds)
}

/// Whether a `git pull --ff-only` failure is due to divergent branches (offer
/// merge/rebase) rather than a hard error.
pub fn is_divergent_pull_error(stderr: &str) -> bool {
    stderr.contains("Not possible to fast-forward")
        || stderr.contains("Need to specify how to reconcile")
        || stderr.contains("fatal: Not possible")
}

/// Whether a pull failure is due to uncommitted local changes blocking the
/// merge/rebase (dirty worktree or index) — an expected, user-actionable
/// condition (commit or stash), not a hard error. Covers both the merge form
/// ("would be overwritten by merge ... commit or stash") and the rebase forms
/// ("cannot pull with rebase: You have unstaged changes / Your index contains
/// uncommitted changes").
pub fn is_dirty_worktree_pull_error(stderr: &str) -> bool {
    stderr.contains("would be overwritten")
        || stderr.contains("cannot pull with rebase")
        || stderr.contains("commit or stash them")
        || stderr.contains("You have unstaged changes")
        || stderr.contains("index contains uncommitted changes")
}

/// Extract the missing ref name from a "couldn't find remote ref <ref>" error.
fn parse_missing_ref(stderr: &str) -> Option<String> {
    stderr
        .split("couldn't find remote ref ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .map(|s| s.trim_end_matches(['\n', '.', '\'']).to_string())
        .filter(|s| !s.is_empty())
}

/// Map known git failure stderr to an actionable one-liner, or `None` when
/// unrecognized (the caller then shows the raw message).
pub fn humanize_git_error(stderr: &str) -> Option<String> {
    if stderr.contains("could not read Username")
        || stderr.contains("Authentication failed")
        || stderr.contains("Permission denied (publickey")
    {
        return Some(
            "Authentication failed — check your credentials / SSH key (e.g. `gh auth login`)"
                .to_string(),
        );
    }
    if stderr.contains("couldn't find remote ref") {
        return Some(match parse_missing_ref(stderr) {
            Some(r) => format!("Remote has no branch '{r}' — it may be renamed or not pushed yet"),
            None => {
                "That branch is missing on the remote — it may be renamed or not pushed yet"
                    .to_string()
            }
        });
    }
    if stderr.contains("would be overwritten by merge")
        || stderr.contains("Your local changes to the following files would be overwritten")
        || stderr.contains("cannot pull with rebase")
    {
        return Some("Local changes would be overwritten — commit or stash them first".to_string());
    }
    if stderr.contains("index.lock") {
        return Some(
            "Another git operation is in progress (index.lock) — wait for it to finish".to_string(),
        );
    }
    None
}

/// Whether a git failure is an **HTTPS** auth failure a username/token prompt
/// could fix. SSH publickey failures are deliberately excluded — no password
/// prompt can satisfy them — so the caller keeps showing them as plain errors.
pub fn is_https_auth_failure(stderr: &str) -> bool {
    if stderr.contains("Permission denied (publickey") {
        return false;
    }
    stderr.contains("could not read Username") || stderr.contains("Authentication failed")
}

/// Host + optional embedded username parsed from a git remote URL or an
/// auth-failure message. The host is the per-session credential-cache key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthUrl {
    pub host: String,
    pub user: Option<String>,
}

/// Host of an `https://[user@]host[:port]/…` URL (scheme optional), lowercased.
/// `None` if no host is present.
pub fn url_host(url: &str) -> Option<String> {
    let authority_and_path = url.split("://").last().unwrap_or(url);
    let authority = authority_and_path.split('/').next().unwrap_or(authority_and_path);
    // Drop any `user@` prefix and `:port` suffix.
    let host = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// Pull the host (and any `user@` component) out of an HTTPS git auth-failure
/// message, e.g. `could not read Username for 'https://github.com': …` or
/// `Authentication failed for 'https://alice@github.example/…'`. Returns `None`
/// when no `https://` URL is present.
pub fn extract_auth_url(stderr: &str) -> Option<AuthUrl> {
    let start = stderr.find("https://")? + "https://".len();
    let rest = &stderr[start..];
    // The URL ends at the closing quote, whitespace, or end of string.
    let end = rest
        .find(|c: char| c == '\'' || c == '"' || c.is_whitespace())
        .unwrap_or(rest.len());
    let authority = rest[..end].split('/').next().unwrap_or("");
    let user = authority
        .rsplit_once('@')
        .map(|(u, _)| u.to_string())
        .filter(|u| !u.is_empty());
    let host = url_host(&rest[..end])?;
    Some(AuthUrl { host, user })
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

/// Whether the working tree is clean enough to hard-reset over: no staged or
/// modified/deleted tracked files. Untracked and ignored files are allowed —
/// `reset --hard` preserves them.
pub fn is_working_tree_clean(repo: &Repository) -> Result<bool> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(false).include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts))?;
    Ok(statuses.iter().all(|e| e.status() == Status::CURRENT))
}

/// Hard-reset HEAD to `oid` — but only when the working tree is clean, so undo
/// never clobbers uncommitted work. The guard lives here (not just at the call
/// site) so every caller is protected.
pub fn reset_hard_checked(repo: &Repository, oid: Oid) -> Result<()> {
    if !is_working_tree_clean(repo)? {
        bail!("working tree has uncommitted changes");
    }
    let obj = repo.find_object(oid, None).context("Target commit not found")?;
    repo.reset(&obj, git2::ResetType::Hard, None)?;
    Ok(())
}

/// Create a pure lightweight tag (a ref straight to `commit_oid`), used to
/// restore a deleted tag. Annotated tags are downgraded to lightweight — we
/// don't reconstruct the original tag object/message.
pub fn create_lightweight_tag(repo: &Repository, tag_name: &str, commit_oid: Oid) -> Result<()> {
    let obj = repo.find_object(commit_oid, None).context("Commit not found")?;
    repo.tag_lightweight(tag_name, &obj, false)
        .context(format!("Failed to recreate tag '{}'", tag_name))?;
    Ok(())
}

/// Whether `refs/tags/<name>` is an annotated tag (points at a tag object) vs a
/// lightweight tag (points straight at a commit).
pub fn is_annotated_tag(repo: &Repository, tag_name: &str) -> bool {
    repo.find_reference(&format!("refs/tags/{tag_name}"))
        .ok()
        .and_then(|r| r.target())
        .and_then(|oid| repo.find_object(oid, None).ok())
        .map(|o| o.kind() == Some(git2::ObjectType::Tag))
        .unwrap_or(false)
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
pub fn push_current(repo_path: &str, creds: Option<&Credentials>) -> Result<()> {
    run_git_creds(repo_path, &["push"], creds)?;
    Ok(())
}

/// Publish `branch` to `remote`, setting it as the branch's upstream
/// (`git push -u <remote> <branch>`).
pub fn push_set_upstream(
    repo_path: &str,
    remote: &str,
    branch: &str,
    creds: Option<&Credentials>,
) -> Result<()> {
    run_git_creds(repo_path, &["push", "-u", remote, branch], creds)?;
    Ok(())
}

/// Push HEAD to an explicit `remote` **without** changing the branch's upstream
/// tracking (`git push <remote> HEAD`). Used when the user picks a remote other
/// than the configured upstream from the push remote-picker.
pub fn push_head_to_remote(
    repo_path: &str,
    remote: &str,
    creds: Option<&Credentials>,
) -> Result<()> {
    run_git_creds(repo_path, &["push", remote, "HEAD"], creds)?;
    Ok(())
}

/// Push current branch to origin (thin wrapper; `git push origin HEAD`).
pub fn push_to_origin(repo_path: &str) -> Result<()> {
    run_git(repo_path, &["push", "origin", "HEAD"])?;
    Ok(())
}

/// Delete a branch on a remote (`git push <remote> --delete <branch>`).
///
/// Credentials-aware so it flows through the same async op + auth-retry pipeline
/// as a normal push ([`crate::app::PushSpec::Delete`]).
pub fn push_delete(
    repo_path: &str,
    remote: &str,
    branch: &str,
    creds: Option<&Credentials>,
) -> Result<()> {
    run_git_creds(repo_path, &["push", remote, "--delete", branch], creds)?;
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

/// Apply a stash without removing it. A conflicting apply is a first-class
/// `Conflicts` outcome, not an error: git leaves the working tree with unmerged
/// paths but — unlike merge/rebase — sets no MERGE_HEAD, so no operation is left
/// "in progress". Apply always keeps the stash entry regardless of the outcome.
pub fn stash_apply(repo_path: &str, index: usize) -> Result<OpOutcome> {
    let ref_name = format!("stash@{{{index}}}");
    run_git_allow_conflict(repo_path, &["stash", "apply", &ref_name])
}

/// Pop a stash (apply, then drop it on success). A conflicting pop returns
/// `Conflicts` and — as git itself notes ("The stash entry is kept in case you
/// need it again") — leaves the stash entry in place; the user resolves the
/// conflicts and drops it manually. No MERGE_HEAD is set, so there is no
/// "continue" step.
pub fn stash_pop(repo_path: &str, index: usize) -> Result<OpOutcome> {
    let ref_name = format!("stash@{{{index}}}");
    run_git_allow_conflict(repo_path, &["stash", "pop", &ref_name])
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

#[cfg(test)]
mod tests {
    use super::{
        extract_auth_url, humanize_git_error, is_dirty_worktree_pull_error,
        is_divergent_pull_error, is_https_auth_failure, url_host, AuthUrl,
        OpOutcome, PullMode,
    };

    #[test]
    fn https_auth_failure_detected_but_not_ssh() {
        // HTTPS credential prompts a token can fix.
        assert!(is_https_auth_failure(
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled"
        ));
        assert!(is_https_auth_failure(
            "remote: Support for password authentication was removed.\nfatal: Authentication failed for 'https://github.com/o/r.git/'"
        ));
        // SSH publickey failure — a token can't fix it, so it's NOT an HTTPS
        // auth failure even though libgit2 words it as a denial.
        assert!(!is_https_auth_failure(
            "git@github.com: Permission denied (publickey).\nfatal: Could not read from remote repository."
        ));
        // Unrelated errors.
        assert!(!is_https_auth_failure("fatal: Not possible to fast-forward"));
        assert!(!is_https_auth_failure("Already up to date."));
    }

    #[test]
    fn extract_auth_url_pulls_host_and_optional_user() {
        assert_eq!(
            extract_auth_url("fatal: could not read Username for 'https://github.com': x"),
            Some(AuthUrl { host: "github.com".into(), user: None })
        );
        // Embedded user@ prefills the username prompt; path is ignored.
        assert_eq!(
            extract_auth_url("fatal: Authentication failed for 'https://alice@git.example.com/o/r.git/'"),
            Some(AuthUrl { host: "git.example.com".into(), user: Some("alice".into()) })
        );
        // No URL present.
        assert_eq!(extract_auth_url("git@github.com: Permission denied (publickey)."), None);
    }

    #[test]
    fn url_host_strips_scheme_user_and_port() {
        assert_eq!(url_host("https://github.com/o/r.git").as_deref(), Some("github.com"));
        assert_eq!(url_host("https://alice@github.com:443/o/r").as_deref(), Some("github.com"));
        assert_eq!(url_host("github.com/o/r").as_deref(), Some("github.com"));
        assert_eq!(url_host("https://").as_deref(), None);
    }

    #[test]
    fn pull_mode_maps_to_reconcile_flags() {
        assert_eq!(PullMode::FfOnly.arg(), "--ff-only");
        assert_eq!(PullMode::Merge.arg(), "--no-rebase");
        assert_eq!(PullMode::Rebase.arg(), "--rebase");
    }

    #[test]
    fn divergence_predicate_matches_reconcile_failures() {
        assert!(is_divergent_pull_error(
            "fatal: Not possible to fast-forward, aborting."
        ));
        assert!(is_divergent_pull_error(
            "hint: You have divergent branches...\nfatal: Need to specify how to reconcile divergent branches."
        ));
        assert!(is_divergent_pull_error("fatal: Not possible"));
        // A normal auth failure is NOT divergence.
        assert!(!is_divergent_pull_error(
            "fatal: Authentication failed for 'https://github.com/o/r'"
        ));
        assert!(!is_divergent_pull_error("Already up to date."));
    }

    #[test]
    fn dirty_worktree_predicate_matches_merge_and_rebase_forms() {
        // Merge form.
        assert!(is_dirty_worktree_pull_error(
            "error: Your local changes to the following files would be overwritten by merge:\n\ta.txt\nPlease commit your changes or stash them before you merge.\nAborting"
        ));
        // Rebase forms (dirty worktree / dirty index).
        assert!(is_dirty_worktree_pull_error(
            "error: cannot pull with rebase: You have unstaged changes."
        ));
        assert!(is_dirty_worktree_pull_error(
            "error: cannot pull with rebase: Your index contains uncommitted changes."
        ));
        // Divergence and auth failures are NOT dirty-worktree conditions.
        assert!(!is_dirty_worktree_pull_error(
            "fatal: Not possible to fast-forward, aborting."
        ));
        assert!(!is_dirty_worktree_pull_error(
            "fatal: Authentication failed for 'https://github.com/o/r'"
        ));
        assert!(!is_dirty_worktree_pull_error("Already up to date."));
    }

    #[test]
    fn humanize_maps_known_failures_to_guidance() {
        // Auth (both HTTPS credential and SSH key forms).
        assert!(humanize_git_error("fatal: could not read Username for 'https://github.com'")
            .unwrap()
            .contains("Authentication failed"));
        assert!(humanize_git_error("remote: Permission denied (publickey).")
            .unwrap()
            .contains("Authentication failed"));
        // Missing remote ref names the branch.
        let missing = humanize_git_error("fatal: couldn't find remote ref feature/x").unwrap();
        assert!(missing.contains("feature/x"), "names the branch: {missing}");
        // Would-be-overwritten local changes.
        assert!(humanize_git_error(
            "error: Your local changes to the following files would be overwritten by merge:\n\tsrc/main.rs"
        )
        .unwrap()
        .contains("commit or stash"));
        // Rebase-mode dirty worktree gets the same commit-or-stash guidance.
        assert!(humanize_git_error("error: cannot pull with rebase: You have unstaged changes.")
            .unwrap()
            .contains("commit or stash"));
        // index.lock.
        assert!(humanize_git_error(
            "fatal: Unable to create '/repo/.git/index.lock': File exists."
        )
        .unwrap()
        .contains("in progress"));
        // Unrecognized -> None (caller falls back to raw).
        assert_eq!(humanize_git_error("some unexpected failure"), None);
    }

    use super::{fetch_all, fetch_remote};
    use crate::test_support::git;
    use git2::{BranchType, Repository};
    use std::process::Command;

    /// Init a bare remote at `path`.
    fn init_bare(path: &std::path::Path) {
        Command::new("git").args(["init", "-q", "--bare"]).arg(path).status().unwrap();
    }

    /// Init a working repo with an isolated identity and one commit on `master`.
    fn init_repo_with_commit(path: &std::path::Path) {
        Command::new("git").args(["init", "-q"]).arg(path).status().unwrap();
        git(path, &["config", "user.email", "t@t.com"]);
        git(path, &["config", "user.name", "t"]);
        std::fs::write(path.join("a.txt"), "a").unwrap();
        git(path, &["add", "a.txt"]);
        git(path, &["commit", "-qm", "init"]);
    }

    /// Clone `remote` to `dst`, then push a fresh branch `branch` carrying a new
    /// commit — so the *original* clone's `<remote>/<branch>` tracking ref does
    /// not yet exist and only appears after it fetches.
    fn push_new_branch_via_clone(remote: &std::path::Path, dst: &std::path::Path, branch: &str) {
        Command::new("git").args(["clone", "-q"]).arg(remote).arg(dst).status().unwrap();
        git(dst, &["config", "user.email", "t@t.com"]);
        git(dst, &["config", "user.name", "t"]);
        git(dst, &["checkout", "-qb", branch]);
        std::fs::write(dst.join("b.txt"), branch).unwrap();
        git(dst, &["add", "b.txt"]);
        git(dst, &["commit", "-qm", "advance"]);
        git(dst, &["push", "-q", "origin", branch]);
    }

    /// #91: `fetch_all` fetches remotes independently — a broken remote must not
    /// prevent a *healthy* remote's tracking refs from being updated on disk, and
    /// the returned error must name only the remote that actually failed.
    #[test]
    fn fetch_all_updates_good_remote_despite_failing_remote() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("good.git");
        let local = tmp.path().join("local");
        init_bare(&good);
        init_repo_with_commit(&local);
        git(&local, &["remote", "add", "origin", good.to_str().unwrap()]);
        git(&local, &["push", "-q", "origin", "master"]);
        // A second remote pointing at a path that does not exist — every fetch of
        // it fails hard.
        let missing = tmp.path().join("nonexistent-remote.git");
        git(&local, &["remote", "add", "broken", missing.to_str().unwrap()]);

        // Publish a new branch to the good remote from another clone, so the
        // local repo has no `origin/feature` tracking ref yet.
        push_new_branch_via_clone(&good, &tmp.path().join("work"), "feature");
        let repo = Repository::open(&local).unwrap();
        assert!(
            repo.find_branch("origin/feature", BranchType::Remote).is_err(),
            "precondition: origin/feature absent before fetch_all"
        );

        let err = fetch_all(local.to_str().unwrap(), None).unwrap_err();

        // The regression: the healthy remote's ref updated even though the call
        // returned Err.
        assert!(
            repo.find_branch("origin/feature", BranchType::Remote).is_ok(),
            "origin/feature must be updated on disk despite the broken remote"
        );
        let msg = err.to_string();
        assert!(msg.contains("broken"), "error must name the failed remote: {msg}");
        assert!(
            !msg.contains("origin"),
            "error must not name the remote that succeeded: {msg}"
        );
    }

    /// All remotes healthy: `fetch_all` returns Ok and updates every remote's
    /// tracking refs — including a branch that is not checked out locally.
    #[test]
    fn fetch_all_all_good_updates_every_remote() {
        let tmp = tempfile::tempdir().unwrap();
        let remote_a = tmp.path().join("a.git");
        let remote_b = tmp.path().join("b.git");
        let local = tmp.path().join("local");
        init_bare(&remote_a);
        init_bare(&remote_b);
        init_repo_with_commit(&local);
        git(&local, &["remote", "add", "origin", remote_a.to_str().unwrap()]);
        git(&local, &["remote", "add", "upstream", remote_b.to_str().unwrap()]);
        git(&local, &["push", "-q", "origin", "master"]);
        git(&local, &["push", "-q", "upstream", "master"]);

        // A branch on each remote that local has never checked out or fetched.
        push_new_branch_via_clone(&remote_a, &tmp.path().join("wa"), "feat-a");
        push_new_branch_via_clone(&remote_b, &tmp.path().join("wb"), "feat-b");

        let repo = Repository::open(&local).unwrap();
        assert!(repo.find_branch("origin/feat-a", BranchType::Remote).is_err());
        assert!(repo.find_branch("upstream/feat-b", BranchType::Remote).is_err());

        fetch_all(local.to_str().unwrap(), None).unwrap();

        assert!(
            repo.find_branch("origin/feat-a", BranchType::Remote).is_ok(),
            "origin/feat-a (not checked out locally) must be fetched"
        );
        assert!(
            repo.find_branch("upstream/feat-b", BranchType::Remote).is_ok(),
            "upstream/feat-b (not checked out locally) must be fetched"
        );
    }

    /// A repo with zero configured remotes is a no-op success, not an error.
    #[test]
    fn fetch_all_no_remotes_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("local");
        init_repo_with_commit(&local);
        fetch_all(local.to_str().unwrap(), None).unwrap();
    }

    #[test]
    fn fetch_remote_prunes_deleted_remote_tracking_refs() {
        // A branch deleted upstream by *another* clone leaves this clone's
        // origin/<branch> ref dangling; fetch must prune it (git fetch --prune).
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        let local = tmp.path().join("local");
        Command::new("git").args(["init", "-q", "--bare"]).arg(&remote).status().unwrap();
        Command::new("git").args(["init", "-q"]).arg(&local).status().unwrap();
        git(&local, &["config", "user.email", "t@t.com"]);
        git(&local, &["config", "user.name", "t"]);
        git(&local, &["remote", "add", "origin", remote.to_str().unwrap()]);
        std::fs::write(local.join("a.txt"), "a").unwrap();
        git(&local, &["add", "a.txt"]);
        git(&local, &["commit", "-qm", "init"]);
        git(&local, &["branch", "feature"]);
        git(&local, &["push", "-q", "origin", "master", "feature"]);

        let repo = Repository::open(&local).unwrap();
        assert!(
            repo.find_branch("origin/feature", BranchType::Remote).is_ok(),
            "origin/feature present after push"
        );

        // A different clone deletes the branch on the remote. Deleting from our
        // own clone would auto-remove the tracking ref and not exercise prune.
        let other = tmp.path().join("other");
        Command::new("git").args(["clone", "-q"]).arg(&remote).arg(&other).status().unwrap();
        git(&other, &["push", "-q", "origin", "--delete", "feature"]);

        // Still stale locally until we fetch.
        assert!(repo.find_branch("origin/feature", BranchType::Remote).is_ok());

        fetch_remote(local.to_str().unwrap(), "origin", None).unwrap();

        assert!(
            repo.find_branch("origin/feature", BranchType::Remote).is_err(),
            "origin/feature must be pruned after upstream deletion + fetch"
        );
    }

    /// Regression coverage for GitHub issue #46 ("merge origin/dev" fails with
    /// "not found" in the TUI while `git merge origin/dev` works in a shell).
    ///
    /// The issue's working hypothesis was that the TUI's long-lived
    /// `git2::Repository` handle caches refs, so a remote-tracking ref updated
    /// by an external fetch is invisible — supposedly fixed by
    /// `GitRepository::reopen()` (added for #48, now called at the start of
    /// every refresh).
    ///
    /// Falsification (see git history on this file / the PR that introduced
    /// this test): reproducing the *exact* call the UI made —
    /// `merge_branch(repo, "origin/dev", BranchType::Local)`, because
    /// `merge_branch` hardcoded `BranchType::Local` — failed with "Branch
    /// 'origin/dev' not found" identically on a fresh handle, a handle made
    /// stale by an external `git fetch`, and a reopened handle. `reopen()`
    /// changed nothing; ref staleness was never the cause. The real bug: a
    /// remote-tracking ref name like `origin/dev` only resolves under
    /// `BranchType::Remote` (`refs/remotes/*`), never `BranchType::Local`
    /// (`refs/heads/*`), no matter how fresh the handle is.
    ///
    /// Fix: `merge_branch` now takes an explicit `branch_type`, threaded from
    /// the selected `BranchInfo::is_remote` at the UI call site
    /// (`ConfirmAction::Merge { name, is_remote }` in `app/mod.rs`, resolved
    /// in `app/confirm_actions.rs`). This test now asserts the *success* case
    /// end-to-end: merging `origin/dev` (via `BranchType::Remote`, the same
    /// resolution the fixed UI path performs) succeeds, uses git's own
    /// "remote-tracking branch" commit message convention, and the resulting
    /// commit is reachable from HEAD — mirroring what `git merge origin/dev`
    /// does in a shell, closing the gap #46 reported.
    #[test]
    fn merge_into_current_of_remote_branch_succeeds_and_is_reachable_from_head() {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        let local = tmp.path().join("local");
        Command::new("git").args(["init", "-q", "--bare"]).arg(&remote).status().unwrap();
        Command::new("git").args(["init", "-q"]).arg(&local).status().unwrap();
        git(&local, &["config", "user.email", "t@t.com"]);
        git(&local, &["config", "user.name", "t"]);
        git(&local, &["remote", "add", "origin", remote.to_str().unwrap()]);
        std::fs::write(local.join("a.txt"), "a").unwrap();
        git(&local, &["add", "a.txt"]);
        git(&local, &["commit", "-qm", "init"]);
        git(&local, &["push", "-q", "origin", "master"]);
        git(&local, &["branch", "dev"]);
        git(&local, &["push", "-q", "origin", "dev"]);

        // Long-lived handle, exactly like the one `App` keeps across refreshes.
        let mut repo = crate::git::GitRepository::open(&local).unwrap();

        // Advance `dev` on the remote from a second clone (a divergent commit,
        // so this exercises the "normal merge" path, not just fast-forward),
        // then update the local remote-tracking ref via an external
        // `git fetch` — bypassing our long-lived handle entirely, exactly as
        // the original bug report described.
        let other = tmp.path().join("other");
        Command::new("git").args(["clone", "-q"]).arg(&remote).arg(&other).status().unwrap();
        git(&other, &["config", "user.email", "t@t.com"]);
        git(&other, &["config", "user.name", "t"]);
        git(&other, &["checkout", "-q", "dev"]);
        std::fs::write(other.join("b.txt"), "b").unwrap();
        git(&other, &["add", "b.txt"]);
        git(&other, &["commit", "-qm", "advance dev"]);
        git(&other, &["push", "-q", "origin", "dev"]);

        // Diverge locally too, so the merge is a real (non-fast-forward) merge
        // commit, matching the general case `git merge origin/dev` handles.
        std::fs::write(local.join("c.txt"), "c").unwrap();
        git(&local, &["add", "c.txt"]);
        git(&local, &["commit", "-qm", "local work"]);

        git(&local, &["fetch", "-q", "origin", "dev"]);

        // Reopen first, matching what `refresh_inner` now does before every
        // refresh (the #48 fix) — the handle is stale relative to the fetch
        // above, and reopening is the documented way the app keeps refs
        // current.
        repo.reopen().unwrap();

        let pre_head = repo.repo().head().unwrap().target().unwrap();

        let outcome = super::merge_branch(repo.repo(), "origin/dev", BranchType::Remote)
            .expect("merging origin/dev must succeed, matching `git merge origin/dev`");
        assert_eq!(outcome, OpOutcome::Completed);

        let post_head = repo.repo().head().unwrap().target().unwrap();
        assert_ne!(pre_head, post_head, "HEAD must move to a new merge commit");

        let merge_commit = repo.repo().find_commit(post_head).unwrap();
        assert_eq!(
            merge_commit.message(),
            Some("Merge remote-tracking branch 'origin/dev'"),
            "commit message must follow git's own remote-tracking merge convention"
        );
        assert_eq!(merge_commit.parent_count(), 2, "must be a real merge commit");

        // The remote's advanced tip must be an ancestor of the new HEAD, i.e.
        // actually merged in and reachable — not just a no-op or a wrong ref.
        let remote_tip = repo
            .repo()
            .find_branch("origin/dev", BranchType::Remote)
            .unwrap()
            .get()
            .target()
            .unwrap();
        assert!(
            repo.repo().graph_descendant_of(post_head, remote_tip).unwrap(),
            "origin/dev's tip must be an ancestor of the merged HEAD"
        );

        // And the file introduced only on origin/dev landed in the working tree.
        assert!(local.join("b.txt").exists(), "content from origin/dev must be merged in");
    }

    /// Same fix, same shape of bug, for `rebase_branch`: rebasing the current
    /// branch onto a remote-tracking branch (`CommitMenuItem::Rebase` on a
    /// selected remote branch) must work end-to-end, not just merge.
    #[test]
    fn rebase_onto_remote_branch_succeeds_and_replays_head_commits() {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        let local = tmp.path().join("local");
        Command::new("git").args(["init", "-q", "--bare"]).arg(&remote).status().unwrap();
        Command::new("git").args(["init", "-q"]).arg(&local).status().unwrap();
        git(&local, &["config", "user.email", "t@t.com"]);
        git(&local, &["config", "user.name", "t"]);
        git(&local, &["remote", "add", "origin", remote.to_str().unwrap()]);
        std::fs::write(local.join("a.txt"), "a").unwrap();
        git(&local, &["add", "a.txt"]);
        git(&local, &["commit", "-qm", "init"]);
        git(&local, &["push", "-q", "origin", "master"]);
        git(&local, &["branch", "dev"]);
        git(&local, &["push", "-q", "origin", "dev"]);

        let mut repo = crate::git::GitRepository::open(&local).unwrap();

        // Advance dev upstream from a second clone.
        let other = tmp.path().join("other");
        Command::new("git").args(["clone", "-q"]).arg(&remote).arg(&other).status().unwrap();
        git(&other, &["config", "user.email", "t@t.com"]);
        git(&other, &["config", "user.name", "t"]);
        git(&other, &["checkout", "-q", "dev"]);
        std::fs::write(other.join("b.txt"), "b").unwrap();
        git(&other, &["add", "b.txt"]);
        git(&other, &["commit", "-qm", "advance dev"]);
        git(&other, &["push", "-q", "origin", "dev"]);

        // Local diverges with its own commit on master.
        std::fs::write(local.join("c.txt"), "c").unwrap();
        git(&local, &["add", "c.txt"]);
        git(&local, &["commit", "-qm", "local work"]);
        let local_commit_msg = super::get_last_commit_message(local.to_str().unwrap()).unwrap();

        git(&local, &["fetch", "-q", "origin", "dev"]);
        repo.reopen().unwrap();

        let remote_tip = repo
            .repo()
            .find_branch("origin/dev", BranchType::Remote)
            .unwrap()
            .get()
            .target()
            .unwrap();

        let outcome = super::rebase_branch(repo.repo(), "origin/dev", BranchType::Remote)
            .expect("rebasing onto origin/dev must succeed, matching `git rebase origin/dev`");
        assert_eq!(outcome, OpOutcome::Completed);

        let post_head = repo.repo().head().unwrap().target().unwrap();
        let replayed = repo.repo().find_commit(post_head).unwrap();
        assert_eq!(
            replayed.message().map(str::trim),
            Some(local_commit_msg.as_str()),
            "the local commit must be replayed on top, message intact"
        );
        assert_eq!(replayed.parent_count(), 1);
        assert_eq!(
            replayed.parent_id(0).unwrap(),
            remote_tip,
            "the replayed commit's parent must be origin/dev's tip"
        );
        assert!(local.join("b.txt").exists(), "content from origin/dev must be present after rebase");
        assert!(local.join("c.txt").exists(), "local work must survive the rebase");
    }

    /// Companion finding to the test above: isolates whether a *targeted*
    /// `find_branch` lookup (as opposed to the `branches()` enumeration the
    /// existing `reopen_observes_remote_ref_created_after_open` test in
    /// `repository.rs` covers) needs `reopen()` to observe a remote-tracking
    /// ref that an external `git fetch` *updated* on a handle opened before
    /// the fetch.
    ///
    /// Finding: it does not. The stale handle resolves `origin/dev` to the new
    /// OID immediately after the external fetch, with no `reopen()` needed.
    /// The staleness `reopen()` fixes is specific to enumerating *newly
    /// created* refs (per the #48 test); it does not extend to targeted
    /// lookups of refs that already existed and were merely updated. This is
    /// further evidence that `merge_branch`'s "not found" failure (above) is
    /// not a ref-caching symptom.
    #[test]
    fn stale_handle_targeted_lookup_sees_externally_fetched_ref_update_without_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        let local = tmp.path().join("local");
        Command::new("git").args(["init", "-q", "--bare"]).arg(&remote).status().unwrap();
        Command::new("git").args(["init", "-q"]).arg(&local).status().unwrap();
        git(&local, &["config", "user.email", "t@t.com"]);
        git(&local, &["config", "user.name", "t"]);
        git(&local, &["remote", "add", "origin", remote.to_str().unwrap()]);
        std::fs::write(local.join("a.txt"), "a").unwrap();
        git(&local, &["add", "a.txt"]);
        git(&local, &["commit", "-qm", "init"]);
        git(&local, &["push", "-q", "origin", "master"]);
        git(&local, &["branch", "dev"]);
        git(&local, &["push", "-q", "origin", "dev"]);

        let repo = crate::git::GitRepository::open(&local).unwrap();
        let stale_oid = repo
            .repo()
            .find_branch("origin/dev", BranchType::Remote)
            .unwrap()
            .get()
            .target()
            .unwrap();

        let other = tmp.path().join("other");
        Command::new("git").args(["clone", "-q"]).arg(&remote).arg(&other).status().unwrap();
        git(&other, &["config", "user.email", "t@t.com"]);
        git(&other, &["config", "user.name", "t"]);
        git(&other, &["checkout", "-q", "dev"]);
        std::fs::write(other.join("b.txt"), "b").unwrap();
        git(&other, &["add", "b.txt"]);
        git(&other, &["commit", "-qm", "advance dev"]);
        git(&other, &["push", "-q", "origin", "dev"]);
        git(&local, &["fetch", "-q", "origin", "dev"]);

        let observed = repo
            .repo()
            .find_branch("origin/dev", BranchType::Remote)
            .ok()
            .and_then(|b| b.get().target());

        assert_ne!(
            observed,
            Some(stale_oid),
            "targeted find_branch on the pre-fetch handle must not silently \
             return the stale OID"
        );
        assert!(
            observed.is_some() && observed != Some(stale_oid),
            "targeted find_branch on the pre-fetch handle should already see \
             the externally-fetched update: got {observed:?}, stale was {stale_oid}"
        );
    }

    /// Regression: checking out a remote-tracking branch whose remote is NOT
    /// named `origin` (here `upstream`) must create and track the right local
    /// branch. The old `strip_prefix("origin/")` returned `None` for
    /// `upstream/feature`, so checkout failed for every non-origin remote.
    #[test]
    fn checkout_remote_branch_from_non_origin_remote_creates_tracking_local() {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        let local = tmp.path().join("local");
        Command::new("git").args(["init", "-q", "--bare"]).arg(&remote).status().unwrap();
        Command::new("git").args(["init", "-q"]).arg(&local).status().unwrap();
        git(&local, &["config", "user.email", "t@t.com"]);
        git(&local, &["config", "user.name", "t"]);
        // The remote is deliberately named `upstream`, not `origin`.
        git(&local, &["remote", "add", "upstream", remote.to_str().unwrap()]);
        std::fs::write(local.join("a.txt"), "a").unwrap();
        git(&local, &["add", "a.txt"]);
        git(&local, &["commit", "-qm", "init"]);
        git(&local, &["push", "-q", "upstream", "master"]);

        // A second clone adds a `feature` branch and pushes it to the shared repo.
        let other = tmp.path().join("other");
        Command::new("git").args(["clone", "-q"]).arg(&remote).arg(&other).status().unwrap();
        git(&other, &["config", "user.email", "t@t.com"]);
        git(&other, &["config", "user.name", "t"]);
        git(&other, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(other.join("b.txt"), "b").unwrap();
        git(&other, &["add", "b.txt"]);
        git(&other, &["commit", "-qm", "feature work"]);
        git(&other, &["push", "-q", "origin", "feature"]);

        // Local fetches: it now has refs/remotes/upstream/feature but no local
        // `feature`, so checkout must create and track it.
        git(&local, &["fetch", "-q", "upstream", "feature"]);

        let repo = Repository::open(&local).unwrap();
        assert!(
            repo.find_branch("feature", BranchType::Local).is_err(),
            "no local `feature` before checkout"
        );

        super::checkout_remote_branch(&repo, "upstream/feature")
            .expect("checkout of upstream/feature must create and track a local branch");

        // A local `feature` now exists and tracks `upstream/feature`.
        let local_branch = repo
            .find_branch("feature", BranchType::Local)
            .expect("local `feature` created");
        let upstream = local_branch.upstream().expect("upstream set on local branch");
        assert_eq!(
            upstream.name().unwrap(),
            Some("upstream/feature"),
            "local branch must track the upstream remote's ref, not a guessed origin"
        );

        // HEAD is on the new local branch, with the remote-only content present.
        assert_eq!(repo.head().unwrap().shorthand(), Some("feature"));
        assert!(local.join("b.txt").exists(), "feature content checked out");
    }
}

