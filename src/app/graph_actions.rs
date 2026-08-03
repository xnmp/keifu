//! Graph panel: navigation, selection, checkout, commit filter.

use super::*;

/// The query accepted by the graph's Ctrl+Shift+F filter entry. Clauses are
/// separated by semicolons so message text can retain spaces, for example:
/// `message=fix parser; author=ada; file=src/`.
#[derive(Default)]
struct CommitFilterQuery {
    text: String,
    message: String,
    author: String,
    file: String,
}

impl CommitFilterQuery {
    fn parse(input: &str) -> Self {
        let mut query = Self::default();
        for clause in input
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            let Some((field, value)) = clause.split_once('=') else {
                query.text = clause.to_string();
                continue;
            };
            match field.trim().to_ascii_lowercase().as_str() {
                "message" | "msg" => query.message = value.trim().to_string(),
                "author" => query.author = value.trim().to_string(),
                "file" | "path" => query.file = value.trim().to_string(),
                _ => query.text = clause.to_string(),
            }
        }
        query
    }

    fn is_active(&self) -> bool {
        !self.text.is_empty()
            || !self.message.is_empty()
            || !self.author.is_empty()
            || !self.file.is_empty()
    }

    fn matches(&self, commit: &crate::git::CommitInfo, changed_paths: &[String]) -> bool {
        let text = self.text.to_lowercase();
        let message = self.message.to_lowercase();
        let author = self.author.to_lowercase();
        (text.is_empty()
            || commit.message.to_lowercase().contains(&text)
            || commit.author_name.to_lowercase().contains(&text)
            || commit.author_email.to_lowercase().contains(&text)
            || commit.short_id.to_lowercase().contains(&text))
            && (message.is_empty() || commit.full_message.to_lowercase().contains(&message))
            && (author.is_empty()
                || commit.author_name.to_lowercase().contains(&author)
                || commit.author_email.to_lowercase().contains(&author))
            && (self.file.is_empty() || changed_paths.iter().any(|path| path.contains(&self.file)))
    }
}

