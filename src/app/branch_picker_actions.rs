//! Branch picker, delete-picker, and branch-filter modes.

use super::*;

impl App {
    pub(crate) fn handle_branch_picker_action(&mut self, action: Action) -> Result<()> {
        let AppMode::BranchPicker { branches, selected } = &self.mode else {
            return Ok(());
        };
        let branches = branches.clone();
        let selected = *selected;

        match action {
            Action::MoveUp => {
                let new = cyclic_prev(selected, branches.len());
                self.mode = AppMode::BranchPicker { branches, selected: new };
            }
            Action::MoveDown => {
                let new = cyclic_next(selected, branches.len());
                self.mode = AppMode::BranchPicker { branches, selected: new };
            }
            Action::MenuSelect | Action::Confirm => {
                if let Some(branch_name) = branches.get(selected) {
                    let name = branch_name.clone();
                    self.mode = AppMode::Normal;
                    self.checkout_branch_by_name(&name)?;
                }
            }
            Action::Cancel | Action::Quit => {
                self.mode = AppMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn handle_branch_delete_picker_action(&mut self, action: Action) -> Result<()> {
        let AppMode::BranchDeletePicker { branches, selected } = &self.mode else {
            return Ok(());
        };
        let branches = branches.clone();
        let selected = *selected;

        match action {
            Action::MoveUp => {
                let new = cyclic_prev(selected, branches.len());
                self.mode = AppMode::BranchDeletePicker {
                    branches,
                    selected: new,
                };
            }
            Action::MoveDown => {
                let new = cyclic_next(selected, branches.len());
                self.mode = AppMode::BranchDeletePicker {
                    branches,
                    selected: new,
                };
            }
            Action::MenuSelect | Action::Confirm => {
                if let Some(branch_name) = branches.get(selected) {
                    self.confirm_delete_branch(branch_name.clone());
                }
            }
            Action::Cancel | Action::Quit => {
                self.mode = AppMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn handle_tag_picker_action(&mut self, action: Action) -> Result<()> {
        let AppMode::TagPicker {
            tags,
            selected,
            action: tag_action,
        } = &self.mode
        else {
            return Ok(());
        };
        let tags = tags.clone();
        let selected = *selected;
        let tag_action = *tag_action;

        match action {
            Action::MoveUp => {
                let new = cyclic_prev(selected, tags.len());
                self.mode = AppMode::TagPicker {
                    tags,
                    selected: new,
                    action: tag_action,
                };
            }
            Action::MoveDown => {
                let new = cyclic_next(selected, tags.len());
                self.mode = AppMode::TagPicker {
                    tags,
                    selected: new,
                    action: tag_action,
                };
            }
            Action::MenuSelect | Action::Confirm => {
                if let Some(tag) = tags.get(selected) {
                    match tag_action {
                        TagAction::Delete => {
                            self.mode = AppMode::Confirm {
                                message: format!("Delete tag '{}'?", tag),
                                action: ConfirmAction::DeleteTag(tag.clone()),
                            };
                        }
                        TagAction::Push => {
                            let tag = tag.clone();
                            self.push_tag_by_name(&tag);
                        }
                    }
                }
            }
            Action::Cancel | Action::Quit => {
                self.mode = AppMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    /// Recompute the cached branch -> author map, but only when the branch
    /// tips have changed since it was last built. Keyed on a sorted snapshot of
    /// `(name, tip OID)` so that adding/removing branches or advancing any tip
    /// invalidates the cache, while repeated picker opens over an unchanged
    /// repo reuse the previous result. Attribution walks history per branch, so
    /// this is deliberately called on picker open, not on every refresh.
    fn ensure_branch_authors(&mut self) {
        let mut key: Vec<(String, git2::Oid)> = self
            .branches
            .iter()
            .map(|b| (b.name.clone(), b.tip_oid))
            .collect();
        key.sort();
        if key != self.branch_authors_key {
            self.branch_authors = crate::git::branch_authors(self.repo.repo(), &self.branches);
            self.branch_authors_key = key;
        }
    }

    /// Author name attributed to `branch`, or an empty string when unknown.
    fn branch_author(&self, branch: &str) -> &str {
        self.branch_authors
            .get(branch)
            .map(String::as_str)
            .unwrap_or("")
    }

    pub(crate) fn open_branch_filter(&mut self) {
        self.ensure_branch_authors();
        let mut all_branches: Vec<String> = self
            .branches
            .iter()
            .map(|b| b.name.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        all_branches.sort();
        self.mode = AppMode::BranchFilter {
            filter: String::new(),
            selected: 0,
            all_branches,
        };
    }

    pub(crate) fn handle_branch_filter_action(&mut self, action: Action) -> Result<()> {
        let AppMode::BranchFilter {
            filter,
            selected,
            all_branches,
        } = &self.mode
        else {
            return Ok(());
        };
        let filter = filter.clone();
        let selected = *selected;
        let all_branches = all_branches.clone();

        // Compute filtered list for navigation. A filter beginning with `@`
        // matches the branch author; any other filter matches the name.
        let filtered: Vec<&String> = all_branches
            .iter()
            .filter(|b| {
                crate::ui::branch_filter::matches_branch_filter(b, self.branch_author(b), &filter)
            })
            .collect();

        match action {
            Action::MoveUp => {
                if filtered.is_empty() {
                    return Ok(());
                }
                let new = cyclic_prev(selected, filtered.len());
                self.mode = AppMode::BranchFilter {
                    filter,
                    selected: new,
                    all_branches,
                };
            }
            Action::MoveDown => {
                if filtered.is_empty() {
                    return Ok(());
                }
                let new = cyclic_next(selected, filtered.len());
                self.mode = AppMode::BranchFilter {
                    filter,
                    selected: new,
                    all_branches,
                };
            }
            Action::Confirm | Action::MenuSelect => {
                // Toggle the selected branch
                if let Some(branch_name) = filtered.get(selected) {
                    let name = (*branch_name).clone();
                    if self.hidden_branches.contains(&name) {
                        self.hidden_branches.remove(&name);
                    } else {
                        self.hidden_branches.insert(name);
                    }
                }
                // Stay in BranchFilter mode
                self.mode = AppMode::BranchFilter {
                    filter,
                    selected,
                    all_branches,
                };
            }
            Action::SelectAll => {
                // Show (un-hide) the currently filtered branches. With no
                // active filter this is every branch — identical to the old
                // "show all" — but under an `@author`/name filter it scopes to
                // just the visible subset.
                for b in &filtered {
                    self.hidden_branches.remove(*b);
                }
                self.mode = AppMode::BranchFilter {
                    filter,
                    selected,
                    all_branches,
                };
            }
            Action::SelectNone => {
                // Hide the currently filtered branches. With no filter this
                // hides every branch; with `@alice` it hides all of alice's
                // branches at once — the point of the author filter.
                for b in &filtered {
                    self.hidden_branches.insert((*b).clone());
                }
                self.mode = AppMode::BranchFilter {
                    filter,
                    selected,
                    all_branches,
                };
            }
            Action::InputChar(c) => {
                let mut new_filter = filter;
                new_filter.push(c);
                // Reset selection when filter changes
                self.mode = AppMode::BranchFilter {
                    filter: new_filter,
                    selected: 0,
                    all_branches,
                };
            }
            Action::InputBackspace => {
                let mut new_filter = filter;
                new_filter.pop();
                self.mode = AppMode::BranchFilter {
                    filter: new_filter,
                    selected: 0,
                    all_branches,
                };
            }
            Action::InputBackspaceWord => {
                let mut new_filter = filter;
                crate::text_editor::pop_word(&mut new_filter);
                self.mode = AppMode::BranchFilter {
                    filter: new_filter,
                    selected: 0,
                    all_branches,
                };
            }
            Action::InputClearLine => {
                self.mode = AppMode::BranchFilter {
                    filter: String::new(),
                    selected: 0,
                    all_branches,
                };
            }
            Action::Cancel | Action::Quit => {
                self.mode = AppMode::Normal;
                self.refresh(true)?;
            }
            _ => {}
        }
        Ok(())
    }
}
