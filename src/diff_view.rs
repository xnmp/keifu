//! Diff rendering backend (plain text / ANSI).

use std::path::Path;

use anyhow::Result;
use git2::Oid;
use tempfile::NamedTempFile;

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
            let repo = git2::Repository::open(repo_path)?;
            let commit = repo.find_commit(commit_oid)?;
            let parent_oid = commit.parent_id(0).ok();

            let new_content = {
                let obj = repo.revparse_single(&format!("{}:{}", commit_oid, path.display()))?;
                let blob = obj.peel_to_blob()?;
                String::from_utf8_lossy(blob.content()).to_string()
            };

            let old_content = if let Some(parent_oid) = parent_oid {
                repo.revparse_single(&format!("{}:{}", parent_oid, path.display()))
                    .ok()
                    .and_then(|obj| obj.peel_to_blob().ok())
                    .map(|blob| String::from_utf8_lossy(blob.content()).to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            };

            let mut old_file = NamedTempFile::new()?;
            let mut new_file = NamedTempFile::new()?;
            use std::io::Write;
            old_file.write_all(old_content.as_bytes())?;
            new_file.write_all(new_content.as_bytes())?;

            let output = std::process::Command::new("difft")
                .args(["--color=always", "--display=inline"])
                .arg(old_file.path())
                .arg(new_file.path())
                .output()?;

            Ok(DiffRender {
                title: path.to_string_lossy().to_string(),
                ansi: String::from_utf8_lossy(&output.stdout).to_string(),
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
            let workdir = git2::Repository::open(repo_path)?
                .workdir()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from(repo_path));

            let full_path = workdir.join(path);
            let new_content = std::fs::read_to_string(&full_path).unwrap_or_default();

            // Old content is from HEAD if it exists; otherwise empty.
            let repo = git2::Repository::open(repo_path)?;
            let old_content = repo
                .revparse_single(&format!("HEAD:{}", path.display()))
                .ok()
                .and_then(|obj| obj.peel_to_blob().ok())
                .map(|b| String::from_utf8_lossy(b.content()).to_string())
                .unwrap_or_default();

            let mut old_file = NamedTempFile::new()?;
            let mut new_file = NamedTempFile::new()?;
            use std::io::Write;
            old_file.write_all(old_content.as_bytes())?;
            new_file.write_all(new_content.as_bytes())?;

            let output = std::process::Command::new("difft")
                .args(["--color=always", "--display=inline"])
                .arg(old_file.path())
                .arg(new_file.path())
                .output()?;

            Ok(DiffRender {
                title: path.to_string_lossy().to_string(),
                ansi: String::from_utf8_lossy(&output.stdout).to_string(),
            })
        }
    }
}
