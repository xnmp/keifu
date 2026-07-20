//! Repository operation wrapper

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use git2::{Repository, Status};

use git2::Oid;

use super::{BranchInfo, CommitDiffInfo, CommitInfo};

#[derive(Debug, Clone)]
pub struct StashInfo {
    pub index: usize,
    pub message: String,
    pub oid: Oid,
    pub base_oid: Oid,
}

/// A tag pointing at a commit. Annotated tags are peeled to the commit they
/// ultimately reference, so `target_oid` is always a commit OID.
#[derive(Debug, Clone)]
pub struct TagInfo {
    pub name: String,
    pub target_oid: Oid,
}

/// The in-progress git operation the repository is currently mid-way through,
/// derived from `git2::RepositoryState`. Drives conflict-recovery UI
/// (abort/continue) and the status-bar indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationState {
    Clean,
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

impl OperationState {
    /// Map a libgit2 repository state to the coarse operation it represents.
    /// States outside the four resolvable operations (bisect, mailbox, …) are
    /// treated as `Clean` — the conflict-recovery UI doesn't cover them.
    pub fn from_repo_state(state: git2::RepositoryState) -> Self {
        use git2::RepositoryState as S;
        match state {
            S::Merge => Self::Merge,
            S::Revert | S::RevertSequence => Self::Revert,
            S::CherryPick | S::CherryPickSequence => Self::CherryPick,
            S::Rebase | S::RebaseInteractive | S::RebaseMerge => Self::Rebase,
            _ => Self::Clean,
        }
    }

    /// Whether an operation is in progress (i.e. not `Clean`).
    pub fn is_in_progress(self) -> bool {
        !matches!(self, Self::Clean)
    }

    /// Uppercase label for the status bar (e.g. "MERGING").
    pub fn label(self) -> &'static str {
        match self {
            Self::Clean => "",
            Self::Merge => "MERGING",
            Self::Rebase => "REBASING",
            Self::CherryPick => "CHERRY-PICKING",
            Self::Revert => "REVERTING",
        }
    }

    /// Lowercase verb for user-facing messages (e.g. "merge").
    pub fn verb(self) -> &'static str {
        self.git_subcommand().unwrap_or("operation")
    }

    /// The `git` subcommand used for `--abort` / `--continue`, or `None` when
    /// clean.
    pub fn git_subcommand(self) -> Option<&'static str> {
        match self {
            Self::Clean => None,
            Self::Merge => Some("merge"),
            Self::Rebase => Some("rebase"),
            Self::CherryPick => Some("cherry-pick"),
            Self::Revert => Some("revert"),
        }
    }
}

pub struct GitRepository {
    repo: Repository,
    pub path: String,
}

impl GitRepository {
    /// Access the underlying git2 Repository.
    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    /// Discover a repository from the current directory
    pub fn discover() -> Result<Self> {
        let repo = Repository::discover(".")
            .context("Git repository not found. Please run inside a Git repository.")?;
        let path = repo
            .workdir()
            .unwrap_or_else(|| repo.path())
            .to_string_lossy()
            .to_string();
        Ok(Self { repo, path })
    }

