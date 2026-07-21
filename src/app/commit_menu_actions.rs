//! Commit context menu: open, filter, execute.

use super::*;

impl App {
    pub(crate) fn open_delete_branch_picker(&mut self) {
        // Deletable = every branch on this node except the current HEAD (which
        // can't be deleted). This includes remote-tracking branches
        // ("origin/foo"), which are deleted on the remote when chosen.
        let deletable: Vec<String> = self
            .selected_node_branches()
            .iter()
            .filter(|name| {
                self.branches
                    .iter()
                    .any(|b| b.name == **name && !b.is_head)
            })
            .map(|s| s.to_string())
            .collect();

        match deletable.len() {
            0 => {}
            1 => self.confirm_delete_branch(deletable[0].clone()),
            _ => {
                self.mode = AppMode::BranchDeletePicker {
                    branches: deletable,
                    selected: 0,
                };
            }
        }
    }

    /// Open the destructive Confirm for deleting `name` — routing to a remote
    /// delete (`git push <remote> --delete`) when `name` is a remote-tracking
    /// ref, a local+remote delete offer when a local branch also exists on a
    /// remote, or a plain local branch delete otherwise.
    pub(crate) fn confirm_delete_branch(&mut self, name: String) {
        let is_remote = self
            .branches
            .iter()
            .any(|b| b.name == name && b.is_remote);
        if is_remote {
            if let Some((remote, branch)) = self.split_remote_ref(&name) {
                self.mode = AppMode::Confirm {
                    message: format!("Delete remote branch '{name}'? This cannot be undone."),
                    action: ConfirmAction::DeleteRemoteBranch { remote, branch },
                };
                return;
            }
        }
        // Local branch that also lives on a remote: offer to delete both.
        if let Some((remote, branch)) = self.remote_counterpart(&name) {
            self.mode = AppMode::Confirm {
                message: format!(
                    "Delete branch '{name}'?\nEnter: delete local · Ctrl+Enter / R: also delete {remote}/{branch}"
                ),
                action: ConfirmAction::DeleteBranchWithRemote { name, remote, branch },
            };
            return;
        }
        self.mode = AppMode::Confirm {
            message: format!("Delete branch '{}'?", name),
            action: ConfirmAction::DeleteBranch(name),
        };
    }

    /// The `(remote, branch)` a local branch also exists as, if any — used to
    /// offer a combined local+remote delete. Prefers the branch's configured
    /// upstream (its authoritative push target) when that upstream is present as
    /// a remote-tracking ref; otherwise falls back to any remote-tracking ref
    /// whose short name matches (`origin/<name>`). Returns `None` for a
    /// local-only branch or a name that isn't a local branch.
    pub(crate) fn remote_counterpart(&self, local_name: &str) -> Option<(String, String)> {
        let local = self
            .branches
            .iter()
            .find(|b| !b.is_remote && b.name == local_name)?;
        // Authoritative: the configured upstream, when it exists as a ref.
        if let Some(upstream) = &local.upstream {
            let tracked = self
                .branches
                .iter()
                .any(|b| b.is_remote && &b.name == upstream);
            if tracked {
                if let Some(split) = self.split_remote_ref(upstream) {
                    return Some(split);
                }
            }
        }
        // Fallback: a remote-tracking ref whose short name matches.
        self.branches
            .iter()
            .filter(|b| b.is_remote)
            .find_map(|b| {
                self.split_remote_ref(&b.name)
                    .filter(|(_, branch)| branch == local_name)
            })
    }

