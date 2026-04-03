//! Graph navigation state: selection, branch traversal, connector skipping.

use ratatui::widgets::ListState;

use crate::git::graph::{GraphLayout, GraphNode};
use crate::git::BranchInfo;

/// Filter branch names to exclude remote branches that have matching local branches.
/// Returns branches in order: local branches first, then remote-only branches.
fn filter_remote_duplicates(branch_names: &[String]) -> Vec<&str> {
    use std::collections::HashSet;

    let local_branches: HashSet<&str> = branch_names
        .iter()
        .filter(|n| !n.starts_with("origin/"))
        .map(|s| s.as_str())
        .collect();

    branch_names
        .iter()
        .filter(|name| {
            if let Some(local_name) = name.strip_prefix("origin/") {
                !local_branches.contains(local_name)
            } else {
                true
            }
        })
        .map(|s| s.as_str())
        .collect()
}

/// Graph navigation state: tracks selection position and branch focus.
pub struct GraphNav {
    pub graph_list_state: ListState,
    /// List of (node_index, branch_name) for all branches
    pub branch_positions: Vec<(usize, String)>,
    /// Currently selected branch position index
    pub selected_branch_position: Option<usize>,
}

impl Default for GraphNav {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphNav {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            graph_list_state: state,
            branch_positions: Vec::new(),
            selected_branch_position: None,
        }
    }

    /// Build a flat list of (node_index, branch_name) for all branches.
    pub fn build_branch_positions(graph_layout: &GraphLayout) -> Vec<(usize, String)> {
        graph_layout
            .nodes
            .iter()
            .enumerate()
            .flat_map(|(node_idx, node)| {
                filter_remote_duplicates(&node.branch_names)
                    .into_iter()
                    .map(move |name| (node_idx, name.to_string()))
            })
            .collect()
    }

    /// Rebuild branch positions from the current graph layout.
    pub fn rebuild_branch_positions(&mut self, graph_layout: &GraphLayout) {
        self.branch_positions = Self::build_branch_positions(graph_layout);
    }

    /// Get the currently selected node index.
    pub fn selected_index(&self) -> Option<usize> {
        self.graph_list_state.selected()
    }

    /// Get the selected graph node.
    pub fn selected_node<'a>(&self, layout: &'a GraphLayout) -> Option<&'a GraphNode> {
        self.graph_list_state
            .selected()
            .and_then(|i| layout.nodes.get(i))
    }

    /// Check if the selected node is the uncommitted changes node.
    pub fn is_uncommitted_selected(&self, layout: &GraphLayout) -> bool {
        self.selected_node(layout)
            .is_some_and(|node| node.is_uncommitted)
    }

    /// Check if the selected node is the HEAD commit (not uncommitted).
    pub fn is_head_commit_selected(&self, layout: &GraphLayout) -> bool {
        self.selected_node(layout)
            .is_some_and(|node| node.is_head && !node.is_uncommitted)
    }

    /// Check if a node is a connector-only row (not a commit, not uncommitted).
    fn is_connector_node(&self, layout: &GraphLayout, idx: usize) -> bool {
        layout
            .nodes
            .get(idx)
            .is_some_and(|n| n.commit.is_none() && !n.is_uncommitted)
    }

    /// Move selection by delta, skipping connector rows.
    pub fn move_selection(&mut self, layout: &GraphLayout, delta: i32) {
        let max = layout.nodes.len().saturating_sub(1);
        let current = self.graph_list_state.selected().unwrap_or(0);
        let mut new = (current as i32 + delta).clamp(0, max as i32) as usize;
        let step: i32 = if delta > 0 { 1 } else { -1 };
        while self.is_connector_node(layout, new) && new > 0 && new < max {
            new = (new as i32 + step).clamp(0, max as i32) as usize;
        }
        self.graph_list_state.select(Some(new));
        self.sync_branch_selection_to_node(new);
    }

    /// Select the first non-connector node.
    pub fn select_first(&mut self, layout: &GraphLayout) {
        let mut idx = 0;
        let max = layout.nodes.len().saturating_sub(1);
        while self.is_connector_node(layout, idx) && idx < max {
            idx += 1;
        }
        self.graph_list_state.select(Some(idx));
        self.sync_branch_selection_to_node(idx);
    }

    /// Select the last non-connector node.
    pub fn select_last(&mut self, layout: &GraphLayout) {
        let mut idx = layout.nodes.len().saturating_sub(1);
        while self.is_connector_node(layout, idx) && idx > 0 {
            idx -= 1;
        }
        self.graph_list_state.select(Some(idx));
        self.sync_branch_selection_to_node(idx);
    }

    /// Sync branch selection to the first branch of the given node.
    pub fn sync_branch_selection_to_node(&mut self, node_idx: usize) {
        self.selected_branch_position = self
            .branch_positions
            .iter()
            .position(|(idx, _)| *idx == node_idx);
    }

    /// Move to the next branch (across all commits).
    pub fn move_to_next_branch(&mut self) {
        if self.branch_positions.is_empty() {
            return;
        }
        let next = match self.selected_branch_position {
            Some(pos) => {
                if pos + 1 < self.branch_positions.len() {
                    pos + 1
                } else {
                    return;
                }
            }
            None => 0,
        };
        self.selected_branch_position = Some(next);
        if let Some((node_idx, _)) = self.branch_positions.get(next) {
            self.graph_list_state.select(Some(*node_idx));
        }
    }

    /// Move to the previous branch (across all commits).
    pub fn move_to_prev_branch(&mut self) {
        if self.branch_positions.is_empty() {
            return;
        }
        let prev = match self.selected_branch_position {
            Some(pos) => {
                if pos > 0 {
                    pos - 1
                } else {
                    return;
                }
            }
            None => self.branch_positions.len() - 1,
        };
        self.selected_branch_position = Some(prev);
        if let Some((node_idx, _)) = self.branch_positions.get(prev) {
            self.graph_list_state.select(Some(*node_idx));
        }
    }

    /// Move to an adjacent branch within the same commit node.
    pub fn move_branch_within_node(&mut self, delta: isize) {
        let Some(pos) = self.selected_branch_position else {
            return;
        };
        let new_pos = (pos as isize + delta) as usize;
        if new_pos >= self.branch_positions.len() {
            return;
        }
        let Some((current_node, _)) = self.branch_positions.get(pos) else {
            return;
        };
        let Some((target_node, _)) = self.branch_positions.get(new_pos) else {
            return;
        };
        if current_node == target_node {
            self.selected_branch_position = Some(new_pos);
        }
    }

    pub fn move_branch_left(&mut self) {
        self.move_branch_within_node(-1);
    }

    pub fn move_branch_right(&mut self) {
        self.move_branch_within_node(1);
    }

    /// Jump to the HEAD branch by name.
    pub fn jump_to_head(&mut self, head_name: Option<&str>) {
        let Some(head_name) = head_name else {
            return;
        };
        let Some((branch_pos_idx, (node_idx, _))) = self
            .branch_positions
            .iter()
            .enumerate()
            .find(|(_, (_, name))| name == head_name)
        else {
            return;
        };
        self.selected_branch_position = Some(branch_pos_idx);
        self.graph_list_state.select(Some(*node_idx));
    }

    /// Get the currently selected branch from the branches list.
    pub fn selected_branch<'a>(&self, branches: &'a [BranchInfo]) -> Option<&'a BranchInfo> {
        let (_, branch_name) = self
            .selected_branch_position
            .and_then(|pos| self.branch_positions.get(pos))?;
        branches.iter().find(|b| &b.name == branch_name)
    }

    /// Get the name of the currently selected branch.
    pub fn selected_branch_name(&self) -> Option<&str> {
        self.selected_branch_position
            .and_then(|pos| self.branch_positions.get(pos))
            .map(|(_, name)| name.as_str())
    }

    /// Returns all branch names for the currently selected node.
    pub fn selected_node_branches(&self) -> Vec<&str> {
        let Some(node_idx) = self.graph_list_state.selected() else {
            return vec![];
        };
        self.branch_positions
            .iter()
            .filter(|(idx, _)| *idx == node_idx)
            .map(|(_, name)| name.as_str())
            .collect()
    }
}
