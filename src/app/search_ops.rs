//! Branch fuzzy-search state and navigation.

use super::*;

impl App {
    /// Update fuzzy search results for the given query
    pub(crate) fn update_fuzzy_search(&mut self, query: &str) {
        self.search_state.fuzzy_matches = fuzzy_search_branches(query, &self.graph_nav.branch_positions);
        self.search_state.clamp_selection();
    }

    /// Jump to the currently selected search result
    pub(crate) fn jump_to_search_result(&mut self) {
        let Some(result) = self.search_state.selected_result() else {
            return;
        };
        let branch_idx = result.branch_idx;
        let Some((node_idx, _)) = self.graph_nav.branch_positions.get(branch_idx) else {
            return;
        };

        self.graph_nav.selected_branch_position = Some(branch_idx);
        self.graph_nav.graph_list_state.select(Some(*node_idx));
    }

    /// Save current position before starting search
    pub(crate) fn save_search_position(&mut self) {
        self.search_state.original_position = self.graph_nav.selected_branch_position;
        self.search_state.original_node = self.graph_nav.graph_list_state.selected();
    }

    /// Restore position saved before search (for cancel)
    pub(crate) fn restore_search_position(&mut self) {
        self.graph_nav.selected_branch_position = self.search_state.original_position;
        if let Some(node) = self.search_state.original_node {
            self.graph_nav.graph_list_state.select(Some(node));
        }
    }

    /// Get current search results for UI rendering
    pub fn search_results(&self) -> &[FuzzySearchResult] {
        &self.search_state.fuzzy_matches
    }

    /// Get current dropdown selection index
    pub fn search_selection(&self) -> Option<usize> {
        self.search_state.dropdown_selection
    }

    /// Check if currently in search input mode
    pub fn is_in_search_mode(&self) -> bool {
        matches!(
            &self.mode,
            AppMode::Input {
                action: InputAction::Search,
                ..
            }
        )
    }
}