    pub(crate) fn open_commit_menu(&mut self) {
        // Keyboard opens the menu centered; a right-click sets `menu_anchor`
        // afterward to place it at the cursor.
        self.menu_anchor = None;
        let Some(node) = self.selected_commit_node() else {
            return;
        };

        if node.is_uncommitted {
            // For uncommitted node, go to files panel
            self.focused_panel = FocusedPanel::Files;
            return;
        }

        if node.is_stash {
            let items = vec![
                CommitMenuItem::StashApply,
                CommitMenuItem::StashPop,
                CommitMenuItem::BranchFromStash,
                CommitMenuItem::StashDrop,
            ];
            self.mode = AppMode::CommitMenu {
                items,
                selected: 0,
                filter: String::new(),
            };
            return;
        }

        let selected_oid = node.commit.as_ref().map(|c| c.oid);
        let has_branch = self.selected_branch().is_some();
        let is_head_branch = self.selected_branch().map(|b| b.is_head).unwrap_or(false);
        let mut items = Vec::new();

        // Push/pull pairing at top: push on any branch tip, pull on the HEAD
        // branch (which is what a bare `git pull` integrates into).
        if has_branch {
            items.push(CommitMenuItem::Push);
            if is_head_branch {
                items.push(CommitMenuItem::Pull);
            }
        }

        // PR actions (need the `gh` CLI + an open-PR context).
        if self.can_offer_create_pr() {
            items.push(CommitMenuItem::CreatePr);
        }
        if self.selected_commit_has_open_pr() {
            items.push(CommitMenuItem::MergePr);
        }

        items.push(CommitMenuItem::Checkout);
        items.push(CommitMenuItem::CreateBranch);

        // Deletable = any branch on this node bar the current HEAD; remote
        // branches are always deletable (on their remote).
        let has_deletable_branch = self.selected_node_branches().iter().any(|name| {
            self.branches
                .iter()
                .any(|b| b.name == *name && !b.is_head)
        });
        if has_deletable_branch {
            items.push(CommitMenuItem::DeleteBranch);
        }

        // Rename applies to any local branch label (including the current one).
        if !self.selected_node_local_branches().is_empty() {
            items.push(CommitMenuItem::RenameBranch);
        }

        if has_branch {
            if let Some(branch) = self.selected_branch() {
                if !branch.is_head {
                    items.push(CommitMenuItem::MergeIntoCurrent);
                }
            }
        }

        items.push(CommitMenuItem::CherryPick);

        if has_branch {
            if let Some(branch) = self.selected_branch() {
                if !branch.is_head {
                    items.push(CommitMenuItem::Rebase);
                }
            }
        }

        items.push(CommitMenuItem::Reset);
        items.push(CommitMenuItem::AddTag);

        // Tag operations only when the commit actually carries tags.
        if !self.selected_node_tags().is_empty() {
            items.push(CommitMenuItem::DeleteTag);
            items.push(CommitMenuItem::PushTag);
        }

        items.extend([
            CommitMenuItem::Revert,
        ]);

        // Compare: offer "compare with marked" once a different commit is
        // already marked, otherwise "mark for compare".
        match self.compare_marked {
            Some(marked) if Some(marked) != selected_oid => {
                items.push(CommitMenuItem::CompareWithMarked)
            }
            _ => items.push(CommitMenuItem::MarkForCompare),
        }

        // Prune stale remote-tracking refs — repo-level, offered when remotes
        // exist.
        if !self.repo.remotes().is_empty() {
            items.push(CommitMenuItem::Prune);
        }

        items.push(CommitMenuItem::CopyHash);
        items.push(CommitMenuItem::CopyMessage);


        self.mode = AppMode::CommitMenu {
            items,
            selected: 0,
            filter: String::new(),
        };
    }

    /// Local (non-remote) branch names pointing at the selected node.
    pub(crate) fn selected_node_local_branches(&self) -> Vec<String> {
        self.selected_node_branches()
            .iter()
            .filter(|name| {
                self.branches
                    .iter()
                    .any(|b| b.name == **name && !b.is_remote)
            })
            .map(|s| s.to_string())
            .collect()
    }

    /// Tag names pointing at the selected node.
    pub(crate) fn selected_node_tags(&self) -> Vec<String> {
        self.selected_commit_node()
            .map(|n| n.tag_names.clone())
            .unwrap_or_default()
    }

    /// The remote to push tags to: `origin` when present, else the sole remote,
    /// else `None` (no remote configured).
    fn default_push_remote(&self) -> Option<String> {
        let remotes = self.repo.repo().remotes().ok()?;
        let names: Vec<String> = remotes.iter().flatten().map(|s| s.to_string()).collect();
        if names.iter().any(|n| n == "origin") {
            Some("origin".to_string())
        } else {
            names.into_iter().next()
        }
    }

    /// Push a single tag to the default remote, reporting the result inline.
    pub(crate) fn push_tag_by_name(&mut self, tag: &str) {
        self.mode = AppMode::Normal;
        match self.default_push_remote() {
            Some(remote) => match push_tag(&self.repo_path, &remote, tag) {
                Ok(()) => self.toast(crate::toast::ToastKind::Success, format!("Pushed tag '{}' to {}", tag, remote)),
                Err(e) => self.toast(crate::toast::ToastKind::Error, format!("Push failed: {}", e)),
            },
            None => self.toast(crate::toast::ToastKind::Info, "No remote configured"),
        }
    }

