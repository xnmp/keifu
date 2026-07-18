//! Graph panel: navigation, selection, checkout, commit filter.

use super::*;

impl App {
    pub(crate) fn handle_normal_action(&mut self, action: Action) -> Result<()> {
        // Panel navigation works from any panel
        match action {
            Action::PanelLeft => {
                self.editing_commit_message = false;
                self.focused_panel = match self.focused_panel {
                    FocusedPanel::Graph => FocusedPanel::CommitDetail,
                    FocusedPanel::CommitDetail => FocusedPanel::Files,
                    FocusedPanel::Files => FocusedPanel::Graph,
                };
                return Ok(());
            }
            Action::PanelRight => {
                self.editing_commit_message = false;
                self.focused_panel = match self.focused_panel {
                    FocusedPanel::Graph => FocusedPanel::Files,
                    FocusedPanel::Files => FocusedPanel::CommitDetail,
                    FocusedPanel::CommitDetail => FocusedPanel::Graph,
                };
                return Ok(());
            }
            Action::FocusGraph => {
                if self.editing_commit_message {
                    self.editing_commit_message = false;
                } else {
                    self.focused_panel = FocusedPanel::Graph;
                    self.files_pane.files_filter.clear();
                    self.files_pane.files_filter_active = false;
                }
                return Ok(());
            }
            _ => {}
        }

        // Route to panel-specific handler
        match self.focused_panel {
            FocusedPanel::Graph => self.handle_graph_action(action),
            FocusedPanel::Files => self.handle_files_action(action),
            FocusedPanel::CommitDetail => self.handle_commit_detail_action(action),
        }
    }

    fn handle_graph_action(&mut self, action: Action) -> Result<()> {
        // Reset commit detail scroll on any graph navigation
        if matches!(
            action,
            Action::MoveUp
                | Action::MoveDown
                | Action::PageUp
                | Action::PageDown
                | Action::GoToTop
                | Action::GoToBottom
                | Action::JumpToHead
                | Action::NextBranch
                | Action::PrevBranch
        ) {
            self.commit_detail_scroll = 0;
        }
        match action {
            Action::Quit => {
                // Esc first clears a pending mark / active comparison; only quit
                // when there's nothing to dismiss.
                if !self.clear_compare() {
                    self.should_quit = true;
                }
            }
            Action::MarkForCompare => {
                self.mark_or_compare_selected();
            }
            Action::OpenPr => {
                self.open_selected_pr();
            }
            Action::OpenCiChecks => {
                self.open_ci_checks();
            }
            Action::OpenPrThread => {
                self.open_pr_thread();
            }
            Action::OpenMetadataMenu => {
                self.mode = AppMode::MetadataMenu { selected: 0 };
            }
            Action::ToggleTrace => {
                self.toggle_trace();
            }
            Action::ShrinkGraphWidth => {
                self.resize_graph_width(-1);
            }
            Action::WidenGraphWidth => {
                self.resize_graph_width(1);
            }
            Action::MoveUp => {
                self.move_selection(-1);
            }
            Action::MoveDown => {
                self.move_selection(1);
            }
            Action::PageUp => {
                self.move_selection(-10);
            }
            Action::PageDown => {
                self.move_selection(10);
            }
            Action::GoToTop => {
                self.select_first();
            }
            Action::GoToBottom => {
                self.select_last();
            }
            Action::JumpToHead => {
                self.jump_to_head();
            }
            Action::NextBranch => {
                self.move_to_next_branch();
            }
            Action::PrevBranch => {
                self.move_to_prev_branch();
            }
            Action::ToggleHelp => {
                self.mode = AppMode::Help;
            }
            Action::Refresh => {
                self.refresh(true)?;
                self.reset_timers();
            }
            Action::Fetch => {
                self.initiate_fetch();
            }
            Action::Pull => {
                self.initiate_pull();
            }
            Action::Push => {
                self.initiate_push();
            }
            Action::OpenCommitMenu => {
                self.open_commit_menu();
            }
            Action::OpenBranchFilter => {
                self.open_branch_filter();
            }
            Action::CreateBranch => {
                self.mode = AppMode::Input {
                    title: "New Branch Name".to_string(),
                    input: String::new(),
                    action: InputAction::CreateBranch,
                };
            }
            Action::Search => {
                self.save_search_position();
                self.mode = AppMode::Input {
                    title: "Search branches".to_string(),
                    input: String::new(),
                    action: InputAction::Search,
                };
            }
            Action::DeleteBranch => {
                self.open_delete_branch_picker();
            }
            Action::OpenFileDiff => {
                self.sync_file_list_cache();
                if let Some(file) = self.selected_file().cloned() {
                    let target = self
                        .current_diff_target()
                        .unwrap_or(DiffTarget::Uncommitted);
                    let file_list = self.files_pane.display_file_list();
                    let flat_idx = self.display_index_to_flat_index(self.file_selected_index());
                    if let Err(e) = self.enter_file_diff(target, flat_idx, file_list, &file.path) {
                        self.set_message(format!("Cannot open diff: {e}"));
                    }
                } else if self.is_diff_loading() {
                    self.set_message("Loading diff...");
                } else {
                    self.set_message("Diff not available");
                }
            }
            Action::StartCommitFilter => {
                self.commit_filter_active = true;
                self.commit_filter.clear();
                self.recompute_visible_commits();
            }
            Action::CommitFilterChar(c) => {
                self.commit_filter.push(c);
                self.recompute_visible_commits();
            }
            Action::CommitFilterBackspace => {
                if !self.commit_filter.is_empty() {
                    self.commit_filter.pop();
                    self.recompute_visible_commits();
                } else {
                    self.commit_filter_active = false;
                    self.recompute_visible_commits();
                }
            }
            Action::InputBackspaceWord if self.commit_filter_active => {
                crate::text_editor::pop_word(&mut self.commit_filter);
                self.recompute_visible_commits();
            }
            Action::InputClearLine if self.commit_filter_active => {
                self.commit_filter.clear();
                self.recompute_visible_commits();
            }
            Action::Confirm if self.commit_filter_active => {
                self.commit_filter_active = false;
            }
            Action::Cancel if self.commit_filter_active => {
                self.commit_filter.clear();
                self.commit_filter_active = false;
                self.recompute_visible_commits();
            }
            _ => {}
        }
        Ok(())
    }

