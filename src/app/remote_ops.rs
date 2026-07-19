//! Multi-remote resolution and the pull / push / publish / prune / delete-remote
//! flows built on top of it.
//!
//! Domain rule: a single-remote repo never prompts. With several remotes we
//! prefer the current branch's upstream remote; only when that can't
//! disambiguate do we surface the [`AppMode::RemotePicker`].

use super::*;

/// How to resolve which remote a network op targets.
enum RemoteChoice {
    /// No remote configured.
    None,
    /// Use this remote without prompting.
    Use(String),
    /// Multiple remotes and no upstream to disambiguate — ask the user.
    Prompt(Vec<String>),
}

impl App {
    /// The current HEAD branch's info, if HEAD is on a (non-detached) branch.
    pub(crate) fn head_branch_info(&self) -> Option<&BranchInfo> {
        self.branches.iter().find(|b| b.is_head)
    }

    /// Resolve the remote for a fetch/pull/push, preferring the current
    /// branch's upstream remote when several exist.
    fn resolve_remote(&self) -> RemoteChoice {
        let mut remotes = self.repo.remotes();
        match remotes.len() {
            0 => RemoteChoice::None,
            1 => RemoteChoice::Use(remotes.remove(0)),
            _ => match self.repo.head_upstream_remote() {
                Some(r) if remotes.contains(&r) => RemoteChoice::Use(r),
                _ => RemoteChoice::Prompt(remotes),
            },
        }
    }

    /// Remote to auto-fetch silently (never prompts): the upstream remote, else
    /// the first configured remote, else none.
    pub(crate) fn auto_fetch_remote(&self) -> Option<String> {
        if let Some(r) = self.repo.head_upstream_remote() {
            return Some(r);
        }
        self.repo.remotes().into_iter().next()
    }

    // ── Fetch ───────────────────────────────────────────────────────────

    pub(crate) fn initiate_fetch(&mut self) {
        if self.network.is_busy() {
            return;
        }
        match self.resolve_remote() {
            RemoteChoice::None => self.set_message("No remote configured"),
            RemoteChoice::Use(r) => self.start_fetch_remote(r, true, false),
            RemoteChoice::Prompt(remotes) => self.open_remote_picker(remotes, RemoteOp::Fetch),
        }
    }

    // ── Pull ────────────────────────────────────────────────────────────

    pub(crate) fn initiate_pull(&mut self) {
        if self.block_if_op_in_progress("pull") {
            return;
        }
        if self.network.is_busy() {
            return;
        }
        if self.repo.remotes().is_empty() {
            self.set_message("No remote configured");
            return;
        }
        // A configured upstream lets a bare `git pull` resolve everything.
        let has_upstream = self
            .head_branch_info()
            .map(|b| b.upstream.is_some())
            .unwrap_or(false);
        if has_upstream {
            self.start_pull_remote(None, PullMode::FfOnly);
            return;
        }
        match self.resolve_remote() {
            RemoteChoice::None => self.set_message("No remote configured"),
            RemoteChoice::Use(r) => self.start_pull_remote(Some(r), PullMode::FfOnly),
            RemoteChoice::Prompt(remotes) => self.open_remote_picker(remotes, RemoteOp::Pull),
        }
    }

    // ── Push / publish ──────────────────────────────────────────────────

    pub(crate) fn initiate_push(&mut self) {
        if self.network.is_busy() {
            return;
        }
        let Some(head) = self.head_branch_info() else {
            self.set_message("Not on a branch");
            return;
        };
        let has_upstream = head.upstream.is_some();
        let branch = head.name.clone();
        let mut remotes = self.repo.remotes();
        match remotes.len() {
            0 => self.set_message("No remote configured"),
            1 => {
                if has_upstream {
                    self.start_push_current();
                } else {
                    self.start_publish(remotes.remove(0), branch);
                }
            }
            // With several remotes, always let the user pick which to push to —
            // even when an upstream is configured — defaulting the selection to
            // that upstream remote.
            _ => self.open_remote_picker(remotes, RemoteOp::Push),
        }
    }

    // ── Prune ───────────────────────────────────────────────────────────

    pub(crate) fn initiate_prune(&mut self) {
        match self.resolve_remote() {
            RemoteChoice::None => self.set_message("No remote configured"),
            RemoteChoice::Use(r) => self.prune_remote_now(&r),
            RemoteChoice::Prompt(remotes) => self.open_remote_picker(remotes, RemoteOp::Prune),
        }
    }