impl App {
    pub(crate) fn handle_normal_action(&mut self, action: Action) -> Result<()> {
        // Esc targets a lingering error toast before any other meaning (#116):
        // a red toast is the most prominent thing on screen, so the first Esc
        // dismisses it instead of quitting / cancelling; the next Esc proceeds
        // as usual. Info/success toasts never intercept (they expire quickly,
        // and swallowing an Esc for them would make quit feel unreliable).
        if matches!(action, Action::Quit | Action::Cancel) && self.toasts.dismiss_errors() {
            return Ok(());
        }
        // Panel navigation works from any panel
        match action {
            Action::PanelLeft => {
                self.editing_commit_message = false;
                // Cycle only through visible panels (#116).
                self.focused_panel = self.next_visible_panel(self.focused_panel, false);
                return Ok(());
            }
            Action::PanelRight => {
                self.editing_commit_message = false;
                self.focused_panel = self.next_visible_panel(self.focused_panel, true);
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
            // The issue list opens from any panel in Normal mode.
            Action::OpenIssueList => {
                self.open_issue_list();
                return Ok(());
            }
            // Pane visibility toggles work from any panel (#116) — hiding the
            // focused pane moves focus to the graph inside the toggle.
            Action::ToggleFilesPane => {
                self.toggle_files_pane();
                return Ok(());
            }
            Action::ToggleCommitPane => {
                self.toggle_commit_pane();
                return Ok(());
            }
            // Commit search is global, but its results live in the graph.
            Action::StartCommitFilter => {
                self.focused_panel = FocusedPanel::Graph;
                self.commit_filter_active = true;
                self.commit_filter.clear();
                self.rebuild_graph()?;
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
                | Action::SameLaneUp
                | Action::SameLaneDown
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
            Action::CreatePullRequest => {
                self.open_create_pr();
            }
            Action::MergePullRequest => {
                self.open_merge_pr();
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
            Action::JumpToMergeBase => {
                self.jump_to_merge_base();
            }
            Action::UndoLastOp => {
                self.request_undo();
            }
            Action::LoadMoreCommits => {
                self.load_more_commits(false);
            }
            Action::LoadAllCommits => {
                // Confirm first — "load all" can be a big walk on huge repos.
                self.mode = AppMode::Confirm {
                    message: "Load ALL commits? This may take a moment on large repositories."
                        .to_string(),
                    action: ConfirmAction::LoadAllCommits,
                };
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
            Action::SameLaneUp => {
                self.jump_same_lane(crate::git::graph::same_lane_descendant_row);
            }
            Action::SameLaneDown => {
                self.jump_same_lane(crate::git::graph::same_lane_ancestor_row);
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
            Action::ToggleRemoteBranches => {
                self.toggle_remote_branches()?;
            }
            Action::ToggleMergedBranches => {
                self.toggle_merged_branches()?;
            }
            Action::CreateBranch => {
                self.input_commit_target = None;
                self.mode = AppMode::Input {
                    title: "New Branch Name".to_string(),
                    input: String::new(),
                    action: InputAction::CreateBranch,
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
                        self.toast(
                            crate::toast::ToastKind::Error,
                            format!("Cannot open diff: {e}"),
                        );
                    }
                } else if self.is_diff_loading() {
                    self.toast(crate::toast::ToastKind::Info, "Loading diff...");
                } else {
                    self.toast(crate::toast::ToastKind::Info, "Diff not available");
                }
            }
            Action::CommitFilterChar(c) => {
                self.commit_filter.push(c);
                self.rebuild_graph()?;
            }
            Action::CommitFilterBackspace => {
                if !self.commit_filter.is_empty() {
                    self.commit_filter.pop();
                    self.rebuild_graph()?;
                } else {
                    self.commit_filter_active = false;
                    self.rebuild_graph()?;
                }
            }
            Action::InputBackspaceWord if self.commit_filter_active => {
                crate::text_editor::pop_word(&mut self.commit_filter);
                self.rebuild_graph()?;
            }
            Action::InputClearLine if self.commit_filter_active => {
                self.commit_filter.clear();
                self.rebuild_graph()?;
            }
            Action::Confirm if self.commit_filter_active => {
                self.commit_filter_active = false;
            }
            Action::Cancel if self.commit_filter_active => {
                self.commit_filter.clear();
                self.commit_filter_active = false;
                self.rebuild_graph()?;
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
            let new_pos = (pos as i32 + delta)
                .clamp(0, self.visible_commit_indices.len() as i32 - 1)
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
                    self.toast(
                        crate::toast::ToastKind::Success,
                        format!("Opening PR #{} in browser", pr.number),
                    );
                }
            }
            None => self.toast(crate::toast::ToastKind::Info, "No open PR for this commit"),
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
        if self.block_if_op_in_progress("checkout") {
            return Ok(());
        }
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
                // A graph label is a raw name; resolve remoteness via the
                // remotes()-aware splitter rather than an "origin/" guess.
                let is_remote = self.split_remote_ref(&branches[0]).is_some();
                self.checkout_branch_by_name(&branches[0], is_remote)?;
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

    /// Check out `branch_name`. `is_remote` is the branch's authoritative
    /// remote/local status (from `BranchInfo::is_remote` where a caller has it,
    /// else resolved via the remotes()-aware [`App::split_remote_ref`]) — it
    /// selects between creating/tracking a local branch off a remote-tracking
    /// ref and a plain local checkout, without string-guessing an "origin/"
    /// prefix.
    pub(crate) fn checkout_branch_by_name(
        &mut self,
        branch_name: &str,
        is_remote: bool,
    ) -> Result<()> {
        if self.block_if_op_in_progress("checkout") {
            return Ok(());
        }
        if is_remote {
            checkout_remote_branch(self.repo.repo(), branch_name)?;
        } else {
            checkout_branch(self.repo.repo(), branch_name)?;
        }
        self.refresh(true)?;
        Ok(())
    }

    /// Whether the graph currently carries a synthetic uncommitted-changes node.
    /// `insert_uncommitted_node` always inserts it at index 0, so this is O(1).
    pub fn has_uncommitted_node(&self) -> bool {
        self.graph_layout
            .nodes
            .first()
            .is_some_and(|n| n.is_uncommitted)
    }

    pub fn node_passes_commit_filter(&self, node: &crate::git::graph::GraphNode) -> bool {
        if self.commit_filter.is_empty() {
            return true;
        }
        if node.is_uncommitted {
            return true;
        }
        // The uncommitted-changes row always shows and its connector is wired to
        // HEAD at build time. Keep HEAD visible even when its own message misses
        // the filter, so that connector always terminates at the star instead of
        // dangling into a filtered-out row (a broken line beneath the star).
        if node.is_head && self.has_uncommitted_node() {
            return true;
        }
        let Some(commit) = &node.commit else {
            return false;
        };
        if self.commit_filter_matches.is_empty() {
            // Keeps the predicate useful to callers that change the query then
            // explicitly ask for a recomputation before the next graph rebuild.
            CommitFilterQuery::parse(&self.commit_filter).matches(
                commit,
                self.commit_changed_paths
                    .get(&commit.oid)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            )
        } else {
            self.commit_filter_matches.contains(&commit.oid)
        }
    }

    /// Retain every direct match plus its loaded parents. The graph builder sees
    /// this closure rather than isolated matches, which keeps merge and parent
    /// edges valid while allowing the renderer to dim the retained ancestors.
    pub(crate) fn filtered_graph_commits(&mut self) -> Vec<crate::git::CommitInfo> {
        let query = CommitFilterQuery::parse(&self.commit_filter);
        if !query.is_active() {
            self.commit_filter_matches.clear();
            return self.commits.clone();
        }
        self.commit_filter_matches = self
            .commits
            .iter()
            .filter(|commit| {
                query.matches(
                    commit,
                    self.commit_changed_paths
                        .get(&commit.oid)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                )
            })
            .map(|commit| commit.oid)
            .collect();
        let mut retained = self.commit_filter_matches.clone();
        let by_oid: std::collections::HashMap<_, _> = self
            .commits
            .iter()
            .map(|commit| (commit.oid, commit))
            .collect();
        let mut pending: Vec<_> = retained.iter().copied().collect();
        while let Some(oid) = pending.pop() {
            if let Some(commit) = by_oid.get(&oid) {
                for parent in &commit.parent_oids {
                    if by_oid.contains_key(parent) && retained.insert(*parent) {
                        pending.push(*parent);
                    }
                }
            }
        }
        // The synthetic working-tree row is connected to HEAD. Preserve that
        // endpoint even when it is not a direct match so the connector cannot
        // terminate in a filtered-out row. Do this after walking direct-match
        // ancestry: including HEAD in `pending` would otherwise retain every
        // loaded ancestor of HEAD and make a dirty-worktree filter ineffective.
        if self.working_tree_status.is_some() {
            if let Some(head) = self.repo.head_oid() {
                retained.insert(head);
            }
        }
        self.commits
            .iter()
            .filter(|commit| retained.contains(&commit.oid))
            .cloned()
            .collect()
    }

    pub fn recompute_visible_commits(&mut self) {
        self.visible_commit_indices =
            if self.commit_filter_matches.is_empty() && !self.commit_filter.is_empty() {
                self.graph_layout
                    .nodes
                    .iter()
                    .enumerate()
                    .filter(|(_, node)| self.node_passes_commit_filter(node))
                    .map(|(idx, _)| idx)
                    .collect()
            } else {
                (0..self.graph_layout.nodes.len()).collect()
            };

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
        self.graph_nav
            .jump_to_head(self.head_name.as_deref(), &self.graph_layout);
    }

    /// Jump the selection to the fork point: the merge base of the selected
    /// commit with the main branch (or, if the selection is on main, with the
    /// current HEAD branch). Answers "where does this meet the other line of
    /// development". No jump — with a status message — for linear history or a
    /// merge base beyond the loaded window.
    fn jump_to_merge_base(&mut self) {
        use crate::merge_base::{fork_target, main_branch_tip, row_of_commit, ForkTarget};

        let Some(selected) = self
            .selected_commit_node()
            .and_then(|n| n.commit.as_ref())
            .map(|c| c.oid)
        else {
            return; // uncommitted / connector row — nothing to diverge from
        };
        let Some(main_tip) = main_branch_tip(&self.graph_layout) else {
            return;
        };
        let head_tip = self.branches.iter().find(|b| b.is_head).map(|b| b.tip_oid);

        // Resolve the target (borrows the repo immutably) before mutating self.
        let target = {
            let repo = self.repo.repo();
            fork_target(selected, main_tip, head_tip, |a, b| {
                repo.merge_base(a, b).ok()
            })
        };

        match target {
            ForkTarget::Jump(oid) => match row_of_commit(&self.graph_layout, oid) {
                Some(idx) => self.select_commit_by_full_idx(idx),
                None => self.toast(
                    crate::toast::ToastKind::Info,
                    "Merge base beyond loaded history",
                ),
            },
            ForkTarget::Linear => self.toast(
                crate::toast::ToastKind::Info,
                "No divergence — linear history",
            ),
            ForkTarget::NoBase => self.toast(crate::toast::ToastKind::Info, "No merge base found"),
        }
    }

    /// Move the selection to the row `lookup` finds relative to the current
    /// selection — the shared plumbing for Ctrl+Up/Ctrl+Down same-lane
    /// navigation (see `same_lane_ancestor_row` / `same_lane_descendant_row`
    /// in `git::graph`). A subtle bound stop: no-op (selection unchanged)
    /// when the lane ends — no error message, matching `move_selection`'s
    /// clamp-at-the-edge behavior.
    fn jump_same_lane(
        &mut self,
        lookup: fn(&crate::git::graph::GraphLayout, usize) -> Option<usize>,
    ) {
        let Some(current) = self.graph_nav.selected_index() else {
            return;
        };
        if let Some(target) = lookup(&self.graph_layout, current) {
            self.select_commit_by_full_idx(target);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use crate::git::GitRepository;
    use crate::test_support::git;

    /// A repo whose HEAD commit message won't match a "feature"-style filter,
    /// with an uncommitted change in the working tree so the graph carries the
    /// synthetic uncommitted node anchored to HEAD.
    fn repo_with_uncommitted_head() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("f.txt"), "base\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "feature work one"]);
        std::fs::write(dir.join("f.txt"), "two\n").unwrap();
        git(dir, &["commit", "-q", "-am", "feature work two"]);
        // HEAD commit's message is deliberately off-theme from the filter below.
        std::fs::write(dir.join("f.txt"), "three\n").unwrap();
        git(dir, &["commit", "-q", "-am", "chore: bump version"]);
        // Dirty the tree so an uncommitted node is inserted.
        std::fs::write(dir.join("f.txt"), "dirty working copy\n").unwrap();
        tmp
    }

    #[test]
    fn filter_keeps_head_row_so_the_uncommitted_connector_never_dangles() {
        let tmp = repo_with_uncommitted_head();
        let repo = GitRepository::open(tmp.path()).expect("open repo");
        let mut app = App::from_repo(repo).expect("build app");

        // Precondition: the graph carries the uncommitted node anchored to HEAD.
        assert!(
            app.has_uncommitted_node(),
            "dirty tree should yield an uncommitted node"
        );
        let head_idx = app
            .graph_layout
            .nodes
            .iter()
            .position(|n| n.is_head)
            .expect("a HEAD commit exists");
        // Sanity: HEAD's own message does NOT contain the filter term, so the
        // plain text predicate would drop it.
        let head_node = &app.graph_layout.nodes[head_idx];
        assert!(
            !head_node
                .commit
                .as_ref()
                .unwrap()
                .message
                .to_lowercase()
                .contains("feature"),
            "test fixture: HEAD must not match the filter on its own"
        );

        // Apply a filter that matches the older commits but not HEAD.
        app.commit_filter = "feature".to_string();
        app.recompute_visible_commits();

        // The HEAD row is kept visible so its uncommitted connector terminates
        // at the star instead of dangling into a hidden row.
        assert!(
            app.visible_commit_indices.contains(&head_idx),
            "HEAD row must stay visible while an uncommitted connector anchors to it"
        );
    }

    #[test]
    fn head_is_still_filtered_out_when_there_are_no_uncommitted_changes() {
        // Without an uncommitted node there is no connector to orphan, so the
        // HEAD-keep exception must not fire — the filter behaves normally.
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("f.txt"), "base\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "feature work one"]);
        std::fs::write(dir.join("f.txt"), "two\n").unwrap();
        git(dir, &["commit", "-q", "-am", "chore: unrelated head"]);
        // Clean tree: no uncommitted node.

        let repo = GitRepository::open(dir).expect("open repo");
        let mut app = App::from_repo(repo).expect("build app");
        assert!(
            !app.has_uncommitted_node(),
            "clean tree: no uncommitted node"
        );

        let head_idx = app
            .graph_layout
            .nodes
            .iter()
            .position(|n| n.is_head)
            .expect("a HEAD commit exists");

        app.commit_filter = "feature".to_string();
        app.recompute_visible_commits();

        assert!(
            !app.visible_commit_indices.contains(&head_idx),
            "with no uncommitted node, a non-matching HEAD is filtered out normally"
        );
    }
}
