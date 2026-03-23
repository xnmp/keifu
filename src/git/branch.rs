//! Branch info structure and operations

use anyhow::Result;
use git2::{BranchType, Oid, Repository};

#[derive(Debug, Clone)]
pub struct BranchInfo {
    pub name: String,
    pub is_head: bool,
    pub is_remote: bool,
    pub upstream: Option<String>,
    pub tip_oid: Oid,
}

impl BranchInfo {
    pub fn list_all(repo: &Repository) -> Result<Vec<Self>> {
        let mut branches = Vec::new();

        // Get HEAD
        let head_oid = repo.head().ok().and_then(|r| r.target());
        let head_shorthand = repo.head().ok().and_then(|h| h.shorthand().map(|s| s.to_string()));

        // Local branches
        for branch_result in repo.branches(Some(BranchType::Local))? {
            let (branch, _) = branch_result?;
            if let Some(name) = branch.name()? {
                let reference = branch.get();
                if let Some(oid) = reference.target() {
                    let is_head = head_oid.map(|h| h == oid).unwrap_or(false)
                        && head_shorthand.as_deref() == Some(name);

                    let upstream = branch
                        .upstream()
                        .ok()
                        .and_then(|u| u.name().ok().flatten().map(|s| s.to_string()));

                    branches.push(BranchInfo {
                        name: name.to_string(),
                        is_head,
                        is_remote: false,
                        upstream,
                        tip_oid: oid,
                    });
                }
            }
        }

        // Remote branches
        for branch_result in repo.branches(Some(BranchType::Remote))? {
            let (branch, _) = branch_result?;
            if let Some(name) = branch.name()? {
                let reference = branch.get();
                if let Some(oid) = reference.target() {
                    branches.push(BranchInfo {
                        name: name.to_string(),
                        is_head: false,
                        is_remote: true,
                        upstream: None,
                        tip_oid: oid,
                    });
                }
            }
        }

        // Put the HEAD branch first
        branches.sort_by(|a, b| b.is_head.cmp(&a.is_head).then(a.name.cmp(&b.name)));

        // If HEAD is detached, `repo.head().shorthand()` is typically "HEAD" and no branch will
        // satisfy the local-branch name check above. Add a synthetic HEAD marker so the UI can
        // still clearly indicate the checked-out commit.
        if head_shorthand.as_deref() == Some("HEAD") {
            if let Some(oid) = head_oid {
                branches.insert(
                    0,
                    BranchInfo {
                        name: "HEAD".to_string(),
                        is_head: true,
                        is_remote: false,
                        upstream: None,
                        tip_oid: oid,
                    },
                );
            }
        }

        Ok(branches)
    }
}
