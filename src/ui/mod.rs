//! UI components

pub mod branch_filter;
pub mod command_palette;
pub mod commit_detail;
pub mod ci_checks;
pub mod commit_menu;
pub mod dialog;
pub mod file_diff_view;
pub mod file_icons;
pub mod files_pane;
pub mod graph_pixels;
pub mod graph_view;
pub mod help_popup;
pub mod issue_compose;
pub mod issue_detail;
pub mod issue_list;
pub mod metadata_menu;
pub mod pr_compose;
pub mod pr_thread;
pub mod search_dropdown;
pub mod settings_menu;
pub mod status_bar;
pub mod theme;

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Widget,
        Wrap,
    },
    Frame,
};

use crate::app::{App, AppMode, InputAction};

use self::{
    branch_filter::BranchFilterWidget,
    commit_detail::{compute_commit_detail_layout, CommitDetailWidget},
    commit_menu::CommitMenuWidget,
    dialog::{
        BranchPickerWidget, ConfirmDialog, FileHistoryWidget, InputDialog, OptionsDialog,
        PullDivergenceDialog,
    },
    file_diff_view::FileDiffViewWidget,
    files_pane::{FilesPaneState, FilesPaneWidget},
    graph_view::GraphViewWidget,
    help_popup::HelpPopup,
    search_dropdown::{calculate_dropdown_height, SearchDropdown},
    status_bar::StatusBar,
    theme::Theme,
};

/// Minimum terminal width required for rendering
const MIN_WIDTH: u16 = 20;
/// Minimum terminal height required for rendering
const MIN_HEIGHT: u16 = 6;

/// Minimum widget dimensions for safe rendering
pub const MIN_WIDGET_WIDTH: u16 = 12;
pub const MIN_WIDGET_HEIGHT: u16 = 3;

/// PR-thread popup size (% of screen). Shared by the scroll pre-pass and the
/// render so their geometry matches.
const PR_THREAD_POPUP_PCT: (u16, u16) = (80, 80);

/// Whether the current mode is one of the full-screen issue views (list/detail)
/// or an overlay drawn on top of them (compose/label picker/label filter).
fn is_issue_mode(mode: &AppMode) -> bool {
    matches!(
        mode,
        AppMode::IssueList
            | AppMode::IssueDetail
            | AppMode::IssueCompose { .. }
            | AppMode::IssueLabelPicker { .. }
            | AppMode::IssueLabelFilter { .. }
    )
}

/// Whether the issue detail (vs the list) is the full-screen backdrop for the
/// current issue mode. The label picker's backdrop follows where it was opened
/// from — detail when a detail popup is live, otherwise the list.
fn issue_bg_is_detail(app: &App) -> bool {
    use crate::app::IssueComposePurpose;
    match &app.mode {
        AppMode::IssueDetail => true,
        AppMode::IssueCompose {
            purpose: IssueComposePurpose::Comment { .. },
        } => true,
        AppMode::IssueLabelPicker { .. } => app.issue_detail.is_some(),
        _ => false,
    }
}

/// Render a placeholder block when widget area is too small
pub fn render_placeholder_block(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.unfocused_border_style());
    block.render(area, buf);
}

/// Whether a scrollbar is worth drawing: content overflows the viewport and the
/// pane is tall enough to host a track between its rounded corners. Pure so the
/// overflow decision is unit-testable.
fn scrollbar_needed(content_length: usize, viewport_length: usize, area_height: u16) -> bool {
    content_length > viewport_length && area_height > 2
}

/// Draw a vertical scrollbar on the right border of a bordered pane, reflecting
/// `position` within `content_length` given a `viewport_length`-tall viewport.
///
/// The bar occupies the rightmost column, inset one row top and bottom so it
/// sits between the pane's rounded corners. In pixel mode the graph images
/// overlay the LEFT of the pane (see `overlay_pixel_graph`), so a right-border
/// track never collides with them. Styled via the theme's muted track / brighter
/// thumb so it stays coherent in light, dark, and background-adapted palettes.
fn render_scrollbar(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    content_length: usize,
    viewport_length: usize,
    position: usize,
) {
    if !scrollbar_needed(content_length, viewport_length, area.height) {
        return;
    }
    let mut state = ScrollbarState::new(content_length)
        .viewport_content_length(viewport_length)
        .position(position);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_style(theme.scrollbar_track_style())
        .thumb_style(theme.scrollbar_thumb_style());
    frame.render_stateful_widget(
        scrollbar,
        area.inner(Margin {
            vertical: 1,
            horizontal: 0,
        }),
        &mut state,
    );
}