    fn move_selection(&mut self, delta: i32) {
        if !self.commit_filter.is_empty() && !self.visible_commit_indices.is_empty() {
            let current = self.graph_nav.graph_list_state.selected().unwrap_or(0);
            let pos = self
                .visible_commit_indices
                .iter()
                .position(|&idx| idx == current)
                .unwrap_or(0);
            let new_pos =
                (pos as i32 + delta).clamp(0, self.visible_commit_indices.len() as i32 - 1)
                    as usize;
            let new_idx = self.visible_commit_indices[new_pos];
            self.graph_nav.graph_list_state.select(Some(new_idx));
            self.graph_nav.sync_branch_selection_to_node(new_idx);
        } else {
            self.graph_nav.move_selection(&self.graph_layout, delta);
        }
    }

    fn select_first(&mut self) {
        if !self.commit_filter.is_empty() && !self.visible_commit_indices.is_empty() {
            let idx = self.visible_commit_indices[0];
            self.graph_nav.graph_list_state.select(Some(idx));
            self.graph_nav.sync_branch_selection_to_node(idx);
        } else {
            self.graph_nav.select_first(&self.graph_layout);
        }
    }

    fn select_last(&mut self) {
        if !self.commit_filter.is_empty() && !self.visible_commit_indices.is_empty() {
            let idx = *self.visible_commit_indices.last().unwrap();
            self.graph_nav.graph_list_state.select(Some(idx));
            self.graph_nav.sync_branch_selection_to_node(idx);
        } else {
            self.graph_nav.select_last(&self.graph_layout);
        }
    }

    fn move_to_next_branch(&mut self) {
        if !self.commit_filter.is_empty() {
            self.move_to_next_visible_branch(true);
        } else {
            self.graph_nav.move_to_next_branch();
        }
    }

    fn move_to_prev_branch(&mut self) {
        if !self.commit_filter.is_empty() {
            self.move_to_next_visible_branch(false);
        } else {
            self.graph_nav.move_to_prev_branch();
        }
    }

    fn move_to_next_visible_branch(&mut self, forward: bool) {
        let current_pos = self.graph_nav.selected_branch_position.unwrap_or(0);
        let len = self.graph_nav.branch_positions.len();
        if len == 0 {
            return;
        }
        let mut pos = current_pos;
        loop {
            if forward {
                if pos + 1 >= len {
                    return;
                }
                pos += 1;
            } else {
                if pos == 0 {
                    return;
                }
                pos -= 1;
            }
            if let Some((node_idx, _)) = self.graph_nav.branch_positions.get(pos) {
                if self.visible_commit_indices.contains(node_idx) {
                    self.graph_nav.selected_branch_position = Some(pos);
                    self.graph_nav.graph_list_state.select(Some(*node_idx));
                    return;
                }
            }
        }
    }