    /// Delete the tag on the selected node: straight to Confirm for one tag,
    /// a picker when several tags share the commit.
    fn open_delete_tag_picker(&mut self) {
        let tags = self.selected_node_tags();
        match tags.len() {
            0 => {}
            1 => {
                self.mode = AppMode::Confirm {
                    message: format!("Delete tag '{}'?", tags[0]),
                    action: ConfirmAction::DeleteTag(tags[0].clone()),
                };
            }
            _ => {
                self.mode = AppMode::TagPicker {
                    tags,
                    selected: 0,
                    action: TagAction::Delete,
                };
            }
        }
    }

    /// Push the tag on the selected node: push directly for one tag, a picker
    /// when several tags share the commit.
    fn open_push_tag_picker(&mut self) {
        let tags = self.selected_node_tags();
        match tags.len() {
            0 => {}
            1 => {
                let tag = tags[0].clone();
                self.push_tag_by_name(&tag);
            }
            _ => {
                self.mode = AppMode::TagPicker {
                    tags,
                    selected: 0,
                    action: TagAction::Push,
                };
            }
        }
    }

    /// Open the stash options menu (staged / all / all+untracked) for the
    /// uncommitted node. Any typed commit-message text is carried through as the
    /// stash's default message.
    pub(crate) fn open_stash_menu(&mut self) {
        if !self.is_uncommitted_selected() {
            return;
        }
        self.mode = AppMode::CommitMenu {
            items: vec![
                CommitMenuItem::StashPushStaged,
                CommitMenuItem::StashPushAll,
                CommitMenuItem::StashPushUntracked,
            ],
            selected: 0,
            filter: String::new(),
        };
    }

    /// Prompt for an optional stash message before pushing, prefilled with the
    /// current commit-message editor text.
    fn prompt_stash_message(&mut self, scope: StashScope) {
        let prefill = self.commit_editor.text.trim().to_string();
        self.mode = AppMode::Input {
            title: "Stash message (optional)".to_string(),
            input: prefill,
            action: InputAction::StashPush { scope },
        };
    }

