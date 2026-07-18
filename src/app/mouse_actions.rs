//! Mouse input handling: hit-testing clicks/scroll against the recorded panel
//! layout, mapping rows to commits/files, and double-click behavior.

use std::time::Instant;

use ratatui::layout::Rect;

use super::*;
use crate::mouse::{
    chip_at, divider_ratio, is_double_click, list_row_index, on_divider, point_in, ChipTarget,
    LastClick, DOUBLE_CLICK_WINDOW,
};

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
            Action::MouseRightClick { col, row } => self.handle_mouse_right_click(col, row),
            Action::MouseScroll { col, row, down } => self.handle_mouse_scroll(col, row, down),
            Action::MouseDrag { col, row } => self.handle_mouse_drag(col, row),
            Action::MouseUp { .. } => self.handle_mouse_up(),
            _ => {}
        }
    }

    /// Drag in progress: if the divider grab is active, resize the split live.
    fn handle_mouse_drag(&mut self, col: u16, row: u16) {
        if !self.dragging_divider {
            return;
        }
        let ml = self.mouse_layout;
        self.graph_split_ratio = divider_ratio(ml.main, ml.side_layout, col, row);
    }

    /// Button released: end a divider drag and persist the new ratio.
    fn handle_mouse_up(&mut self) {
        if self.dragging_divider {
            self.dragging_divider = false;
            self.save_ui_state();
        }
    }

    fn handle_mouse_click(&mut self, col: u16, row: u16) {
        let now = Instant::now();
        let double = is_double_click(self.last_click, col, row, now, DOUBLE_CLICK_WINDOW);
        self.last_click = Some(LastClick { col, row, at: now });

        // Status-bar key hints are clickable in every mode: a hit dispatches the
        // same Action pressing the key would. Checked before popup routing so a
        // hint click isn't swallowed as a click-outside dismissal.
        if let Some(action) = crate::mouse::region_at(&self.status_hints, col, row).cloned() {
            let _ = self.handle_action(action);
            return;
        }

        // A popup is open: route the click to it rather than the panels behind.
        if !matches!(self.mode, AppMode::Normal) {
            if let Some(rect) = self.popup_rect {
                if point_in(rect, col, row) {
                    self.handle_popup_click(rect, col, row);
                } else if !matches!(self.mode, AppMode::PrCompose { .. }) {
                    // A click outside the popup dismisses it — and is swallowed
                    // so it does not also act on the panel underneath. The
                    // compose editor is excluded so a stray click can't discard
                    // in-progress text.
                    let _ = self.handle_action(Action::Cancel);
                }
            }
            return;
        }

        let ml = self.mouse_layout;
        // Grabbing the divider between graph and detail starts a resize drag
        // instead of selecting a row.
        if on_divider(ml.graph, ml.side_layout, col, row) {
            self.dragging_divider = true;
            return;
        }
        if point_in(ml.graph, col, row) {
            self.focused_panel = FocusedPanel::Graph;
            let inner = inner_rect(ml.graph);
            let offset = self.graph_nav.graph_list_state.offset();
            if let Some(list_idx) = list_row_index(inner, offset, col, row) {
                if let Some(full_idx) = self.graph_row_full_idx(list_idx) {
                    self.select_commit_by_full_idx(full_idx);
                    // A click landing on a chip (PR badge / branch label) acts on
                    // that chip instead of the row's open/double-click behavior.
                    let line_col = col.saturating_sub(inner.x);
                    if self.handle_graph_chip_click(list_idx, line_col) {
                        return;
                    }
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

    /// Act on a chip (PR badge / branch label) clicked on graph row `list_idx`
    /// at line column `line_col`. Returns whether a chip handled the click.
    fn handle_graph_chip_click(&mut self, list_idx: usize, line_col: u16) -> bool {
        let Some(target) = self
            .graph_chip_hits
            .get(list_idx)
            .and_then(|chips| chip_at(chips, line_col))
            .map(|c| c.target.clone())
        else {
            return false;
        };
        match target {
            ChipTarget::PrBadge => self.open_selected_pr(),
            ChipTarget::Branch(name) => {
                self.mode = AppMode::Confirm {
                    message: format!("Checkout branch '{name}'?"),
                    action: ConfirmAction::Checkout(name),
                };
            }
        }
        true
    }

    /// Right-click on a graph row: select that commit and open its context menu
    /// anchored at the cursor (clamped on-screen by the renderer).
    fn handle_mouse_right_click(&mut self, col: u16, row: u16) {
        if !matches!(self.mode, AppMode::Normal) {
            return;
        }
        let ml = self.mouse_layout;
        if !point_in(ml.graph, col, row) {
            return;
        }
        self.focused_panel = FocusedPanel::Graph;
        let inner = inner_rect(ml.graph);
        let offset = self.graph_nav.graph_list_state.offset();
        let Some(list_idx) = list_row_index(inner, offset, col, row) else {
            return;
        };
        let Some(full_idx) = self.graph_row_full_idx(list_idx) else {
            return;
        };
        self.select_commit_by_full_idx(full_idx);
        self.open_commit_menu();
        // `open_commit_menu` clears the anchor (keyboard opens centered); set it
        // now so this menu renders at the click position. Only if a menu opened
        // (uncommitted/stash-less nodes may not produce one).
        if matches!(self.mode, AppMode::CommitMenu { .. }) {
            self.menu_anchor = Some((col, row));
        }
    }

    /// A click landed inside the open popup's rect: if it hit a selectable row,
    /// select it and activate (Enter-equivalent).
    fn handle_popup_click(&mut self, rect: Rect, col: u16, row: u16) {
        let inner = inner_rect(rect);
        let Some(idx) = list_row_index(inner, 0, col, row) else {
            return;
        };
        // Only some popups expose click-selectable list rows; others (editors,
        // scroll views) swallow the click without acting.
        let Some(count) = self.popup_row_count() else {
            return;
        };
        if idx >= count {
            return;
        }
        if self.set_popup_selected(idx) {
            let _ = self.handle_action(Action::MenuSelect);
        }
    }

    /// Number of click-selectable rows in the current popup, or `None` when the
    /// popup has no clickable list.
    fn popup_row_count(&self) -> Option<usize> {
        match &self.mode {
            AppMode::CommitMenu { items, filter, .. } => {
                Some(self.commit_menu_visible_count(items, filter))
            }
            AppMode::BranchPicker { branches, .. }
            | AppMode::BranchDeletePicker { branches, .. } => Some(branches.len()),
            AppMode::TagPicker { tags, .. } => Some(tags.len()),
            AppMode::RemotePicker { remotes, .. } => Some(remotes.len()),
            AppMode::PrMergePicker { .. } => Some(crate::pr_action::MergeMethod::ALL.len()),
            AppMode::PrReviewPicker { .. } => Some(crate::pr_action::ReviewDecision::ALL.len()),
            _ => None,
        }
    }

    /// Set the `selected` index on the current list popup. Returns whether the
    /// mode had a selectable list.
    fn set_popup_selected(&mut self, idx: usize) -> bool {
        match &mut self.mode {
            AppMode::CommitMenu { selected, .. }
            | AppMode::BranchPicker { selected, .. }
            | AppMode::BranchDeletePicker { selected, .. }
            | AppMode::TagPicker { selected, .. }
            | AppMode::RemotePicker { selected, .. }
            | AppMode::PrMergePicker { selected, .. }
            | AppMode::PrReviewPicker { selected, .. } => {
                *selected = idx;
                true
            }
            _ => false,
        }
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
