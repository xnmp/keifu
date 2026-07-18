//! UI components

pub mod branch_filter;
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
pub mod metadata_menu;
pub mod pr_compose;
pub mod pr_thread;
pub mod search_dropdown;
pub mod status_bar;
pub mod theme;

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
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

/// Render a placeholder block when widget area is too small
pub fn render_placeholder_block(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.unfocused_border_style());
    block.render(area, buf);
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
    if let AppMode::FileDiff {
        content,
        rendered_lines,
        scroll_offset,
        horizontal_offset,
        file_index,
        file_list,
        ..
    } = &app.mode
    {
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);

        // Update viewport dimensions for scroll calculations (minus borders)
        app.diff_viewport_height = vertical[0].height.saturating_sub(2);
        app.diff_viewport_width = vertical[0].width.saturating_sub(2);

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
        frame.render_widget(StatusBar::new(app, &theme), vertical[1]);
        // Toasts sit on top of the full-screen diff view too.
        render_toasts(frame, app, &theme);
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
    let pixel_mode = if app.pixel_graph.is_some() {
        let panel_available = graph_area
            .width
            .saturating_sub(2)
            .saturating_sub(graph_view::GRAPH_LEADING_COLUMNS) as usize;
        // The user's resize cap: specs (and thus cached protocols) depend on it,
        // so it's part of the cache key alongside the panel width.
        let needed = (app.graph_layout.max_lane + 1) * 2;
        let graph_width = graph_view::effective_graph_width(needed, app.graph_width_cap);
        // Branch tracing bakes a per-cell dim mask into the specs, so the cache
        // must vary by the traced selection. `None` when tracing is inactive, so
        // moving the selection with tracing off doesn't rebuild the specs.
        let trace_key = app.trace_selection_key();
        let reuse = app
            .pixel_specs_cache
            .as_ref()
            .is_some_and(|(gen, filter, pa, gw, tk, _)| {
                *gen == app.graph_generation
                    && filter == &app.commit_filter
                    && *pa as usize == panel_available
                    && *gw as usize == graph_width
                    && *tk == trace_key
            });
        if !reuse {
            let specs = graph_view::build_pixel_row_specs(app, &theme, graph_width, panel_available);
            app.pixel_specs_cache = Some((
                app.graph_generation,
                app.commit_filter.clone(),
                panel_available as u16,
                graph_width as u16,
                trace_key,
                specs,
            ));
        }
        // Disjoint field borrows: `specs` from pixel_specs_cache, `pg` from
        // pixel_graph. Sync transmits/evicts protocols for this frame.
        let specs = &app.pixel_specs_cache.as_ref().unwrap().5;
        let active = app.pixel_graph.as_mut().is_some_and(|pg| {
            pg.sync_frame(specs);
            pg.is_active()
        });
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
    let graph_widget = GraphViewWidget::new(app, graph_area.width, &theme, pixel_mode);
    app.graph_chip_hits = graph_widget.chip_hits.clone();
    frame.render_stateful_widget(
        graph_widget,
        graph_area,
        &mut app.graph_nav.graph_list_state,
    );
    if pixel_mode {
        if let Some((_, _, _, _, _, specs)) = &app.pixel_specs_cache {
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
    frame.render_widget(StatusBar::new(app, &theme), status_area);

    // Show cursor when editing commit message
    if app.editing_commit_message && app.focused_panel == crate::app::FocusedPanel::CommitDetail {
        let (cursor_row, cursor_col) = app.commit_editor.cursor_position();
        let commit_inner_x = commit_area.x + 1;
        let commit_inner_y = commit_area.y + 1;
        let editor_start_line = app.commit_editor_line_offset;
        let absolute_row = editor_start_line + cursor_row as u16;
        let cursor_x = commit_inner_x + cursor_col as u16;
        let cursor_y =
            commit_inner_y + absolute_row.saturating_sub(app.commit_detail_scroll);
        if cursor_y < commit_area.y + commit_area.height - 1
            && cursor_y >= commit_inner_y
            && cursor_x < commit_area.x + commit_area.width - 1
        {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    // PR-thread pre-pass: clamp the conversation scroll to its wrapped height
    // (needs &mut app before the immutable borrow in the popup match below).
    if matches!(app.mode, AppMode::PrThread) {
        let popup = centered_rect(PR_THREAD_POPUP_PCT.0, PR_THREAD_POPUP_PCT.1, area);
        let inner_w = popup.width.saturating_sub(2);
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
        AppMode::Input { title, input, .. } => {
            let popup_area = centered_rect(50, 20, area);
            frame.render_widget(InputDialog::new(title, input, &theme), popup_area);
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
            // 5 rows + top/bottom border.
            let popup_area = centered_rect_fixed(24, 7, area);
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
            let filtered_count = all_branches
                .iter()
                .filter(|b| b.to_lowercase().contains(&filter.to_lowercase()))
                .count();
            // +3 for borders and footer
            let popup_height = (filtered_count + 3).min(24) as u16;
            let max_name_len = all_branches.iter().map(|b| b.len()).max().unwrap_or(10);
            let popup_width = (max_name_len + 10).clamp(46, 60) as u16;
            let popup_area = centered_rect_fixed(popup_width, popup_height, area);
            frame.render_widget(
                BranchFilterWidget::new(all_branches, &app.hidden_branches, filter, *selected, &theme),
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
fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
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