    pub(crate) fn selected_branch(&self) -> Option<&BranchInfo> {
        self.graph_nav.selected_branch(&self.branches)
    }

    pub fn selected_branch_name(&self) -> Option<&str> {
        self.graph_nav.selected_branch_name()
    }

    pub fn selected_node_branches(&self) -> Vec<&str> {
        self.graph_nav.selected_node_branches()
    }

    pub(crate) fn selected_commit_node(&self) -> Option<&crate::git::graph::GraphNode> {
        self.graph_nav.selected_node(&self.graph_layout)
    }

    /// Open the selected commit's associated open PR in the browser, if any of
    /// its branch labels match an open PR's head branch.
    pub(crate) fn open_selected_pr(&mut self) {
        let pr = self.selected_commit_node().and_then(|node| {
            crate::ui::graph_view::pr_for_branch_labels(
                &node.branch_names,
                &self.remotes,
                &self.open_prs,
            )
            .cloned()
        });
        match pr {
            Some(pr) => {
                if let Err(e) = open_url(&pr.url) {
                    self.show_error(format!("Could not open PR: {e}"));
                } else {
                    self.set_message(format!("Opening PR #{} in browser", pr.number));
                }
            }
            None => self.set_message("No open PR for this commit"),
        }
    }

    /// Adjust the graph column width cap by `direction` lanes (each lane = 2
    /// cells): negative shrinks (adds/tightens the cap, floor 4 cells), positive
    /// widens (loosens it; past the width needed to fit all lanes it becomes
    /// uncapped). Persists the choice.
    pub(crate) fn resize_graph_width(&mut self, direction: i32) {
        let needed = (self.graph_layout.max_lane + 1) * 2;
        self.graph_width_cap =
            crate::ui::graph_view::next_graph_cap(needed, self.graph_width_cap, direction);
        self.save_ui_state();
    }

    pub(crate) fn do_checkout(&mut self) -> Result<()> {
        let branches: Vec<String> = self
            .selected_node_branches()
            .iter()
            .map(|s| s.to_string())
            .collect();

        match branches.len() {
            0 => {
                if let Some(node) = self.selected_commit_node() {
                    if let Some(commit) = &node.commit {
                        checkout_commit(self.repo.repo(), commit.oid)?;
                        self.refresh(true)?;
                    }
                }
            }
            1 => {
                self.checkout_branch_by_name(&branches[0])?;
            }
            _ => {
                self.mode = AppMode::BranchPicker {
                    branches,
                    selected: 0,
                };
            }
        }
        Ok(())
    }

    pub(crate) fn checkout_branch_by_name(&mut self, branch_name: &str) -> Result<()> {
        if branch_name.starts_with("origin/") {
            checkout_remote_branch(self.repo.repo(), branch_name)?;
        } else {
            checkout_branch(self.repo.repo(), branch_name)?;
        }
        self.refresh(true)?;
        Ok(())
    }

    pub fn node_passes_commit_filter(&self, node: &crate::git::graph::GraphNode) -> bool {
        if self.commit_filter.is_empty() {
            return true;
        }
        if node.is_uncommitted {
            return true;
        }
        let Some(commit) = &node.commit else {
            return false;
        };
        let query = self.commit_filter.to_lowercase();
        commit.message.to_lowercase().contains(&query)
            || commit.author_name.to_lowercase().contains(&query)
            || commit.short_id.to_lowercase().contains(&query)
    }

    pub fn recompute_visible_commits(&mut self) {
        self.visible_commit_indices = self
            .graph_layout
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| self.node_passes_commit_filter(node))
            .map(|(idx, _)| idx)
            .collect();

        // Ensure selection is within visible range
        if !self.visible_commit_indices.is_empty() {
            let current = self.graph_nav.graph_list_state.selected().unwrap_or(0);
            if !self.visible_commit_indices.contains(&current) {
                // Move to nearest visible node
                let nearest = self
                    .visible_commit_indices
                    .iter()
                    .min_by_key(|&&idx| (idx as i64 - current as i64).unsigned_abs())
                    .copied()
                    .unwrap_or(0);
                self.graph_nav.graph_list_state.select(Some(nearest));
            }
        }
    }

    fn jump_to_head(&mut self) {
        self.graph_nav.jump_to_head(self.head_name.as_deref());
    }
}