    /// Open a repository from a specified path
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let repo = Repository::open(path.as_ref())
            .context("Git repository not found at specified path.")?;
        let path_str = repo
            .workdir()
            .unwrap_or_else(|| repo.path())
            .to_string_lossy()
            .to_string();
        Ok(Self {
            repo,
            path: path_str,
        })
    }

    /// Re-open the underlying libgit2 repository handle in place.
    ///
    /// A `git2::Repository` is a point-in-time view that caches refs, config and
    /// object-db state. When another process mutates the repo — `git push` or
    /// `git fetch` creating/updating `refs/remotes/<remote>/*`, or setting a
    /// branch's upstream in config — a long-lived handle can keep reporting the
    /// pre-change state, so a refresh may still classify a just-pushed branch as
    /// unpushed (its `origin/<branch>` counterpart not yet visible). This is the
    /// refs/config analogue of the `clear_ignore_rules()` flush already done
    /// before status queries. Re-opening drops those caches so subsequent
    /// queries observe current on-disk state; cheap enough to run every refresh.
    pub fn reopen(&mut self) -> Result<()> {
        let repo = Repository::open(&self.path)
            .with_context(|| format!("Failed to re-open git repository at {}", self.path))?;
        self.repo = repo;
        Ok(())
    }

    /// Get commit history (newest first).
    ///
    /// Walks from the tips of the supplied `branches` — the caller decides
    /// which branches are visible (e.g. by excluding hidden ones), so hiding a
    /// branch removes its exclusive commits from the graph. HEAD and stash
    /// commits are always pushed as well.
    pub fn get_commits(
        &self,
        max_count: usize,
        branches: &[BranchInfo],
        stashes: &[StashInfo],
    ) -> Result<Vec<CommitInfo>> {
        let mut revwalk = self.repo.revwalk()?;
        revwalk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;

        // Walk from the tips of the requested (visible) branches.
        for branch in branches {
            let _ = revwalk.push(branch.tip_oid);
        }

        // Include HEAD itself: a detached HEAD on a commit not reachable from
        // any branch tip — or a HEAD whose own branch is hidden — would
        // otherwise vanish from the graph (taking the uncommitted-changes node
        // with it).
        if let Some(oid) = self.repo.head().ok().and_then(|h| h.target()) {
            let _ = revwalk.push(oid);
        }

        // Collect stash OIDs and their internal commits (index, untracked)
        // that should be excluded from the graph.
        let stash_oids: std::collections::HashSet<Oid> =
            stashes.iter().map(|s| s.oid).collect();
        let mut stash_internal_oids: std::collections::HashSet<Oid> =
            std::collections::HashSet::new();
        for stash in stashes {
            if let Ok(commit) = self.repo.find_commit(stash.oid) {
                // Parents beyond the first (index tree, untracked tree) are internal
                for i in 1..commit.parent_count() {
                    if let Ok(parent_id) = commit.parent_id(i) {
                        stash_internal_oids.insert(parent_id);
                    }
                }
            }
            let _ = revwalk.push(stash.oid);
        }

        let mut commits = Vec::new();
        for oid_result in revwalk.take(max_count) {
            let oid = oid_result?;
            // Skip stash internal commits (index tree, untracked tree)
            if stash_internal_oids.contains(&oid) {
                continue;
            }
            let commit = self.repo.find_commit(oid)?;
            let mut info = CommitInfo::from_git2_commit(&commit);
            // Stash commits have 2-3 parents (base + index + untracked).
            // Treat them as single-parent to avoid merge rendering.
            if stash_oids.contains(&oid) {
                info.parent_oids.truncate(1);
            }
            commits.push(info);
        }

        Ok(commits)
    }

    pub fn get_stashes(&mut self) -> Vec<StashInfo> {
        let mut raw_stashes = Vec::new();
        let _ = self.repo.stash_foreach(|index, message, oid| {
            raw_stashes.push((index, message.to_string(), *oid));
            true
        });

        raw_stashes
            .into_iter()
            .filter_map(|(index, message, oid)| {
                let commit = self.repo.find_commit(oid).ok()?;
                let base_oid = commit.parent_id(0).ok()?;
                Some(StashInfo {
                    index,
                    message,
                    oid,
                    base_oid,
                })
            })
            .collect()
    }

    /// Get branch list
    pub fn get_branches(&self) -> Result<Vec<BranchInfo>> {
        BranchInfo::list_all(&self.repo)
    }

    /// Load all tags as (name -> target commit OID). Both lightweight and
    /// annotated tags are resolved to the commit they point at (annotated tags
    /// are peeled through their tag object). Malformed tags are skipped rather
    /// than failing the whole load.
    pub fn get_tags(&self) -> Vec<TagInfo> {
        let mut tags = Vec::new();
        let Ok(names) = self.repo.tag_names(None) else {
            return tags;
        };
        for name in names.iter().flatten() {
            if let Ok(reference) = self.repo.find_reference(&format!("refs/tags/{name}")) {
                // peel_to_commit follows annotated tag objects to the commit.
                if let Ok(commit) = reference.peel_to_commit() {
                    tags.push(TagInfo {
                        name: name.to_string(),
                        target_oid: commit.id(),
                    });
                }
            }
        }
        tags
    }

    /// Names of the configured remotes (e.g. `["origin", "upstream"]`).
    pub fn remotes(&self) -> Vec<String> {
        self.repo
            .remotes()
            .map(|arr| arr.iter().flatten().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    }

    /// The fetch URL of a configured remote by name (e.g. for extracting its
    /// host to look up cached credentials). `None` if the remote or URL is
    /// missing.
    pub fn remote_url(&self, remote: &str) -> Option<String> {
        self.repo
            .find_remote(remote)
            .ok()
            .and_then(|r| r.url().map(|s| s.to_string()))
    }

    /// The remote configured for the current branch's upstream, if any
    /// (`branch.<name>.remote`). `None` for a detached HEAD or an
    /// upstream-less branch.
    pub fn head_upstream_remote(&self) -> Option<String> {
        let head = self.repo.head().ok()?;
        let refname = head.name()?;
        let buf = self.repo.branch_upstream_remote(refname).ok()?;
        buf.as_str().map(|s| s.to_string())
    }

    /// Get the current HEAD name
    pub fn head_name(&self) -> Option<String> {
        self.repo
            .head()
            .ok()
            .and_then(|h| h.shorthand().map(|s| s.to_string()))
    }

    /// Check if HEAD is detached
    pub fn is_head_detached(&self) -> bool {
        self.repo.head_detached().unwrap_or(false)
    }

    /// Get the current HEAD commit OID
    pub fn head_oid(&self) -> Option<Oid> {
        self.repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .map(|c| c.id())
    }

    /// The in-progress git operation (merge/rebase/cherry-pick/revert), if any.
    pub fn operation_state(&self) -> OperationState {
        OperationState::from_repo_state(self.repo.state())
    }

    /// Number of paths currently in a conflicted (unmerged) state.
    pub fn conflicted_count(&self) -> usize {
        self.repo
            .statuses(None)
            .map(|statuses| {
                statuses
                    .iter()
                    .filter(|e| e.status().contains(Status::CONFLICTED))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Get working tree status (staged + unstaged + untracked changes)
    /// Returns None if there are no changes
    pub fn get_working_tree_status(&self) -> Result<Option<WorkingTreeStatus>> {
        if self.repo.is_bare() {
            return Ok(None);
        }

        // Flush cached ignore rules so .gitignore edits take effect
        // without restarting the application.
        let _ = self.repo.clear_ignore_rules();

        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .include_ignored(false);

        let statuses = self.repo.statuses(Some(&mut opts))?;
        let workdir = self.repo.workdir().unwrap_or_else(|| self.repo.path());
        let mut file_paths: Vec<PathBuf> = Vec::new();
        let mut has_collapsed_untracked_dirs = false;

        for entry in statuses.iter() {
            let status = entry.status();

            // Staged changes (INDEX_*)
            let is_staged = status.intersects(
                git2::Status::INDEX_NEW
                    | git2::Status::INDEX_MODIFIED
                    | git2::Status::INDEX_DELETED
                    | git2::Status::INDEX_RENAMED
                    | git2::Status::INDEX_TYPECHANGE,
            );

            // Worktree changes: unstaged + untracked (WT_*)
            let has_worktree_changes = status.intersects(
                git2::Status::WT_NEW
                    | git2::Status::WT_MODIFIED
                    | git2::Status::WT_DELETED
                    | git2::Status::WT_RENAMED
                    | git2::Status::WT_TYPECHANGE,
            );

            // Conflicted (unmerged) paths report only CONFLICTED — without this
            // a merge whose sole change is the conflicted file would leave the
            // uncommitted node (and its Merge Changes section) invisible.
            let is_conflicted = status.contains(Status::CONFLICTED);

            if is_staged || has_worktree_changes || is_conflicted {
                let path = super::path_from_bytes(entry.path_bytes());
                if status.intersects(Status::WT_NEW) {
                    let full_path = workdir.join(&path);
                    if CommitDiffInfo::is_plain_directory(&full_path) {
                        has_collapsed_untracked_dirs = true;
                    }
                }
                file_paths.push(path);
            }
        }

        if file_paths.is_empty() {
            Ok(None)
        } else {
            file_paths.sort();

            // Compute mtime hash from all changed files
            let mtime_hash: u128 = file_paths
                .iter()
                .filter_map(|path| {
                    let full_path = workdir.join(path);
                    std::fs::symlink_metadata(&full_path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis())
                })
                .sum();

            Ok(Some(WorkingTreeStatus {
                file_paths,
                mtime_hash,
                has_collapsed_untracked_dirs,
            }))
        }
    }
}

/// Working tree status
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingTreeStatus {
    /// Sorted list of file paths with changes (used as cache key)
    pub file_paths: Vec<PathBuf>,
    /// Sum of file mtimes in milliseconds (used as cache key for content changes)
    pub mtime_hash: u128,
    /// True when untracked directories were collapsed to a single status entry.
    /// In that case the mtime hash is not precise enough to safely reuse the
    /// uncommitted diff cache across refreshes.
    pub has_collapsed_untracked_dirs: bool,
}

impl WorkingTreeStatus {
    pub fn file_count(&self) -> usize {
        self.file_paths.len()
    }

    /// Returns the exact file count when accurate, or None when untracked
    /// directories were collapsed and the true count is unknown.
    pub fn accurate_file_count(&self) -> Option<usize> {
        if self.has_collapsed_untracked_dirs {
            None
        } else {
            Some(self.file_paths.len())
        }
    }

    pub fn is_precise_cache_key(&self) -> bool {
        !self.has_collapsed_untracked_dirs
    }
}

#[cfg(test)]
mod tests {
    use super::GitRepository;
    use std::path::Path;
    use std::process::Command;

    /// Run a git command in `dir`, asserting success.
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git invocation failed");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn reopen_observes_remote_ref_created_after_open() {
        // The staleness fix: a remote-tracking ref created (by a push) after the
        // handle was opened must be visible once the handle is reopened, so a
        // refresh classifies the branch as pushed rather than unpushed.
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
        git(&local, &["checkout", "-qb", "feature"]);

        // Long-lived handle opened BEFORE the push.
        let mut repo = GitRepository::open(&local).unwrap();
        assert!(
            !repo.get_branches().unwrap().iter().any(|b| b.name == "origin/feature"),
            "origin/feature must not exist before the push"
        );

        git(&local, &["push", "-q", "origin", "feature"]);

        repo.reopen().unwrap();
        assert!(
            repo.get_branches().unwrap().iter().any(|b| b.name == "origin/feature"),
            "reopened handle must see origin/feature created by the push"
        );
    }

    #[test]
    fn hide_remote_branches_caps_walk_at_local_tip_when_behind() {
        // Issue #57: a local branch that is behind its remote-tracking upstream
        // must not leak the remote-only commits into the graph once
        // hide_remote_branches is on. This exercises the exact filtering the
        // app performs (git::remote_only_branch_names -> drop from the
        // walk-tip list -> get_commits) against a real repo.
        use crate::git::{remote_only_branch_names, BranchInfo};

        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        let local = tmp.path().join("local");
        Command::new("git").args(["init", "-q", "--bare"]).arg(&remote).status().unwrap();
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .arg(&local)
            .status()
            .unwrap();
        git(&local, &["config", "user.email", "t@t.com"]);
        git(&local, &["config", "user.name", "t"]);
        git(&local, &["remote", "add", "origin", remote.to_str().unwrap()]);
        std::fs::write(local.join("a.txt"), "a").unwrap();
        git(&local, &["add", "a.txt"]);
        git(&local, &["commit", "-qm", "c1"]);
        git(&local, &["push", "-q", "-u", "origin", "main"]);

        // A second commit lands on the remote (e.g. pushed from elsewhere)
        // that the local branch hasn't pulled yet.
        std::fs::write(local.join("a.txt"), "b").unwrap();
        git(&local, &["add", "a.txt"]);
        git(&local, &["commit", "-qm", "c2-remote-only"]);
        git(&local, &["push", "-q", "origin", "main"]);
        // Roll local main back so it's one commit behind origin/main, without
        // touching the remote-tracking ref that was just pushed.
        git(&local, &["reset", "-q", "--hard", "HEAD~1"]);

        let repo = GitRepository::open(&local).unwrap();
        let branches = repo.get_branches().unwrap();
        let local_main = branches
            .iter()
            .find(|b| b.name == "main" && !b.is_remote)
            .expect("local main branch");
        assert_eq!(local_main.behind, 1, "local main must be exactly one commit behind origin/main");

        // Simulate the app's hide-remote-branches filtering (app/refresh.rs,
        // app/init.rs): remote-only branches are dropped before the walk.
        let remote_only = remote_only_branch_names(&branches);
        let visible: Vec<BranchInfo> =
            branches.into_iter().filter(|b| !remote_only.contains(&b.name)).collect();

        let commits = repo.get_commits(50, &visible, &[]).unwrap();
        assert!(
            !commits.iter().any(|c| c.message.contains("c2-remote-only")),
            "remote-only commit leaked into the graph with hide_remote_branches on"
        );
    }
}