/// Render the main UI
pub fn draw(frame: &mut Frame, app: &mut App) {
    // Rebuild display items so they match the latest diff data.
    // Selection is path-based so this doesn't reset it.
    app.sync_file_list_cache();

    let theme = app.theme();
    let area = frame.area();

    // Check minimum terminal size to prevent buffer overflow panics
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let msg = format!(
            "Terminal too small ({}x{}). Need at least {}x{}.",
            area.width, area.height, MIN_WIDTH, MIN_HEIGHT
        );
        let paragraph = Paragraph::new(msg).style(Style::default().fg(theme.file_deleted));
        frame.render_widget(paragraph, area);
        return;
    }

    // FileDiff mode: full-screen diff view
    if matches!(app.mode, AppMode::FileDiff { .. }) {
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);

        // Update viewport dimensions for scroll calculations (minus borders)
        app.diff_viewport_height = vertical[0].height.saturating_sub(2);
        app.diff_viewport_width = vertical[0].width.saturating_sub(2);

        // Re-lay-out (soft-wrap) the diff if the wrap toggle or pane width has
        // changed since the last frame, before taking the immutable borrow below.
        app.ensure_diff_layout();

        let AppMode::FileDiff {
            content,
            rendered_lines,
            scroll_offset,
            horizontal_offset,
            file_index,
            file_list,
            ..
        } = &app.mode
        else {
            unreachable!("guarded by matches! above");
        };

        // Capture scroll geometry before the app.mode borrow ends so the
        // scrollbar can render after the status bar takes a &mut borrow.
        let diff_total = rendered_lines.len();
        let diff_viewport = app.diff_viewport_height as usize;
        let diff_pos = *scroll_offset;

        frame.render_widget(
            FileDiffViewWidget::new(
                content,
                rendered_lines,
                *scroll_offset,
                *horizontal_offset,
                *file_index,
                file_list.len(),
                &theme,
            ),
            vertical[0],
        );
        render_scrollbar(frame, &theme, vertical[0], diff_total, diff_viewport, diff_pos);

        let status_bar = StatusBar::new(app, &theme);
        app.status_hints = status_bar.hint_regions(vertical[1]);
        frame.render_widget(status_bar, vertical[1]);
        // Toasts sit on top of the full-screen diff view too.
        render_toasts(frame, app, &theme);
        return;
    }

    // Issue modes: full-screen list/detail (mirroring FileDiff), with the
    // compose / label-picker / label-filter drawn as centered overlays on top of
    // the relevant backdrop.
    if is_issue_mode(&app.mode) {
        draw_issue_screen(frame, app, &theme, area);
        return;
    }

    // Vertical split: main area + status bar (1 row)
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let main_area = vertical[0];
    let status_area = vertical[1];

    // Split main area: graph + detail. The graph gets `graph_split_ratio`%;
    // the divider between them is drag-resizable.
    let graph_ratio = app.graph_split_ratio;
    let detail_ratio = 100u16.saturating_sub(graph_ratio);
    let (graph_area, detail_area) = if app.side_panel_layout {
        // Side layout: detail on LEFT, graph on RIGHT.
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(detail_ratio),
                Constraint::Percentage(graph_ratio),
            ])
            .split(main_area);
        (h[1], h[0])
    } else {
        // Default: graph on TOP, detail on BOTTOM.
        let v = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(graph_ratio),
                Constraint::Percentage(detail_ratio),
            ])
            .split(main_area);
        (v[0], v[1])
    };

    // Split detail area into files pane + commit detail
    let detail_direction = if detail_area.width <= 56 {
        Direction::Vertical
    } else {
        Direction::Horizontal
    };
    let detail_chunks = Layout::default()
        .direction(detail_direction)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(detail_area);
    let files_area = detail_chunks[0];
    let commit_area = detail_chunks[1];

    // Record panel rects for mouse hit-testing.
    app.mouse_layout = crate::app::MouseLayout {
        graph: graph_area,
        files: files_area,
        commit: commit_area,
        main: main_area,
        side_layout: app.side_panel_layout,
    };

    // Pre-render pass: compute layout metrics that update App scroll state
    let commit_lines = compute_commit_detail_layout(app, commit_area, &theme);

    // Pixel graph pre-pass: (re)build the row specs and transmit their
    // protocols (needs &mut app) before the immutable borrow taken by the graph
    // widget. Specs are capped to the width the overlay will draw and cached
    // until the layout, filter, or width changes.
    // Branch-trace dimming for the visible window, layered onto the cached base
    // specs. `Some` only while tracing is active with a non-empty lineage; the
    // owned Vec lives until the overlay pass so both `sync_frame` and
    // `overlay_pixel_graph` see the same dimmed rows.
    let mut pixel_frame_dim: Option<Vec<graph_pixels::RowSpec>> = None;
    let pixel_mode = if app.pixel_graph.is_some() {
        let panel_available = graph_area
            .width
            .saturating_sub(2)
            .saturating_sub(graph_view::GRAPH_LEADING_COLUMNS) as usize;
        // The user's resize cap: specs (and thus cached protocols) depend on it,
        // so it's part of the cache key alongside the panel width.
        let needed = (app.graph_layout.max_lane + 1) * 2;
        let graph_width = graph_view::effective_graph_width(needed, app.graph_width_cap);
        // The base specs are trace- and scroll-independent, so the key omits the
        // traced selection and offset: moving the selection reuses this cache and
        // only re-dims the on-screen window below.
        let reuse = app
            .pixel_specs_cache
            .as_ref()
            .is_some_and(|(gen, filter, pa, gw, _)| {
                *gen == app.graph_generation
                    && filter == &app.commit_filter
                    && *pa as usize == panel_available
                    && *gw as usize == graph_width
            });
        if !reuse {
            let specs =
                graph_view::build_pixel_base_specs(app, &theme, graph_width, panel_available);
            app.pixel_specs_cache = Some((
                app.graph_generation,
                app.commit_filter.clone(),
                panel_available as u16,
                graph_width as u16,
                specs,
            ));
        }
        let offset = app.graph_nav.graph_list_state.offset();
        let viewport = graph_area.height.saturating_sub(2) as usize;
        let base_len = app.pixel_specs_cache.as_ref().unwrap().4.len();
        // Only the on-screen window (± 2 viewport-heights) is rasterized; the
        // dim overlay is built for exactly this window so it aligns with what
        // `sync_frame` transmits and `overlay_pixel_graph` draws.
        let (win_start, win_end) = graph_pixels::protocol_window(offset, viewport, base_len);
        // Two per-frame dim sources feed the overlay: branch-trace lineage and
        // base-update force-dim (#55). Build it when EITHER is active — the
        // latter must dim the back-merge connector even with tracing off, which
        // the old trace-only guard skipped (the pixel connector stayed bright
        // while the message muted).
        let trace_lineage = app.active_trace_lineage();
        let want_base_mute = app.metadata_columns.mute_base_merges
            && !app.merged.base_update.value().is_empty();
        pixel_frame_dim = if trace_lineage.is_some() || want_base_mute {
            let base = &app.pixel_specs_cache.as_ref().unwrap().4;
            Some(graph_view::dim_pixel_specs_window(
                app,
                &theme,
                base,
                trace_lineage.as_ref(),
                win_start,
                win_end,
            ))
        } else {
            None
        };
        let active = {
            // Disjoint field borrows: `specs` from pixel_specs_cache (or the
            // owned dim overlay), `pg` from pixel_graph.
            let base = &app.pixel_specs_cache.as_ref().unwrap().4;
            let specs: &[graph_pixels::RowSpec] = pixel_frame_dim.as_deref().unwrap_or(base);
            let windowed = &specs[win_start..win_end];
            app.pixel_graph.as_mut().is_some_and(|pg| {
                pg.sync_frame(windowed);
                pg.is_active()
            })
        };
        if !active {
            // Protocol creation kept failing — drop pixel rendering and fall
            // back to Unicode glyphs for the rest of the session.
            app.pixel_graph = None;
        }
        active
    } else {
        false
    };

    // Prepare avatar protocols for the visible authors (pixel mode + toggle on).
    // Built from an immutable borrow into an owned Vec first, so the mutable
    // `pixel_graph` borrow that transmits them doesn't overlap.
    if pixel_mode && app.metadata_columns.avatars {
        let reqs = build_avatar_reqs(app);
        if let Some(pg) = app.pixel_graph.as_mut() {
            pg.sync_avatars(&reqs);
        }
    }

    // Render widgets
    // Inner height (minus the block's top/bottom borders) is the drawable row
    // count; the widget builds only the rows this viewport can show.
    let graph_viewport = graph_area.height.saturating_sub(2);
    let graph_widget =
        GraphViewWidget::new(app, graph_area.width, &theme, pixel_mode, graph_viewport);
    app.graph_chip_hits = graph_widget.chip_hits.clone();
    frame.render_stateful_widget(
        graph_widget,
        graph_area,
        &mut app.graph_nav.graph_list_state,
    );
    if pixel_mode {
        // Draw the dimmed window overlay when tracing, else the cached base
        // specs. The on-screen rows the overlay reads are inside the window, so
        // the dim overlay always has real specs there.
        let base = app.pixel_specs_cache.as_ref().map(|c| c.4.as_slice());
        if let Some(specs) = pixel_frame_dim.as_deref().or(base) {
            overlay_pixel_graph(frame, app, graph_area, specs);
        }
        if app.metadata_columns.avatars {
            overlay_avatars(frame, app, graph_area);
        }
    }
    let mut files_state = FilesPaneState {
        selected: Some(app.file_selected_index()),
        offset: 0,
    };
    frame.render_stateful_widget(FilesPaneWidget::new(app, &theme), files_area, &mut files_state);
    // The widget windows around the selection; keep the resulting offset for
    // mouse hit-testing.
    app.files_view_offset = files_state.offset;
    frame.render_widget(
        CommitDetailWidget::new(app, commit_area, &theme, commit_lines),
        commit_area,
    );

    // Scrollbars on the right border of each scrollable pane. Rendered after the
    // panes (and the pixel overlay) so the track sits on top of the border.
    // - Graph: `graph_chip_hits` has one entry per rendered (filtered) row, so
    //   its length is the filtered item count that the ListState offset indexes.
    render_scrollbar(
        frame,
        &theme,
        graph_area,
        app.graph_chip_hits.len(),
        graph_area.height.saturating_sub(2) as usize,
        app.graph_nav.graph_list_state.offset(),
    );
    render_scrollbar(
        frame,
        &theme,
        files_area,
        app.display_items().len(),
        files_area.height.saturating_sub(2) as usize,
        app.files_view_offset,
    );
    // Commit detail wraps text; max_scroll + visible rows recovers the wrapped
    // content height without recomputing it.
    render_scrollbar(
        frame,
        &theme,
        commit_area,
        (app.commit_detail_max_scroll + app.commit_detail_visible_rows) as usize,
        app.commit_detail_visible_rows as usize,
        app.commit_detail_scroll as usize,
    );

    let status_bar = StatusBar::new(app, &theme);
    app.status_hints = status_bar.hint_regions(status_area);
    frame.render_widget(status_bar, status_area);

    // Show cursor when editing commit message
    if app.editing_commit_message && app.focused_panel == crate::app::FocusedPanel::CommitDetail {
        let (cursor_row, cursor_col) = app.commit_editor.cursor_position();
        // Border (1) + the commit-detail block's horizontal padding (1).
        let commit_inner_x = commit_area.x + 2;
        let commit_inner_y = commit_area.y + 1;
        let editor_start_line = app.commit_editor_line_offset;
        let absolute_row = editor_start_line + cursor_row as u16;
        let cursor_x = commit_inner_x + cursor_col as u16;
        let cursor_y =
            commit_inner_y + absolute_row.saturating_sub(app.commit_detail_scroll);
        if cursor_y < commit_area.y + commit_area.height - 1
            && cursor_y >= commit_inner_y
            && cursor_x < commit_area.x + commit_area.width - 2
        {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    // PR-thread pre-pass: clamp the conversation scroll to its wrapped height
    // (needs &mut app before the immutable borrow in the popup match below).
    if matches!(app.mode, AppMode::PrThread) {
        let popup = centered_rect(PR_THREAD_POPUP_PCT.0, PR_THREAD_POPUP_PCT.1, area);
        // 2 border cols + 1 col horizontal padding each side (popup_block).
        let inner_w = popup.width.saturating_sub(4);
        // 2 border rows + 1 footer row.
        let body_h = popup.height.saturating_sub(3) as usize;
        let total = app.pr_thread.as_ref().map(|v| {
            let lines = pr_thread::build_lines(&v.state, &theme);
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .line_count(inner_w)
        });
        if let (Some(total), Some(v)) = (total, app.pr_thread.as_mut()) {
            v.max_scroll = total.saturating_sub(body_h);
            v.scroll = v.scroll.min(v.max_scroll);
        }
    }

    // Popups. Each interactive popup records its rect for mouse hit-testing
    // (click-inside routes, click-outside closes).
    let mut rendered_popup: Option<Rect> = None;
    match &app.mode {
        AppMode::Help => {
            let popup_area = centered_rect(60, 70, area);
            frame.render_widget(
                HelpPopup::new(app.is_uncommitted_selected(), &theme),
                popup_area,
            );
        }
        AppMode::Input {
            input,
            action: InputAction::Search,
            ..
        } => {
            // Search dropdown at bottom of screen
            let results = app.search_results();
            let height = calculate_dropdown_height(results.len());
            let popup_area = bottom_rect(60, height, area);
            frame.render_widget(
                SearchDropdown::new(
                    input,
                    results,
                    &app.graph_nav.branch_positions,
                    app.search_selection(),
                    &theme,
                ),
                popup_area,
            );
        }
        AppMode::Input { title, input, action } => {
            let popup_area = centered_rect(50, 20, area);
            let widget = if matches!(action, InputAction::AuthPassword) {
                InputDialog::masked(title, input, &theme)
            } else {
                InputDialog::new(title, input, &theme)
            };
            frame.render_widget(widget, popup_area);
        }
        AppMode::Confirm { message, .. } => {
            let popup_area = centered_rect(50, 20, area);
            frame.render_widget(ConfirmDialog::new(message, &theme), popup_area);
        }
        AppMode::CommitMenu {
            items,
            selected,
            filter,
        } => {
            let menu_height = (items.len() + 2).min(20) as u16;
            let menu_width = 42;
            // Right-click anchors the menu at the cursor (clamped on-screen);
            // keyboard opens it centered.
            let popup_area = match app.menu_anchor {
                Some(anchor) => {
                    let (x, y) = crate::mouse::clamp_menu_pos(
                        anchor,
                        menu_width,
                        menu_height,
                        (area.width, area.height),
                    );
                    Rect::new(x, y, menu_width, menu_height)
                }
                None => centered_rect_fixed(menu_width, menu_height, area),
            };
            frame.render_widget(
                CommitMenuWidget::new(items, *selected, filter, &theme),
                popup_area,
            );
            rendered_popup = Some(popup_area);
        }
        AppMode::MetadataMenu { selected } => {
            use self::metadata_menu::MetadataMenuWidget;
            // 7 rows + top/bottom border. Wide enough for the longest label
            // ("Mute base-update merges") plus its "> [x] " prefix.
            let popup_area = centered_rect_fixed(32, 9, area);
            frame.render_widget(
                MetadataMenuWidget::new(app.metadata_columns, *selected, &theme),
                popup_area,
            );
            rendered_popup = Some(popup_area);
        }
        AppMode::PullDivergence { selected } => {
            let popup_area = centered_rect_fixed(48, 8, area);
            frame.render_widget(PullDivergenceDialog::new(*selected, &theme), popup_area);
            rendered_popup = Some(popup_area);
        }
        AppMode::Settings { selected, editing } => {
            use self::settings_menu::SettingsMenuWidget;
            let model = app.settings_model();
            // Rows + 4 group headers + footer + borders; cap to the frame.
            let want = (crate::settings::descriptors().len()
                + crate::settings::SettingGroup::ALL.len()
                + 3) as u16;
            let height = want.min(area.height.saturating_sub(2)).max(6);
            let popup_area = centered_rect_fixed(52, height, area);
            frame.render_widget(
                SettingsMenuWidget::new(&model, *selected, editing.as_deref(), &theme),
                popup_area,
            );
            rendered_popup = Some(popup_area);
        }
        AppMode::CiChecks => {
            if let Some(view) = &app.ci_checks {
                use self::ci_checks::CiChecksWidget;
                let popup_area = centered_rect(72, 74, area);
                frame.render_widget(CiChecksWidget::new(view, &theme), popup_area);
                rendered_popup = Some(popup_area);
            }
        }
        AppMode::PrThread => {
            if let Some(view) = &app.pr_thread {
                use self::pr_thread::PrThreadWidget;
                let popup_area = centered_rect(PR_THREAD_POPUP_PCT.0, PR_THREAD_POPUP_PCT.1, area);
                frame.render_widget(PrThreadWidget::new(view, &theme), popup_area);
                rendered_popup = Some(popup_area);
            }
        }
        AppMode::PrCompose { purpose } => {
            use self::pr_compose::{text_area, PrComposeWidget};
            let popup_area = centered_rect(64, 60, area);
            rendered_popup = Some(popup_area);
            frame.render_widget(
                PrComposeWidget::new(&app.pr_editor, *purpose, &theme),
                popup_area,
            );
            // Place the terminal cursor at the editor position.
            let (row, col) = app.pr_editor.cursor_position();
            let body = text_area(popup_area);
            let cx = body.x + col as u16;
            let cy = body.y + row as u16;
            if cx < body.x + body.width && cy < body.y + body.height {
                frame.set_cursor_position((cx, cy));
            }
        }
        AppMode::PrMergePicker { selected, .. } => {
            let labels: Vec<&str> = crate::pr_action::MergeMethod::ALL
                .iter()
                .map(|m| m.label())
                .collect();
            let popup_area = centered_rect_fixed(40, 9, area);
            frame.render_widget(
                OptionsDialog::new("Merge Pull Request", "Merge method:", &labels, *selected, &theme),
                popup_area,
            );
            rendered_popup = Some(popup_area);
        }
        AppMode::PrReviewPicker { selected, .. } => {
            let labels: Vec<&str> = crate::pr_action::ReviewDecision::ALL
                .iter()
                .map(|d| d.label())
                .collect();
            let popup_area = centered_rect_fixed(40, 9, area);
            frame.render_widget(
                OptionsDialog::new("Submit Review", "Disposition:", &labels, *selected, &theme),
                popup_area,
            );
            rendered_popup = Some(popup_area);
        }
        // Issue modes render full-screen via `draw_issue_screen` (early return
        // above), so they never reach this popup match.
        AppMode::BranchPicker { branches, selected } => {
            let max_name_len = branches.iter().map(|b| b.len()).max().unwrap_or(10);
            let popup_width = (max_name_len + 6).clamp(30, 60) as u16;
            let popup_height = (branches.len() + 2).min(12) as u16;
            let popup_area = centered_rect_fixed(popup_width, popup_height, area);
            frame.render_widget(
                BranchPickerWidget::new(branches, *selected, &theme),
                popup_area,
            );
            rendered_popup = Some(popup_area);
        }
        AppMode::BranchDeletePicker { branches, selected } => {
            let max_name_len = branches.iter().map(|b| b.len()).max().unwrap_or(10);
            let popup_width = (max_name_len + 6).clamp(30, 60) as u16;
            let popup_height = (branches.len() + 2).min(12) as u16;
            let popup_area = centered_rect_fixed(popup_width, popup_height, area);
            frame.render_widget(
                BranchPickerWidget::with_title(branches, *selected, &theme, " Delete Branch "),
                popup_area,
            );
            rendered_popup = Some(popup_area);
        }
        AppMode::TagPicker {
            tags,
            selected,
            action,
        } => {
            let max_name_len = tags.iter().map(|t| t.len()).max().unwrap_or(10);
            let popup_width = (max_name_len + 6).clamp(30, 60) as u16;
            let popup_height = (tags.len() + 2).min(12) as u16;
            let popup_area = centered_rect_fixed(popup_width, popup_height, area);
            let title = match action {
                crate::app::TagAction::Delete => " Delete Tag ",
                crate::app::TagAction::Push => " Push Tag ",
            };
            frame.render_widget(
                BranchPickerWidget::with_title(tags, *selected, &theme, title),
                popup_area,
            );
            rendered_popup = Some(popup_area);
        }
        AppMode::RemotePicker { remotes, selected, op } => {
            let title = match op {
                crate::app::RemoteOp::Fetch => " Fetch From Remote ",
                crate::app::RemoteOp::Pull => " Pull From Remote ",
                crate::app::RemoteOp::Push => " Push To Remote ",
                crate::app::RemoteOp::Prune => " Prune Remote ",
            };
            let max_name_len = remotes.iter().map(|b| b.len()).max().unwrap_or(10);
            let popup_width = (max_name_len + 6).clamp(30, 60) as u16;
            let popup_height = (remotes.len() + 2).min(12) as u16;
            let popup_area = centered_rect_fixed(popup_width, popup_height, area);
            frame.render_widget(
                BranchPickerWidget::with_title(remotes, *selected, &theme, title),
                popup_area,
            );
            rendered_popup = Some(popup_area);
        }
        AppMode::BranchFilter {
            filter,
            selected,
            all_branches,
        } => {
            let author_of = |b: &String| {
                app.branch_authors
                    .get(b)
                    .map(String::as_str)
                    .unwrap_or("")
            };
            let filtered_count = all_branches
                .iter()
                .filter(|b| {
                    branch_filter::matches_branch_filter(b, author_of(b), filter)
                })
                .count();
            // +3 for borders and footer; keep at least one body row so the
            // empty-state ("no matching branches") placeholder has room.
            let popup_height = (filtered_count.max(1) + 3).min(24) as u16;
            // Room for "[x] " + name + a gap + author.
            let max_row_len = all_branches
                .iter()
                .map(|b| b.len() + author_of(b).len())
                .max()
                .unwrap_or(10);
            let popup_width = (max_row_len + 12).clamp(46, 72) as u16;
            let popup_area = centered_rect_fixed(popup_width, popup_height, area);
            frame.render_widget(
                BranchFilterWidget::new(
                    all_branches,
                    &app.hidden_branches,
                    &app.branch_authors,
                    filter,
                    *selected,
                    &theme,
                ),
                popup_area,
            );
            rendered_popup = Some(popup_area);
        }
        AppMode::FileHistory {
            path,
            entries,
            selected,
        } => {
            let popup_height = (entries.len() + 2).clamp(6, 24) as u16;
            let popup_width = area.width.saturating_sub(6).clamp(30, 80);
            let popup_area = centered_rect_fixed(popup_width, popup_height, area);
            frame.render_widget(
                FileHistoryWidget::new(entries, *selected, &theme, path),
                popup_area,
            );
            rendered_popup = Some(popup_area);
        }
        AppMode::CommandPalette { query, selected } => {
            use self::command_palette::CommandPaletteWidget;
            let results = app.palette_results(query);
            // A tall, wide centered popup: query line + up to PALETTE_CAP rows +
            // borders (+ a footer line when capped).
            let footer = usize::from(results.more > 0);
            let popup_height =
                (results.items.len() + 3 + footer).clamp(6, 22) as u16;
            let popup_width = area.width.saturating_sub(6).clamp(40, 90);
            let popup_area = centered_rect_fixed(popup_width, popup_height, area);
            frame.render_widget(
                CommandPaletteWidget::new(
                    query,
                    &results.items,
                    results.more,
                    *selected,
                    &theme,
                ),
                popup_area,
            );
            rendered_popup = Some(popup_area);
        }
        _ => {}
    }

    // Record the active popup's rect for mouse hit-testing (click-inside to
    // select a row, click-outside to dismiss).
    app.popup_rect = rendered_popup;

    // Toasts render last, so they sit on top of every panel and popup.
    render_toasts(frame, app, &theme);
}

/// Render stacked toast notifications in the top-right corner, newest on top,
/// over whatever else is on screen.
/// Draw a full-screen issue view (list or detail) plus any centered overlay
/// (compose / label picker / label filter) and the shared status bar. Mirrors
/// the `FileDiff` full-screen path.
fn draw_issue_screen(frame: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);
    let content = vertical[0];
    let status_area = vertical[1];

    let bg_is_detail = issue_bg_is_detail(app);

    // Detail scroll pre-pass (clamp to wrapped height) before immutable borrows,
    // matching the PR-thread pre-pass. Geometry: full content rect minus the
    // border rows; horizontal padding removes 2 more columns on each side.
    if bg_is_detail {
        let inner_w = content.width.saturating_sub(4);
        let body_h = content.height.saturating_sub(2) as usize;
        let total = app.issue_detail.as_ref().map(|v| {
            let lines = issue_detail::build_lines(&v.state, theme);
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .line_count(inner_w)
        });
        if let (Some(total), Some(v)) = (total, app.issue_detail.as_mut()) {
            v.max_scroll = total.saturating_sub(body_h);
            v.scroll = v.scroll.min(v.max_scroll);
        }
    }

    // Full-screen backdrop.
    if bg_is_detail {
        if let Some(view) = &app.issue_detail {
            frame.render_widget(issue_detail::IssueDetailWidget::new(view, theme), content);
        }
    } else if let Some(view) = &app.issue_list {
        let empty = std::collections::HashSet::new();
        let blocked = app.issue_fetch.cached_blocked().unwrap_or(&empty);
        let repo_name = std::path::Path::new(&app.repo_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(app.repo_path.as_str());
        frame.render_widget(
            issue_list::IssueListWidget::new(view, blocked, repo_name, theme),
            content,
        );
    }

    // Centered overlay on top of the backdrop.
    match &app.mode {
        AppMode::IssueCompose { purpose } => {
            let popup_area = centered_rect(64, 60, area);
            frame.render_widget(
                issue_compose::IssueComposeWidget::new(&app.issue_editor, *purpose, theme),
                popup_area,
            );
            let (row, col) = app.issue_editor.cursor_position();
            let body = pr_compose::text_area(popup_area);
            let cx = body.x + col as u16;
            let cy = body.y + row as u16;
            if cx < body.x + body.width && cy < body.y + body.height {
                frame.set_cursor_position((cx, cy));
            }
        }
        AppMode::IssueLabelPicker { selected, .. } => {
            if let Some(picker) = &app.issue_label_picker {
                let rows = (picker.labels.len() + 3).clamp(6, 24) as u16;
                let popup_area = centered_rect_fixed(48, rows, area);
                frame.render_widget(
                    issue_detail::IssueLabelPickerWidget::new(picker, *selected, theme),
                    popup_area,
                );
            }
        }
        AppMode::IssueLabelFilter { selected } => {
            if let Some(picker) = &app.issue_label_filter {
                let rows = (picker.labels.len() + 3).clamp(6, 24) as u16;
                let popup_area = centered_rect_fixed(52, rows, area);
                frame.render_widget(
                    issue_list::IssueLabelFilterWidget::new(picker, *selected, theme),
                    popup_area,
                );
            }
        }
        _ => {}
    }

    let status_bar = StatusBar::new(app, theme);
    app.status_hints = status_bar.hint_regions(status_area);
    frame.render_widget(status_bar, status_area);
    render_toasts(frame, app, theme);
}

fn render_toasts(frame: &mut Frame, app: &App, theme: &Theme) {
    use crate::toast::ToastKind;
    let toasts = app.toasts.visible();
    if toasts.is_empty() {
        return;
    }
    let area = frame.area();
    // Each toast is a 3-row bordered box; width fits the text within bounds.
    const MAX_W: u16 = 44;
    const H: u16 = 3;
    let margin = 1u16;
    let width = MAX_W.min(area.width.saturating_sub(margin * 2));
    if width < 8 || area.height < H + margin {
        return;
    }

    // Newest on top: iterate newest → oldest, stacking downward from the top.
    for (i, toast) in toasts.iter().rev().enumerate() {
        let y = margin + i as u16 * H;
        if y + H > area.height {
            break;
        }
        let x = area.width.saturating_sub(width + margin);
        let rect = Rect::new(x, y, width, H);

        let (accent, icon) = match toast.kind {
            ToastKind::Success => (theme.pr_ci_pass, "✓"),
            ToastKind::Error => (theme.pr_ci_fail, "✗"),
            ToastKind::Info => (theme.pr_badge, "•"),
        };
        frame.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(accent))
            .style(Style::default().bg(theme.popup_bg));
        let text_w = width.saturating_sub(4) as usize; // borders + icon + space
        let line = Line::from(vec![
            Span::styled(
                format!("{icon} "),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate_str(&toast.text, text_w),
                Style::default().fg(theme.text_primary),
            ),
        ]);
        frame.render_widget(Paragraph::new(line).block(block), rect);
    }
}

