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
    /// Commits this branch is ahead of its upstream (0 when no upstream).
    pub ahead: usize,
    /// Commits this branch is behind its upstream (0 when no upstream).
    pub behind: usize,
}

impl BranchInfo {
    pub fn list_all(repo: &Repository) -> Result<Vec<Self>> {
        let mut branches = Vec::new();

        // Get HEAD
        let head_oid = repo.head().ok().and_then(|r| r.target());

        // Local branches
        for branch_result in repo.branches(Some(BranchType::Local))? {
            let (branch, _) = branch_result?;
            if let Some(name) = branch.name()? {
                let reference = branch.get();
                if let Some(oid) = reference.target() {
                    let is_head = head_oid.map(|h| h == oid).unwrap_or(false)
                        && repo
                            .head()
                            .ok()
                            .and_then(|h| h.shorthand().map(|s| s == name))
                            .unwrap_or(false);

                    let upstream_branch = branch.upstream().ok();
                    let upstream = upstream_branch
                        .as_ref()
                        .and_then(|u| u.name().ok().flatten().map(|s| s.to_string()));

                    // Ahead/behind vs the upstream tip, when tracking one.
                    let (ahead, behind) = upstream_branch
                        .as_ref()
                        .and_then(|u| u.get().target())
                        .and_then(|up_oid| repo.graph_ahead_behind(oid, up_oid).ok())
                        .unwrap_or((0, 0));

                    branches.push(BranchInfo {
                        name: name.to_string(),
                        is_head,
                        is_remote: false,
                        upstream,
                        tip_oid: oid,
                        ahead,
                        behind,
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
                        ahead: 0,
                        behind: 0,
                    });
                }
            }
        }

        // Put the HEAD branch first
        branches.sort_by(|a, b| b.is_head.cmp(&a.is_head).then(a.name.cmp(&b.name)));

        Ok(branches)
    }
}
