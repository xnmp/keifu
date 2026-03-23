//! Diff rendering backend (plain text / ANSI).

use std::path::Path;

use anyhow::Result;
use git2::Oid;

/// Which backend to use to produce diff output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffBackend {
    /// Built-in implementation (libgit2 + git CLI fallbacks).
    Git,
    /// External renderer (e.g. difftastic). Not wired yet.
    Difftastic,
}

/// Rendered diff payload.
#[derive(Debug, Clone)]
pub struct DiffRender {
    pub title: String,
    /// Diff content (ideally ANSI-colored). May be plain text.
    pub ansi: String,
}

pub fn render_commit_file_diff(
    repo_path: &str,
    commit_oid: Oid,
    path: &Path,
    backend: DiffBackend,
) -> Result<DiffRender> {
    match backend {
        DiffBackend::Git => {
            let repo = git2::Repository::open(repo_path)?;
            let ansi = crate::git::CommitDiffInfo::unified_diff_for_file(&repo, commit_oid, path)?;
            Ok(DiffRender {
                title: path.to_string_lossy().to_string(),
                ansi,
            })
        }
        DiffBackend::Difftastic => {
            // TODO: run difftastic and capture ANSI output.
            Ok(DiffRender {
                title: path.to_string_lossy().to_string(),
                ansi: "(difftastic backend not implemented yet)".to_string(),
            })
        }
    }
}

pub fn render_worktree_file_diff(
    repo_path: &str,
    path: &Path,
    backend: DiffBackend,
) -> Result<DiffRender> {
    match backend {
        DiffBackend::Git => {
            let repo = git2::Repository::open(repo_path)?;
            let ansi = crate::git::CommitDiffInfo::unified_diff_for_working_tree_file(&repo, path)?;
            Ok(DiffRender {
                title: path.to_string_lossy().to_string(),
                ansi,
            })
        }
        DiffBackend::Difftastic => {
            // TODO: run difftastic and capture ANSI output.
            Ok(DiffRender {
                title: path.to_string_lossy().to_string(),
                ansi: "(difftastic backend not implemented yet)".to_string(),
            })
        }
    }
}

