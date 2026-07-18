//! Mouse input handling: hit-testing clicks/scroll against the recorded panel
//! layout, mapping rows to commits/files, and double-click behavior.

use std::time::Instant;

use ratatui::layout::Rect;

use super::*;
use crate::mouse::{is_double_click, list_row_index, point_in, LastClick, DOUBLE_CLICK_WINDOW};

/// The inner (border-inset) area of a panel, where its list rows render.
fn inner_rect(r: Rect) -> Rect {
    Rect {
        x: r.x.saturating_add(1),
        y: r.y.saturating_add(1),
        width: r.width.saturating_sub(2),
        height: r.height.saturating_sub(2),
    }
}

impl App {
    pub(crate) fn handle_mouse_action(&mut self, action: Action) {
        match action {
            Action::MouseClick { col, row } => self.handle_mouse_click(col, row),
            Action::MouseScroll { col, row, down } => self.handle_mouse_scroll(col, row, down),
            // Right-click, drag, and up are handled by later capabilities.
            _ => {}
        }
    }

    fn handle_mouse_click(&mut self, col: u16, row: u16) {
        let now = Instant::now();
        let double = is_double_click(self.last_click, col, row, now, DOUBLE_CLICK_WINDOW);
        self.last_click = Some(LastClick { col, row, at: now });

        // Popup/overlay click handling is layered on by later capabilities; for
        // now clicks only act in the main (Normal) layout.
        if !matches!(self.mode, AppMode::Normal) {
            return;
        }

        let ml = self.mouse_layout;
        if point_in(ml.graph, col, row) {
            self.focused_panel = FocusedPanel::Graph;
            let inner = inner_rect(ml.graph);
            let offset = self.graph_nav.graph_list_state.offset();
            if let Some(list_idx) = list_row_index(inner, offset, col, row) {
                if let Some(full_idx) = self.graph_row_full_idx(list_idx) {
                    self.select_commit_by_full_idx(full_idx);
                    if double {
                        self.open_commit_menu();
                    }
                }
            }
        } else if point_in(ml.files, col, row) {
            self.editing_commit_message = false;
            self.focused_panel = FocusedPanel::Files;
            self.sync_file_list_cache();
            let inner = inner_rect(ml.files);
            if let Some(list_idx) = list_row_index(inner, self.files_view_offset, col, row) {
                if list_idx < self.files_pane.display_items().len() {
                    self.files_pane.select_file_at(list_idx);
                    if double {
                        let _ = self.handle_files_action(Action::OpenFileDiff);
                    }
                }
            }
        } else if point_in(ml.commit, col, row) {
            self.editing_commit_message = false;
            self.focused_panel = FocusedPanel::CommitDetail;
        }
    }

    /// Map a graph list row (folded/filtered index space) to a node index into
    /// `graph_layout.nodes`, matching what the widget rendered.
    pub(crate) fn graph_row_full_idx(&self, list_idx: usize) -> Option<usize> {
        let fold = self.pixel_graph.is_some();
        crate::ui::graph_view::visible_rows(self, fold)
            .get(list_idx)
            .map(|r| r.full_idx)
    }

    /// Select the commit at `full_idx` (a node index), as clicking a row does.
    pub(crate) fn select_commit_by_full_idx(&mut self, full_idx: usize) {
        if full_idx >= self.graph_layout.nodes.len() {
            return;
        }
        self.graph_nav.graph_list_state.select(Some(full_idx));
        self.graph_nav.sync_branch_selection_to_node(full_idx);
        self.commit_detail_scroll = 0;
    }

    fn handle_mouse_scroll(&mut self, col: u16, row: u16, down: bool) {
        // Full-screen diff: scroll a few lines at a time.
        if matches!(self.mode, AppMode::FileDiff { .. }) {
            let a = if down {
                Action::ScrollDown
            } else {
                Action::ScrollUp
            };
            for _ in 0..3 {
                let _ = self.handle_action(a.clone());
            }
            return;
        }

        // Scrollable popups: route the wheel to their up/down.
        if self.mode_scrolls_with_wheel() {
            let a = if down { Action::MoveDown } else { Action::MoveUp };
            let _ = self.handle_action(a);
            return;
        }

        if !matches!(self.mode, AppMode::Normal) {
            return;
        }

        // Focus and scroll the panel under the cursor.
        let ml = self.mouse_layout;
        let panel = if point_in(ml.graph, col, row) {
            Some(FocusedPanel::Graph)
        } else if point_in(ml.files, col, row) {
            Some(FocusedPanel::Files)
        } else if point_in(ml.commit, col, row) {
            Some(FocusedPanel::CommitDetail)
        } else {
            None
        };
        if let Some(panel) = panel {
            self.focused_panel = panel;
            let a = if down { Action::MoveDown } else { Action::MoveUp };
            let _ = self.handle_normal_action(a);
        }
    }

    /// Whether the current popup mode consumes a scroll wheel as up/down.
    fn mode_scrolls_with_wheel(&self) -> bool {
        matches!(
            self.mode,
            AppMode::CiChecks
                | AppMode::PrThread
                | AppMode::CommitMenu { .. }
                | AppMode::MetadataMenu { .. }
                | AppMode::PullDivergence { .. }
                | AppMode::PrMergePicker { .. }
                | AppMode::PrReviewPicker { .. }
                | AppMode::BranchFilter { .. }
                | AppMode::BranchPicker { .. }
                | AppMode::BranchDeletePicker { .. }
                | AppMode::TagPicker { .. }
                | AppMode::RemotePicker { .. }
                | AppMode::FileHistory { .. }
        )
    }
}
