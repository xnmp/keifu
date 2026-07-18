//! Branch info structure and operations

use std::collections::HashSet;

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
    /// The branch name with any remote prefix stripped: `origin/main` -> `main`,
    /// `feature/x` (local) -> `feature/x`. Only the first path segment of a
    /// remote ref is the remote name, so the rest is the branch's own name.
    fn short_name(&self) -> &str {
        if self.is_remote {
            self.name.split_once('/').map_or(&self.name, |(_, rest)| rest)
        } else {
            &self.name
        }
    }

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

/// Names of the remote branches in `branches` that have no matching local
/// branch — the "remote-only" refs that the show/hide-remotes toggle targets.
///
/// A remote branch is considered *tracked* (and therefore kept visible, even
/// when remotes are hidden) when some local branch:
///   - declares it as its upstream, or
///   - shares its short name (`origin/main` ↔ local `main`), or
///   - points at the same commit.
///
/// In any of those cases a local branch already represents the same work, so
/// hiding remotes must not drop it from the graph. Everything else — refs that
/// live only on the remote — is returned here.
pub fn remote_only_branch_names(branches: &[BranchInfo]) -> HashSet<String> {
    let locals: Vec<&BranchInfo> = branches.iter().filter(|b| !b.is_remote).collect();

    branches
        .iter()
        .filter(|b| b.is_remote)
        .filter(|remote| {
            !locals.iter().any(|local| {
                local.upstream.as_deref() == Some(remote.name.as_str())
                    || local.short_name() == remote.short_name()
                    || local.tip_oid == remote.tip_oid
            })
        })
        .map(|b| b.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(byte: u8) -> Oid {
        Oid::from_bytes(&[byte; 20]).unwrap()
    }

    fn local(name: &str, tip: Oid, upstream: Option<&str>) -> BranchInfo {
        BranchInfo {
            name: name.to_string(),
            is_head: false,
            is_remote: false,
            upstream: upstream.map(str::to_string),
            tip_oid: tip,
            ahead: 0,
            behind: 0,
        }
    }

    fn remote(name: &str, tip: Oid) -> BranchInfo {
        BranchInfo {
            name: name.to_string(),
            is_head: false,
            is_remote: true,
            upstream: None,
            tip_oid: tip,
            ahead: 0,
            behind: 0,
        }
    }

    #[test]
    fn remote_with_no_local_counterpart_is_remote_only() {
        let branches = vec![
            local("main", oid(1), None),
            remote("origin/agent-work", oid(2)),
        ];
        let names = remote_only_branch_names(&branches);
        assert_eq!(names.len(), 1);
        assert!(names.contains("origin/agent-work"));
    }

    #[test]
    fn remote_tracked_by_upstream_config_is_not_remote_only() {
        // Local `main` tracks `origin/main` but sits on a different commit
        // (ahead/behind). Same-name would also match, so also assert on a
        // differently-named upstream to isolate the upstream check.
        let branches = vec![
            local("feature", oid(1), Some("origin/renamed-on-remote")),
            remote("origin/renamed-on-remote", oid(2)),
        ];
        assert!(remote_only_branch_names(&branches).is_empty());
    }

    #[test]
    fn remote_sharing_short_name_with_local_is_not_remote_only() {
        // No upstream configured and tips differ, but the names line up.
        let branches = vec![
            local("main", oid(1), None),
            remote("origin/main", oid(2)),
        ];
        assert!(remote_only_branch_names(&branches).is_empty());
    }

    #[test]
    fn remote_sharing_tip_with_local_is_not_remote_only() {
        // Differently named, no upstream, but pointing at the same commit —
        // e.g. a just-pushed branch or `origin/HEAD` aliasing the default.
        let branches = vec![
            local("main", oid(7), None),
            remote("origin/HEAD", oid(7)),
        ];
        assert!(remote_only_branch_names(&branches).is_empty());
    }

    #[test]
    fn classifies_a_mixed_set() {
        let branches = vec![
            local("main", oid(1), Some("origin/main")),
            remote("origin/main", oid(1)),        // tracked (upstream + name + tip)
            remote("origin/dependabot", oid(2)),  // remote-only
            remote("origin/colleague", oid(3)),   // remote-only
            local("wip", oid(4), None),           // local-only, untouched
        ];
        let names = remote_only_branch_names(&branches);
        assert_eq!(names.len(), 2);
        assert!(names.contains("origin/dependabot"));
        assert!(names.contains("origin/colleague"));
    }
}