    fn prune_remote_now(&mut self, remote: &str) {
        match prune_remote(&self.repo_path, remote) {
            Ok(()) => {
                if let Err(e) = self.refresh(true) {
                    self.show_error(format!("Refresh failed: {e}"));
                    return;
                }
                self.set_message(format!("Pruned {remote}"));
            }
            Err(e) => self.show_error(format!("Prune failed: {e}")),
        }
    }

    // ── Remote picker ───────────────────────────────────────────────────

    fn open_remote_picker(&mut self, remotes: Vec<String>, op: RemoteOp) {
        let selected =
            remote_picker_default(&remotes, self.repo.head_upstream_remote().as_deref());
        self.mode = AppMode::RemotePicker {
            remotes,
            selected,
            op,
        };
    }

    pub(crate) fn handle_remote_picker_action(&mut self, action: Action) -> Result<()> {
        let AppMode::RemotePicker {
            remotes,
            selected,
            op,
        } = &self.mode
        else {
            return Ok(());
        };
        let remotes = remotes.clone();
        let selected = *selected;
        let op = *op;

        match action {
            Action::MoveUp => {
                let new = cyclic_prev(selected, remotes.len());
                self.mode = AppMode::RemotePicker { remotes, selected: new, op };
            }
            Action::MoveDown => {
                let new = cyclic_next(selected, remotes.len());
                self.mode = AppMode::RemotePicker { remotes, selected: new, op };
            }
            Action::MenuSelect | Action::Confirm => {
                if let Some(remote) = remotes.get(selected).cloned() {
                    self.mode = AppMode::Normal;
                    self.run_remote_op(op, remote);
                }
            }
            Action::Cancel | Action::Quit => {
                self.mode = AppMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    fn run_remote_op(&mut self, op: RemoteOp, remote: String) {
        match op {
            RemoteOp::Fetch => self.start_fetch_remote(remote, true, false),
            RemoteOp::Pull => self.start_pull_remote(Some(remote), PullMode::FfOnly),
            RemoteOp::Push => self.run_push_to_remote(remote),
            RemoteOp::Prune => self.prune_remote_now(&remote),
        }
    }

    /// Push HEAD to the chosen `remote`. If the branch has no upstream, publish
    /// (set upstream). If `remote` is the configured upstream, a plain
    /// `git push`. Otherwise push to that remote without retargeting upstream.
    fn run_push_to_remote(&mut self, remote: String) {
        let Some(head) = self.head_branch_info() else {
            self.set_message("Not on a branch");
            return;
        };
        let branch = head.name.clone();
        let has_upstream = head.upstream.is_some();
        let upstream_remote = self.repo.head_upstream_remote();
        if !has_upstream {
            self.start_publish(remote, branch);
        } else if upstream_remote.as_deref() == Some(remote.as_str()) {
            self.start_push_current();
        } else {
            self.start_push_head_to(remote);
        }
    }

    // ── Delete remote branch ────────────────────────────────────────────

    /// Split a remote-tracking ref ("origin/feature/x") into its remote and
    /// branch parts, matching against the configured remotes so branch names
    /// containing slashes are handled correctly.
    pub(crate) fn split_remote_ref(&self, refname: &str) -> Option<(String, String)> {
        self.repo.remotes().into_iter().find_map(|remote| {
            refname
                .strip_prefix(&format!("{remote}/"))
                .map(|branch| (remote.clone(), branch.to_string()))
        })
    }
}

/// Default-selected row in the remote picker: the branch's upstream remote when
/// it's among the choices, else the first remote. Pure so it's unit-testable
/// independent of a live repo.
pub(crate) fn remote_picker_default(remotes: &[String], upstream_remote: Option<&str>) -> usize {
    upstream_remote
        .and_then(|r| remotes.iter().position(|x| x == r))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::remote_picker_default;

    fn remotes() -> Vec<String> {
        vec!["origin".to_string(), "upstream".to_string(), "fork".to_string()]
    }

    #[test]
    fn defaults_to_upstream_remote_when_present() {
        assert_eq!(remote_picker_default(&remotes(), Some("upstream")), 1);
        assert_eq!(remote_picker_default(&remotes(), Some("fork")), 2);
        assert_eq!(remote_picker_default(&remotes(), Some("origin")), 0);
    }

    #[test]
    fn falls_back_to_first_when_upstream_absent_or_unknown() {
        assert_eq!(remote_picker_default(&remotes(), None), 0);
        // Upstream remote not in the list (renamed/removed) — pick the first.
        assert_eq!(remote_picker_default(&remotes(), Some("gone")), 0);
    }
}
