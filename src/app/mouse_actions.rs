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
                } else if !matches!(
                    self.mode,
                    AppMode::PrCompose { .. } | AppMode::IssueCompose { .. }
                ) {
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
                // A graph chip carries a raw label; resolve remoteness via the
                // remotes()-aware splitter rather than an "origin/" guess.
                let is_remote = self.split_remote_ref(&name).is_some();
                self.mode = AppMode::Confirm {
                    message: format!("Checkout branch '{name}'?"),
                    action: ConfirmAction::Checkout { name, is_remote },
                };
            }
        }
        true
    }

    /// Right-click on a graph row: select that commit and open its context menu
    /// anchored at the cursor (clamped on-screen by the renderer).
    ///
    /// If the commit menu is already open, this re-targets it instead of being
    /// swallowed: a right-click on a *different* commit closes the current menu
    /// and immediately opens the new one (standard GUI context-menu behavior).
    /// A right-click on the same commit, or off any commit row, just closes it
    /// — matching the left-click-outside dismissal.
    fn handle_mouse_right_click(&mut self, col: u16, row: u16) {
        let menu_open = matches!(self.mode, AppMode::CommitMenu { .. });
        if !menu_open && !matches!(self.mode, AppMode::Normal) {
            return;
        }

        let target = self.graph_row_at(col, row);

        if menu_open {
            // Close first: a re-target starts from a clean slate rather than
            // mutating the open menu in place.
            self.mode = AppMode::Normal;
            let same_commit = target.is_some()
                && target == self.graph_nav.graph_list_state.selected();
            if target.is_none() || same_commit {
                return;
            }
        }

        let Some(full_idx) = target else {
            return;
        };
        self.focused_panel = FocusedPanel::Graph;
        self.select_commit_by_full_idx(full_idx);
        self.open_commit_menu();
        // `open_commit_menu` clears the anchor (keyboard opens centered); set it
        // now so this menu renders at the click position. Only if a menu opened
        // (uncommitted/stash-less nodes may not produce one).
        if matches!(self.mode, AppMode::CommitMenu { .. }) {
            self.menu_anchor = Some((col, row));
        }
    }

    /// The graph node index a screen point maps to, if it lands on a commit
    /// row in the graph panel. Layout math only — independent of `self.mode`,
    /// so it can be reused to hit-test a click against the commit rows behind
    /// an open popup.
    fn graph_row_at(&self, col: u16, row: u16) -> Option<usize> {
        let ml = self.mouse_layout;
        if !point_in(ml.graph, col, row) {
            return None;
        }
        let inner = inner_rect(ml.graph);
        let offset = self.graph_nav.graph_list_state.offset();
        let list_idx = list_row_index(inner, offset, col, row)?;
        self.graph_row_full_idx(list_idx)
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
                | AppMode::Settings { .. }
                | AppMode::PullDivergence { .. }
                | AppMode::PrMergePicker { .. }
                | AppMode::PrReviewPicker { .. }
                | AppMode::BranchFilter { .. }
                | AppMode::BranchPicker { .. }
                | AppMode::BranchDeletePicker { .. }
                | AppMode::TagPicker { .. }
                | AppMode::RemotePicker { .. }
                | AppMode::FileHistory { .. }
                | AppMode::IssueList
                | AppMode::IssueDetail
                | AppMode::IssueLabelPicker { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{App, AppMode, MouseLayout};
    use crate::git::GitRepository;
    use ratatui::layout::Rect;
    use std::process::Command;

    fn git(dir: &std::path::Path, args: &[&str]) {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .expect("run git");
    }

    /// A clean (no uncommitted changes) linear repo with three commits, so the
    /// graph has exactly three commit rows and no connector/uncommitted rows —
    /// row index in the graph panel maps 1:1 to `graph_layout.nodes` index.
    fn app_with_three_commits() -> App {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("f.txt"), "one\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("f.txt"), "two\n").unwrap();
        git(dir, &["commit", "-q", "-am", "second"]);
        std::fs::write(dir.join("f.txt"), "three\n").unwrap();
        git(dir, &["commit", "-q", "-am", "third"]);

        let repo = GitRepository::open(dir).expect("open repo");
        let mut app = App::from_repo(repo).expect("build app");
        assert_eq!(app.graph_layout.nodes.len(), 3, "three commits, no folding");

        // A graph panel tall enough to show all three rows starting at row 1
        // (row 0 is the border): row 1 -> list_idx 0, row 2 -> list_idx 1, ...
        app.mouse_layout = MouseLayout {
            graph: Rect::new(0, 0, 40, 10),
            files: Rect::default(),
            commit: Rect::default(),
            main: Rect::default(),
            side_layout: false,
        };
        app
    }

    /// Right-clicking a commit row when no menu is open selects that commit
    /// and opens the menu for it.
    #[test]
    fn right_click_opens_menu_for_clicked_commit() {
        let mut app = app_with_three_commits();
        app.handle_mouse_action(crate::action::Action::MouseRightClick { col: 5, row: 1 });
        assert!(matches!(app.mode, AppMode::CommitMenu { .. }));
        assert_eq!(app.graph_nav.graph_list_state.selected(), Some(0));
    }

    /// The bug this fixes: right-clicking a *different* commit row while the
    /// menu is already open must close the old menu and reopen it for the new
    /// commit in one action, not be swallowed.
    #[test]
    fn right_click_on_different_commit_retargets_open_menu() {
        let mut app = app_with_three_commits();
        app.handle_mouse_action(crate::action::Action::MouseRightClick { col: 5, row: 1 });
        assert!(matches!(app.mode, AppMode::CommitMenu { .. }));
        assert_eq!(app.graph_nav.graph_list_state.selected(), Some(0));

        app.handle_mouse_action(crate::action::Action::MouseRightClick { col: 5, row: 2 });

        assert!(
            matches!(app.mode, AppMode::CommitMenu { .. }),
            "menu should reopen for the new commit, not just close"
        );
        assert_eq!(
            app.graph_nav.graph_list_state.selected(),
            Some(1),
            "selection should follow the new right-click target"
        );
        assert_eq!(
            app.menu_anchor,
            Some((5, 2)),
            "menu should re-anchor at the new click position"
        );
    }

    /// Right-clicking the *same* commit row the menu is already open for just
    /// closes it — mirroring left-click-outside dismissal.
    #[test]
    fn right_click_on_same_commit_closes_menu() {
        let mut app = app_with_three_commits();
        app.handle_mouse_action(crate::action::Action::MouseRightClick { col: 5, row: 1 });
        assert!(matches!(app.mode, AppMode::CommitMenu { .. }));

        app.handle_mouse_action(crate::action::Action::MouseRightClick { col: 5, row: 1 });

        assert!(matches!(app.mode, AppMode::Normal));
    }

    /// Right-clicking off the commit rows entirely (outside the graph panel)
    /// while the menu is open closes it instead of doing nothing.
    #[test]
    fn right_click_off_graph_closes_open_menu() {
        let mut app = app_with_three_commits();
        app.handle_mouse_action(crate::action::Action::MouseRightClick { col: 5, row: 1 });
        assert!(matches!(app.mode, AppMode::CommitMenu { .. }));

        app.handle_mouse_action(crate::action::Action::MouseRightClick { col: 5, row: 50 });

        assert!(matches!(app.mode, AppMode::Normal));
    }
}