/// Truncate to `max` display columns with an ellipsis.
/// Truncate `s` to at most `max` display columns, appending `…` when it
/// overflows. Uses Unicode display width (not char count) so wide CJK/emoji
/// glyphs are measured correctly and the result — including the ellipsis — never
/// exceeds `max` columns. Shared by widgets that write with `Buffer::set_string`,
/// which does not clip.
pub(crate) fn truncate_str(s: &str, max: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    // Reserve one column for the ellipsis.
    let budget = max - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Overlay pixel-rendered graph images on top of the (blank) graph column.
///
/// Runs after the list widget has rendered and written the current scroll
/// offset back into `graph_list_state`. Each visible row's image is placed at
/// the leading-space column so it lines up with the spaces emitted by
/// `render_graph_line` in pixel mode. Images are transparent, so the list's
/// selection highlight shows through.
fn overlay_pixel_graph(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    specs: &[graph_pixels::RowSpec],
) {
    use ratatui_image::Image;
    if area.width < MIN_WIDGET_WIDTH || area.height < MIN_WIDGET_HEIGHT {
        return;
    }
    let Some(pg) = &app.pixel_graph else {
        return;
    };
    let inner_x = area.x + 1;
    let inner_y = area.y + 1;
    let inner_h = area.height.saturating_sub(2);
    // Graph column starts after the leading marker; specs were already
    // truncated to this width in the pre-pass, so draw them at their full cell
    // count (no re-clamping — a clamp here would desync the protocol's width
    // from the rect and blank the row on iTerm2/Sixel). The image carries a
    // transparent pad column on its left (HEAD-star spill room), so the rect
    // starts that many cells before the graph column, over the leading spaces.
    let x = inner_x + graph_view::GRAPH_LEADING_COLUMNS
        - graph_pixels::PIXEL_LEFT_PAD_CELLS;
    let offset = app.graph_nav.graph_list_state.offset();
    for row in 0..inner_h {
        let idx = offset + row as usize;
        let Some(spec) = specs.get(idx) else {
            break;
        };
        let Some(proto) = pg.get(spec) else {
            continue;
        };
        if spec.cells.is_empty() {
            continue;
        }
        let w = spec.cells.len() as u16 + graph_pixels::PIXEL_LEFT_PAD_CELLS;
        let rect = Rect::new(x, inner_y + row, w, 1);
        frame.render_widget(Image::new(proto), rect);
    }
}

/// The avatars to prepare this frame: one per unique visible author email whose
/// download has resolved (pending emails are skipped — nothing drawn yet). The
/// fallback color is precomputed so a decode failure still yields a disc.
fn build_avatar_reqs(app: &App) -> Vec<graph_pixels::AvatarReq> {
    use crate::avatar_fetch::AvatarState;
    let dir = crate::avatar::cache_dir();
    let mut reqs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in graph_view::visible_rows(app, true) {
        let Some(commit) = &row.node.commit else {
            continue;
        };
        let email = &commit.author_email;
        if email.trim().is_empty() || !seen.insert(email.clone()) {
            continue;
        }
        let source = match app.avatar_fetch.state_of(email) {
            Some(AvatarState::Ready) => match &dir {
                Some(d) => graph_pixels::AvatarSource::Ready(crate::avatar::cache_png_path(d, email)),
                None => continue,
            },
            Some(AvatarState::Missing) => graph_pixels::AvatarSource::Fallback,
            None => continue, // still downloading — draw nothing yet
        };
        reqs.push(graph_pixels::AvatarReq {
            email: email.clone(),
            source,
            color: crate::avatar::fallback_color(email),
        });
    }
    reqs
}

/// Overlay each on-screen row's author avatar in the reserved columns between
/// the graph and the message (pixel mode). Mirrors `overlay_pixel_graph`'s
/// offset/index-space handling; the 2-cell-wide protocol matches its 2-cell rect
/// (required by iTerm2).
fn overlay_avatars(frame: &mut Frame, app: &App, area: Rect) {
    use ratatui_image::Image;
    if area.width < MIN_WIDGET_WIDTH || area.height < MIN_WIDGET_HEIGHT {
        return;
    }
    let Some(pg) = &app.pixel_graph else {
        return;
    };
    let inner_x = area.x + 1;
    let inner_y = area.y + 1;
    let inner_h = area.height.saturating_sub(2);
    let needed = (app.graph_layout.max_lane + 1) * 2;
    let graph_width = graph_view::effective_graph_width(needed, app.graph_width_cap);
    let avatar_x = graph_view::avatar_overlay_x(inner_x, graph_width);
    let rows = graph_view::visible_rows(app, true);
    let offset = app.graph_nav.graph_list_state.offset();
    for row in 0..inner_h {
        let idx = offset + row as usize;
        let Some(rr) = rows.get(idx) else {
            break;
        };
        let Some(commit) = &rr.node.commit else {
            continue;
        };
        let Some(proto) = pg.get_avatar(&commit.author_email) else {
            continue;
        };
        let rect = Rect::new(avatar_x, inner_y + row, graph_view::AVATAR_IMAGE_CELLS, 1);
        frame.render_widget(Image::new(proto), rect);
    }
}

/// Calculate a centered rectangle
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Calculate a centered rectangle with fixed pixel dimensions
fn centered_rect_fixed(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

/// Calculate a bottom-aligned rectangle (for dropdowns)
fn bottom_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let clamped_height = height.min(area.height.saturating_sub(2));
    let y = area.y + area.height.saturating_sub(clamped_height + 1);

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(area);

    Rect::new(horizontal[1].x, y, horizontal[1].width, clamped_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrollbar_paints_the_right_border_column_when_content_overflows() {
        use ratatui::{backend::TestBackend, Terminal};
        let theme = Theme::dark();

        // Overflowing content: expect glyphs on the right column, inset from the
        // corners (rows 1..height-1).
        let mut term = Terminal::new(TestBackend::new(10, 8)).unwrap();
        term.draw(|f| render_scrollbar(f, &theme, Rect::new(0, 0, 10, 8), 100, 6, 0))
            .unwrap();
        let buf = term.backend().buffer();
        let painted = (1..7).any(|y| buf[(9, y)].symbol() != " ");
        assert!(painted, "an overflowing pane draws a scrollbar on its right column");

        // Content that fits draws nothing at all.
        let mut term = Terminal::new(TestBackend::new(10, 8)).unwrap();
        term.draw(|f| render_scrollbar(f, &theme, Rect::new(0, 0, 10, 8), 3, 6, 0))
            .unwrap();
        let buf = term.backend().buffer();
        let painted = (0..8).any(|y| (0..10).any(|x| buf[(x, y)].symbol() != " "));
        assert!(!painted, "a pane whose content fits draws no scrollbar");
    }

    #[test]
    fn truncate_str_measures_display_width() {
        use unicode_width::UnicodeWidthStr;

        // ASCII: fits untouched, exact-fit untouched, overflow gets an ellipsis.
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello", 5), "hello");
        assert_eq!(truncate_str("hello world", 5), "hell…");
        assert_eq!(UnicodeWidthStr::width(truncate_str("hello world", 5).as_str()), 5);

        // CJK glyphs are 2 columns each: 3 chars = 6 columns.
        assert_eq!(truncate_str("日本語", 6), "日本語");
        // max 5 can't fit the third wide glyph (2 cols) + ellipsis, so one glyph
        // + ellipsis = 3 columns, never exceeding 5.
        let cjk = truncate_str("日本語テスト", 5);
        assert!(UnicodeWidthStr::width(cjk.as_str()) <= 5, "got {cjk:?}");
        assert!(cjk.ends_with('…'));

        // Emoji (width 2) never overflow the budget.
        let emoji = truncate_str("🐛🐛🐛🐛", 5);
        assert!(UnicodeWidthStr::width(emoji.as_str()) <= 5, "got {emoji:?}");
        assert!(emoji.ends_with('…'));

        // Zero max yields empty (can't even fit the ellipsis).
        assert_eq!(truncate_str("anything", 0), "");
        // Empty input stays empty regardless of max.
        assert_eq!(truncate_str("", 0), "");
        assert_eq!(truncate_str("", 5), "");
    }

    #[test]
    fn scrollbar_hidden_unless_content_overflows_a_tall_enough_pane() {
        // Content fits the viewport → no scrollbar.
        assert!(!scrollbar_needed(10, 10, 12));
        assert!(!scrollbar_needed(5, 10, 12));
        // Overflow in a pane with room for a track → shown.
        assert!(scrollbar_needed(20, 10, 12));
        // Overflow but the pane is too short to host a track between corners.
        assert!(!scrollbar_needed(20, 10, 2));
        assert!(!scrollbar_needed(20, 0, 1));
    }
}
