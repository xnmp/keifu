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

    /// Get commit history (newest first)
    pub fn get_commits(
        &self,
        max_count: usize,
        stashes: &[StashInfo],
    ) -> Result<Vec<CommitInfo>> {
        let mut revwalk = self.repo.revwalk()?;
        revwalk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;

        // Include all branches
        for branch_result in self.repo.branches(None)? {
            let (branch, _) = branch_result?;
            if let Some(oid) = branch.get().target() {
                revwalk.push(oid)?;
            }
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

            if is_staged || has_worktree_changes {
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