    pub(crate) fn handle_commit_menu_action(&mut self, action: Action) -> Result<()> {
        let AppMode::CommitMenu {
            items,
            selected,
            filter,
        } = &self.mode
        else {
            return Ok(());
        };
        let items = items.clone();
        let selected = *selected;
        let mut filter = filter.clone();

        match action {
            Action::MoveUp => {
                let visible = self.commit_menu_visible_count(&items, &filter);
                let new = cyclic_prev(selected, visible);
                self.mode = AppMode::CommitMenu {
                    items,
                    selected: new,
                    filter,
                };
            }
            Action::MoveDown => {
                let visible = self.commit_menu_visible_count(&items, &filter);
                let new = cyclic_next(selected, visible);
                self.mode = AppMode::CommitMenu {
                    items,
                    selected: new,
                    filter,
                };
            }
            Action::MenuSelect | Action::Confirm => {
                let ordered = self.commit_menu_ordered(&items, &filter);
                if let Some(item) = ordered.get(selected) {
                    self.execute_menu_item(*item)?;
                }
            }
            Action::InputChar(c) => {
                filter.push(c);
                self.mode = AppMode::CommitMenu {
                    items,
                    selected: 0,
                    filter,
                };
            }
            Action::InputBackspace => {
                filter.pop();
                self.mode = AppMode::CommitMenu {
                    items,
                    selected: 0,
                    filter,
                };
            }
            Action::InputBackspaceWord => {
                crate::text_editor::pop_word(&mut filter);
                self.mode = AppMode::CommitMenu {
                    items,
                    selected: 0,
                    filter,
                };
            }
            Action::InputClearLine => {
                self.mode = AppMode::CommitMenu {
                    items,
                    selected: 0,
                    filter: String::new(),
                };
            }
            Action::Cancel | Action::Quit => {
                self.mode = AppMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    fn commit_menu_ordered(
        &self,
        items: &[CommitMenuItem],
        filter: &str,
    ) -> Vec<CommitMenuItem> {
        if filter.is_empty() {
            return items.to_vec();
        }

        use fuzzy_matcher::skim::SkimMatcherV2;
        use fuzzy_matcher::FuzzyMatcher;
        let matcher = SkimMatcherV2::default();

        let mut scored: Vec<(CommitMenuItem, i64)> = items
            .iter()
            .filter_map(|item| {
                matcher
                    .fuzzy_match(item.label(), filter)
                    .map(|score| (*item, score))
            })
            .collect();

        scored.sort_by(|a, b| b.1.cmp(&a.1));

        scored.into_iter().map(|(item, _)| item).collect()
    }

    pub(crate) fn commit_menu_visible_count(&self, items: &[CommitMenuItem], filter: &str) -> usize {
        if filter.is_empty() {
            return items.len();
        }

        use fuzzy_matcher::skim::SkimMatcherV2;
        use fuzzy_matcher::FuzzyMatcher;
        let matcher = SkimMatcherV2::default();

        items
            .iter()
            .filter(|item| matcher.fuzzy_match(item.label(), filter).is_some())
            .count()
    }

    fn execute_menu_item(&mut self, item: CommitMenuItem) -> Result<()> {
        self.mode = AppMode::Normal;

        let commit_oid = self
            .selected_commit_node()
            .and_then(|n| n.commit.as_ref())
            .map(|c| c.oid);

        match item {
            CommitMenuItem::Checkout => self.do_checkout()?,
            CommitMenuItem::CreateBranch => {
                self.mode = AppMode::Input {
                    title: "New Branch Name".to_string(),
                    input: String::new(),
                    action: InputAction::CreateBranch,
                };
            }
            CommitMenuItem::DeleteBranch => {
                self.open_delete_branch_picker();
            }
            CommitMenuItem::MergeIntoCurrent => {
                if self.block_if_op_in_progress("merge") {
                    return Ok(());
                }
                if let Some(branch) = self.selected_branch() {
                    if !branch.is_head {
                        self.mode = AppMode::Confirm {
                            message: format!("Merge '{}' into current branch?", branch.name),
                            action: ConfirmAction::Merge {
                                name: branch.name.clone(),
                                is_remote: branch.is_remote,
                            },
                        };
                    }
                }
            }
            CommitMenuItem::CherryPick => {
                if self.block_if_op_in_progress("cherry-pick") {
                    return Ok(());
                }
                if let Some(oid) = commit_oid {
                    self.mode = AppMode::Confirm {
                        message: format!("Cherry-pick commit {}?", short_hash(oid)),
                        action: ConfirmAction::CherryPick(oid),
                    };
                }
            }
            CommitMenuItem::Rebase => {
                if self.block_if_op_in_progress("rebase") {
                    return Ok(());
                }
                if let Some(branch) = self.selected_branch() {
                    if !branch.is_head {
                        self.mode = AppMode::Confirm {
                            message: format!("Rebase current branch onto '{}'?", branch.name),
                            action: ConfirmAction::Rebase {
                                name: branch.name.clone(),
                                is_remote: branch.is_remote,
                            },
                        };
                    }
                }
            }
            CommitMenuItem::Reset => {
                // Open reset submenu
                self.mode = AppMode::CommitMenu {
                    items: vec![
                        CommitMenuItem::ResetSoft,
                        CommitMenuItem::ResetMixed,
                        CommitMenuItem::ResetHard,
                    ],
                    selected: 0,
                    filter: String::new(),
                };
            }
            CommitMenuItem::ResetSoft => {
                if let Some(oid) = commit_oid {
                    self.mode = AppMode::Confirm {
                        message: format!("Reset (soft) to {}?", short_hash(oid)),
                        action: ConfirmAction::ResetSoft(oid),
                    };
                }
            }
            CommitMenuItem::ResetMixed => {
                if let Some(oid) = commit_oid {
                    self.mode = AppMode::Confirm {
                        message: format!("Reset (mixed) to {}?", short_hash(oid)),
                        action: ConfirmAction::ResetMixed(oid),
                    };
                }
            }
            CommitMenuItem::ResetHard => {
                if let Some(oid) = commit_oid {
                    self.mode = AppMode::Confirm {
                        message: format!(
                            "Reset (HARD) to {}? This will discard changes!",
                            short_hash(oid)
                        ),
                        action: ConfirmAction::ResetHard(oid),
                    };
                }
            }
            CommitMenuItem::RenameBranch => {
                let locals = self.selected_node_local_branches();
                // Prefer the branch the selection is anchored to; otherwise the
                // first local branch label on the node.
                let old = self
                    .selected_branch_name()
                    .map(|s| s.to_string())
                    .filter(|n| locals.contains(n))
                    .or_else(|| locals.first().cloned());
                if let Some(old) = old {
                    self.mode = AppMode::Input {
                        title: format!("Rename '{}' to", old),
                        input: old.clone(),
                        action: InputAction::RenameBranch { old_name: old },
                    };
                }
            }
            CommitMenuItem::AddTag => {
                self.mode = AppMode::Input {
                    title: "Tag Name".to_string(),
                    input: String::new(),
                    action: InputAction::AddTag,
                };
            }
            CommitMenuItem::DeleteTag => {
                self.open_delete_tag_picker();
            }
            CommitMenuItem::PushTag => {
                self.open_push_tag_picker();
            }
            CommitMenuItem::Revert => {
                if self.block_if_op_in_progress("revert") {
                    return Ok(());
                }
                if let Some(oid) = commit_oid {
                    self.mode = AppMode::Confirm {
                        message: format!("Revert commit {}?", short_hash(oid)),
                        action: ConfirmAction::Revert(oid),
                    };
                }
            }
            CommitMenuItem::CopyHash => {
                if let Some(oid) = commit_oid {
                    let hash = oid.to_string();
                    match copy_to_clipboard(&hash) {
                        Ok(outcome) => self
                            .toast(crate::toast::ToastKind::Success, format!("Copied {}{}", short_hash(oid), outcome.suffix())),
                        Err(e) => self.toast(crate::toast::ToastKind::Error, format!("Clipboard error: {}", e)),
                    }
                }
            }
            CommitMenuItem::CopyMessage => {
                if let Some(msg) = self
                    .selected_commit_node()
                    .and_then(|n| n.commit.as_ref())
                    .map(|c| c.full_message.clone())
                {
                    match copy_to_clipboard(&msg) {
                        Ok(outcome) => self.toast(crate::toast::ToastKind::Success, format!(
                            "Copied commit message{}",
                            outcome.suffix()
                        )),
                        Err(e) => self.toast(crate::toast::ToastKind::Error, format!("Clipboard error: {}", e)),
                    }
                }
            }
            CommitMenuItem::MarkForCompare | CommitMenuItem::CompareWithMarked => {
                self.mark_or_compare_selected();
            }
            CommitMenuItem::Push => {
                self.initiate_push();
            }
            CommitMenuItem::Pull => {
                self.initiate_pull();
            }
            CommitMenuItem::Prune => {
                self.initiate_prune();
            }
            CommitMenuItem::CreatePr => {
                self.open_create_pr();
            }
            CommitMenuItem::MergePr => {
                self.open_merge_pr();
            }
            CommitMenuItem::StashApply => {
                if self.block_if_op_in_progress("apply a stash") {
                    return Ok(());
                }
                if let Some(index) = self.selected_stash_index() {
                    let outcome = stash_apply(&self.repo_path, index)?;
                    self.handle_stash_outcome(outcome, "applied", "apply")?;
                }
            }
            CommitMenuItem::StashPop => {
                if self.block_if_op_in_progress("pop a stash") {
                    return Ok(());
                }
                if let Some(index) = self.selected_stash_index() {
                    let outcome = stash_pop(&self.repo_path, index)?;
                    self.handle_stash_outcome(outcome, "popped", "pop")?;
                }
            }
            CommitMenuItem::StashDrop => {
                if let Some(index) = self.selected_stash_index() {
                    self.mode = AppMode::Confirm {
                        message: format!("Drop stash@{{{}}}? This cannot be undone.", index),
                        action: ConfirmAction::StashDrop(index),
                    };
                }
            }
            CommitMenuItem::BranchFromStash => {
                if let Some(index) = self.selected_stash_index() {
                    self.mode = AppMode::Input {
                        title: "Branch from stash".to_string(),
                        input: String::new(),
                        action: InputAction::BranchFromStash { index },
                    };
                }
            }
            CommitMenuItem::StashPushStaged => self.prompt_stash_message(StashScope::Staged),
            CommitMenuItem::StashPushAll => self.prompt_stash_message(StashScope::All),
            CommitMenuItem::StashPushUntracked => {
                self.prompt_stash_message(StashScope::AllUntracked)
            }
        }
        Ok(())
    }
}
