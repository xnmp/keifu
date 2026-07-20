//! Graph navigation state: selection, branch traversal, connector skipping.

use ratatui::widgets::ListState;

use crate::git::graph::{GraphLayout, GraphNode};
use crate::git::BranchInfo;

/// Filter branch names to exclude remote branches that have matching local branches.
/// Returns branches in order: local branches first, then remote-only branches.
///
/// `remotes` is the repo's configured remote list; pairing a remote ref with
/// its local twin goes through the shared [`crate::git::split_remote_ref`], so a
/// non-`origin` remote (e.g. `upstream/foo` ↔ `foo`) dedups correctly.
fn filter_remote_duplicates<'a>(branch_names: &'a [String], remotes: &[String]) -> Vec<&'a str> {
    use std::collections::HashSet;

    let local_branches: HashSet<&str> = branch_names
        .iter()
        .filter(|n| crate::git::split_remote_ref(remotes, n).is_none())
        .map(|s| s.as_str())
        .collect();

    branch_names
        .iter()
        .filter(|name| match crate::git::split_remote_ref(remotes, name) {
            Some((_remote, local_name)) => !local_branches.contains(local_name.as_str()),
            None => true,
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
    /// `remotes` is the repo's configured remote list, used to pair remote refs
    /// with their local twins for dedup.
    pub fn build_branch_positions(
        graph_layout: &GraphLayout,
        remotes: &[String],
    ) -> Vec<(usize, String)> {
        graph_layout
            .nodes
            .iter()
            .enumerate()
            .flat_map(|(node_idx, node)| {
                filter_remote_duplicates(&node.branch_names, remotes)
                    .into_iter()
                    .map(move |name| (node_idx, name.to_string()))
            })
            .collect()
    }

    /// Rebuild branch positions from the current graph layout.
    pub fn rebuild_branch_positions(&mut self, graph_layout: &GraphLayout, remotes: &[String]) {
        self.branch_positions = Self::build_branch_positions(graph_layout, remotes);
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

#[cfg(test)]
mod tests {
    use super::filter_remote_duplicates;

    fn strings(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pairs_non_origin_remote_with_its_local_twin() {
        // `upstream/foo` is the remote twin of local `foo` -> deduped away;
        // `upstream/lonely` has no local twin -> kept.
        let names = strings(&["foo", "upstream/foo", "upstream/lonely"]);
        let remotes = strings(&["upstream"]);
        assert_eq!(
            filter_remote_duplicates(&names, &remotes),
            vec!["foo", "upstream/lonely"]
        );
    }

    #[test]
    fn origin_dedup_still_works() {
        let names = strings(&["main", "origin/main", "origin/solo"]);
        let remotes = strings(&["origin"]);
        assert_eq!(
            filter_remote_duplicates(&names, &remotes),
            vec!["main", "origin/solo"]
        );
    }

    #[test]
    fn local_branch_with_slash_is_not_treated_as_remote() {
        // No remote named `feature`, so `feature/x` is a local branch kept as-is.
        let names = strings(&["feature/x", "origin/main"]);
        let remotes = strings(&["origin"]);
        assert_eq!(
            filter_remote_duplicates(&names, &remotes),
            vec!["feature/x", "origin/main"]
        );
    }
}
