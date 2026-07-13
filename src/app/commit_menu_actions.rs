//! Commit context menu: open, filter, execute.

use super::*;

impl App {
    pub(crate) fn open_delete_branch_picker(&mut self) {
        let deletable: Vec<String> = self
            .selected_node_branches()
            .iter()
            .filter(|name| {
                self.branches
                    .iter()
                    .any(|b| b.name == **name && !b.is_head && !b.is_remote)
            })
            .map(|s| s.to_string())
            .collect();

        match deletable.len() {
            0 => {}
            1 => {
                self.mode = AppMode::Confirm {
                    message: format!("Delete branch '{}'?", deletable[0]),
                    action: ConfirmAction::DeleteBranch(deletable[0].clone()),
                };
            }
            _ => {
                self.mode = AppMode::BranchDeletePicker {
                    branches: deletable,
                    selected: 0,
                };
            }
        }
    }

    pub(crate) fn open_commit_menu(&mut self) {
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
                CommitMenuItem::StashDrop,
            ];
            self.mode = AppMode::CommitMenu {
                items,
                selected: 0,
                filter: String::new(),
            };
            return;
        }

        let has_branch = self.selected_branch().is_some();
        let mut items = Vec::new();

        // Push at top if available
        if has_branch {
            items.push(CommitMenuItem::Push);
        }

        items.push(CommitMenuItem::Checkout);
        items.push(CommitMenuItem::CreateBranch);

        let has_deletable_branch = self.selected_node_branches().iter().any(|name| {
            self.branches
                .iter()
                .any(|b| b.name == *name && !b.is_head && !b.is_remote)
        });
        if has_deletable_branch {
            items.push(CommitMenuItem::DeleteBranch);
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

        items.extend([
            CommitMenuItem::Reset,
            CommitMenuItem::AddTag,
            CommitMenuItem::Revert,
            CommitMenuItem::CopyHash,
        ]);

        self.mode = AppMode::CommitMenu {
            items,
            selected: 0,
            filter: String::new(),
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

    fn commit_menu_visible_count(&self, items: &[CommitMenuItem], filter: &str) -> usize {
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
                if let Some(branch) = self.selected_branch() {
                    if !branch.is_head {
                        self.mode = AppMode::Confirm {
                            message: format!("Merge '{}' into current branch?", branch.name),
                            action: ConfirmAction::Merge(branch.name.clone()),
                        };
                    }
                }
            }
            CommitMenuItem::CherryPick => {
                if let Some(oid) = commit_oid {
                    self.mode = AppMode::Confirm {
                        message: format!("Cherry-pick commit {}?", &oid.to_string()[..7]),
                        action: ConfirmAction::CherryPick(oid),
                    };
                }
            }
            CommitMenuItem::Rebase => {
                if let Some(branch) = self.selected_branch() {
                    if !branch.is_head {
                        self.mode = AppMode::Confirm {
                            message: format!("Rebase current branch onto '{}'?", branch.name),
                            action: ConfirmAction::Rebase(branch.name.clone()),
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
                        message: format!("Reset (soft) to {}?", &oid.to_string()[..7]),
                        action: ConfirmAction::ResetSoft(oid),
                    };
                }
            }
            CommitMenuItem::ResetMixed => {
                if let Some(oid) = commit_oid {
                    self.mode = AppMode::Confirm {
                        message: format!("Reset (mixed) to {}?", &oid.to_string()[..7]),
                        action: ConfirmAction::ResetMixed(oid),
                    };
                }
            }
            CommitMenuItem::ResetHard => {
                if let Some(oid) = commit_oid {
                    self.mode = AppMode::Confirm {
                        message: format!(
                            "Reset (HARD) to {}? This will discard changes!",
                            &oid.to_string()[..7]
                        ),
                        action: ConfirmAction::ResetHard(oid),
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
            CommitMenuItem::Revert => {
                if let Some(oid) = commit_oid {
                    self.mode = AppMode::Confirm {
                        message: format!("Revert commit {}?", &oid.to_string()[..7]),
                        action: ConfirmAction::Revert(oid),
                    };
                }
            }
            CommitMenuItem::CopyHash => {
                if let Some(oid) = commit_oid {
                    let hash = oid.to_string();
                    match copy_to_clipboard(&hash) {
                        Ok(()) => self.set_message(format!("Copied {}", &hash[..7])),
                        Err(e) => self.set_message(format!("Clipboard error: {}", e)),
                    }
                }
            }
            CommitMenuItem::Push => {
                self.start_push();
            }
            CommitMenuItem::StashApply => {
                if let Some(index) = self.selected_stash_index() {
                    stash_apply(&self.repo_path, index)?;
                    self.refresh(false)?;
                    self.set_message("Stash applied");
                }
            }
            CommitMenuItem::StashPop => {
                if let Some(index) = self.selected_stash_index() {
                    stash_pop(&self.repo_path, index)?;
                    self.refresh(true)?;
                    self.set_message("Stash popped");
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
        }
        Ok(())
    }
}
