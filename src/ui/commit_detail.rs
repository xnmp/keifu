//! Commit detail widget

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::app::{App, FocusedPanel};
use crate::git::CommitDiffInfo;

use super::{render_placeholder_block, theme::Theme, MIN_WIDGET_HEIGHT, MIN_WIDGET_WIDTH};

/// Width threshold for switching to vertical layout
/// When panel width would be <= 28 chars, use vertical layout
const VERTICAL_LAYOUT_THRESHOLD: u16 = 56;

pub struct CommitDetailWidget<'a> {
    commit_lines: Vec<Line<'a>>,
    file_lines: Vec<Line<'a>>,
    selected_line_index: u16,
    is_focused: bool,
    is_files_focused: bool,
    files_title: Line<'a>,
    files_filter_active: bool,
    commit_scroll: u16,
    theme: &'a Theme,
}

/// Pre-render layout metrics for the commit detail panel.
/// Call this before constructing `CommitDetailWidget` to update App scroll state.
pub fn compute_commit_detail_layout(app: &mut App, detail_area: Rect) {
    let detail_width = detail_area.width;
    let theme = app.theme();
    let (commit_lines, raw_editor_offset) =
        CommitDetailWidget::build_commit_lines_with_offset(app, &theme);

    // Set the raw editor line offset from the line builder before wrapping adjustment.
    if let Some(offset) = raw_editor_offset {
        app.commit_editor_line_offset = offset;
    }

    let direction = if detail_width <= VERTICAL_LAYOUT_THRESHOLD {
        Direction::Vertical
    } else {
        Direction::Horizontal
    };
    let commit_chunk_height = if direction == Direction::Vertical {
        detail_area.height / 2
    } else {
        detail_area.height
    };
    let commit_inner_width = if direction == Direction::Vertical {
        detail_width.saturating_sub(2) as usize
    } else {
        (detail_width / 2).saturating_sub(2) as usize
    };
    let commit_visible = commit_chunk_height.saturating_sub(2) as usize;
    app.commit_detail_visible_rows = commit_visible as u16;

    // Use Ratatui's own line_count to match its actual wrapping behaviour.
    let commit_wrapped_total = if commit_inner_width > 0 {
        let p = Paragraph::new(commit_lines.clone()).wrap(Wrap { trim: false });
        p.line_count(commit_inner_width as u16)
    } else {
        commit_lines.len()
    };

    // Recompute editor line offset using wrapped line counts so the cursor
    // accounts for long hint text that wraps across multiple visual lines.
    if app.editing_commit_message && commit_inner_width > 0 {
        let raw_offset = app.commit_editor_line_offset as usize;
        let header_lines = &commit_lines[..raw_offset.min(commit_lines.len())];
        if !header_lines.is_empty() {
            let hp = Paragraph::new(header_lines.to_vec()).wrap(Wrap { trim: false });
            app.commit_editor_line_offset =
                hp.line_count(commit_inner_width as u16) as u16;
        }
    }
    app.commit_detail_max_scroll =
        commit_wrapped_total.saturating_sub(commit_visible) as u16;
    // Clamp current scroll to the newly computed max
    app.commit_detail_scroll = app
        .commit_detail_scroll
        .min(app.commit_detail_max_scroll);
}

impl<'a> CommitDetailWidget<'a> {
    pub fn new(app: &App, detail_area: Rect, theme: &'a Theme) -> Self {
        let detail_width = detail_area.width;
        let commit_lines = Self::build_commit_lines(app, theme);

        // Files pane gets ~half the detail width minus borders
        let files_inner_width = if detail_width <= VERTICAL_LAYOUT_THRESHOLD {
            detail_width.saturating_sub(2) as usize
        } else {
            (detail_width / 2).saturating_sub(2) as usize
        };
        let (file_lines, selected_line_index) = Self::build_file_lines(app, files_inner_width, theme);

        Self {
            commit_lines,
            file_lines,
            selected_line_index,
            is_focused: app.focused_panel == FocusedPanel::CommitDetail,
            is_files_focused: app.focused_panel == FocusedPanel::Files,
            files_filter_active: app.files_pane.files_filter_active,
            commit_scroll: app.commit_detail_scroll,
            theme,
            files_title: {
                let mut spans = vec![Span::raw(" Changed Files")];
                if app.files_pane.files_group_by_folder {
                    spans.push(Span::styled(
                        " [folders]",
                        Style::default().fg(theme.text_muted),
                    ));
                }
                if app.files_pane.files_filter_active {
                    spans.push(Span::styled(
                        format!(" filter: {}\u{2588}", app.files_pane.files_filter),
                        Style::default()
                            .fg(theme.status_mode_fg)
                            .bg(theme.border_filter_active)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else if !app.files_pane.files_filter.is_empty() {
                    spans.push(Span::styled(
                        format!(" [{}]", app.files_pane.files_filter),
                        Style::default().fg(theme.search_cursor),
                    ));
                }
                spans.push(Span::raw(" "));
                Line::from(spans)
            },
        }
    }

    /// Returns (lines, selected_line_index)
    fn build_file_lines(app: &App, max_width: usize, theme: &Theme) -> (Vec<Line<'a>>, u16) {
        use crate::app::FilesPaneItem;

        let selected_file_index = if app.focused_panel == FocusedPanel::Files {
            Some(app.file_selected_index())
        } else {
            None
        };

        let line_stats_loading = app.is_line_stats_loading();

        // Use the same items that file_selected_index() resolves against
        let items = app.display_items().to_vec();
        if items.is_empty() {
            if app.is_diff_loading() {
                return (vec![Line::from(Span::styled(
                    "Loading...",
                    Style::default().fg(theme.text_muted),
                ))], 0);
            }
            if let Some(diff) = app.cached_diff_or_quick() {
                return (Self::build_file_list_lines_from(Some(diff), selected_file_index, line_stats_loading, theme), 0);
            }
            return (Vec::new(), 0);
        }

        let mut lines = Vec::new();
        let mut selected_line_idx: u16 = 0;

        // Summary header
        if let Some(diff) = app.cached_diff_or_quick() {
            if line_stats_loading {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{} files changed", diff.total_files),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("  ...", Style::default().fg(theme.text_muted)),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{} files changed", diff.total_files),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(format!("+{}", diff.total_insertions), Style::default().fg(theme.file_added)),
                    Span::raw(" "),
                    Span::styled(format!("-{}", diff.total_deletions), Style::default().fg(theme.file_deleted)),
                ]));
            }
            lines.push(Line::from(""));
        }

        let indent = "  ";
        let prefix_len = 2; // "M " is 2 chars

        for (item_idx, item) in items.iter().enumerate() {
            match item {
                FilesPaneItem::Header(text) => {
                    let is_selected = selected_file_index == Some(item_idx);
                    if is_selected {
                        selected_line_idx = lines.len() as u16;
                    }
                    let style = if is_selected {
                        Style::default()
                            .fg(theme.help_header)
                            .bg(theme.selection_bg)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                            .fg(theme.help_header)
                            .add_modifier(Modifier::BOLD)
                    };
                    lines.push(Line::from(Span::styled(
                        format!("  {}", text),
                        style,
                    )));
                }
                FilesPaneItem::File(file) => {
                    let is_selected = selected_file_index == Some(item_idx);
                    if is_selected {
                        selected_line_idx = lines.len() as u16;
                    }
                    let (indicator, color) = theme.file_change_style(&file.kind);

                    let path_str = file.path.to_string_lossy().to_string();

                    // Build suffix (stats)
                    let suffix = if file.is_binary {
                        " (binary)".to_string()
                    } else if line_stats_loading {
                        " ...".to_string()
                    } else if file.insertions > 0 || file.deletions > 0 {
                        format!(" +{} -{}", file.insertions, file.deletions)
                    } else {
                        String::new()
                    };

                    let full_text = format!("{}{}", path_str, suffix);
                    let avail = max_width.saturating_sub(prefix_len);

                    let select_style = if is_selected {
                        theme.selection_style()
                    } else {
                        Style::default()
                    };

                    let icon = super::file_icons::file_icon(&file.path);

                    if avail == 0 || full_text.len() <= avail {
                        // Fits on one line
                        let mut spans = vec![
                            Span::styled(format!("{} ", indicator), Style::default().fg(color)),
                            Span::styled(format!("{} ", icon.icon), Style::default().fg(icon.color)),
                        ];
                        spans.push(Span::raw(path_str));
                        if file.is_binary {
                            spans.push(Span::styled(" (binary)", Style::default().fg(theme.text_muted)));
                        } else if line_stats_loading {
                            spans.push(Span::styled(" ...", Style::default().fg(theme.text_muted)));
                        } else if file.insertions > 0 || file.deletions > 0 {
                            spans.push(Span::raw(" "));
                            spans.push(Span::styled(format!("+{}", file.insertions), Style::default().fg(theme.file_added)));
                            spans.push(Span::raw(" "));
                            spans.push(Span::styled(format!("-{}", file.deletions), Style::default().fg(theme.file_deleted)));
                        }
                        lines.push(Line::from(spans).style(select_style));
                    } else {
                        // Wrap: first line has indicator + start of path
                        // continuation lines have indent + rest of path
                        let mut remaining = full_text.as_str();
                        let first_chunk_len = avail.min(remaining.len());
                        let first_chunk = &remaining[..first_chunk_len];
                        remaining = &remaining[first_chunk_len..];

                        lines.push(Line::from(vec![
                            Span::styled(format!("{} ", indicator), Style::default().fg(color)),
                            Span::styled(format!("{} ", icon.icon), Style::default().fg(icon.color)),
                            Span::raw(first_chunk.to_string()),
                        ]).style(select_style));

                        let cont_avail = max_width.saturating_sub(indent.len());
                        while !remaining.is_empty() {
                            let chunk_len = cont_avail.min(remaining.len());
                            let chunk = &remaining[..chunk_len];
                            remaining = &remaining[chunk_len..];
                            lines.push(Line::from(vec![
                                Span::raw(indent.to_string()),
                                Span::raw(chunk.to_string()),
                            ]).style(select_style));
                        }
                    }
                }
            }
        }

        (lines, selected_line_idx)
    }

    fn build_commit_lines(app: &App, theme: &Theme) -> Vec<Line<'a>> {
        Self::build_commit_lines_with_offset(app, theme).0
    }

    /// Build the commit detail lines. Returns `(lines, raw_editor_line_offset)`.
    /// The offset is `Some` when an editor section is present (uncommitted or amend).
    fn build_commit_lines_with_offset(app: &App, theme: &Theme) -> (Vec<Line<'a>>, Option<u16>) {
        let Some(selected) = app.graph_nav.graph_list_state.selected() else {
            return (vec![Line::from(Span::styled(
                "Select a commit",
                Style::default().fg(theme.text_muted),
            ))], None);
        };

        let Some(node) = app.graph_layout.nodes.get(selected) else {
            return (Vec::new(), None);
        };

        // Handle uncommitted changes node
        if node.is_uncommitted {
            let mut lines = vec![
                Line::from(Span::styled(
                    "Uncommitted Changes",
                    Style::default()
                        .fg(theme.text_muted)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ];

            if app.editing_commit_message {
                lines.push(Line::from(Span::styled(
                    "Commit Message:",
                    Style::default().fg(theme.search_cursor),
                )));
            } else if !app.commit_editor.text.is_empty() {
                lines.push(Line::from(Span::styled(
                    "Commit Message (Enter to edit):",
                    Style::default().fg(theme.text_muted),
                )));
            } else if app.focused_panel == FocusedPanel::CommitDetail {
                lines.push(Line::from(Span::styled(
                    "Press Enter to type a commit message",
                    Style::default().fg(theme.text_muted),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    match node.uncommitted_count {
                        Some(count) => format!("{} files with changes", count),
                        None => "files with changes".to_string(),
                    },
                    Style::default().fg(theme.text_muted),
                )));
            }

            // Show editor content with selection highlighting
            let mut editor_line_offset = None;
            if app.editing_commit_message || !app.commit_editor.text.is_empty() {
                lines.push(Line::from(""));
                // Track where editor text starts for auto-scroll
                editor_line_offset = Some(lines.len() as u16);
                let sel = app.commit_editor.selection.map(|s| s.ordered());
                let sel_style = Style::default().bg(theme.editor_selection_bg).fg(theme.editor_selection_fg);
                let mut byte_offset = 0usize;
                for line_text in app.commit_editor.lines() {
                    let line_start = byte_offset;
                    let line_end = line_start + line_text.len();
                    if let Some((sel_start, sel_end)) = sel {
                        if sel_start != sel_end && sel_start < line_end && sel_end > line_start {
                            // Selection overlaps this line
                            let hl_start = sel_start.max(line_start) - line_start;
                            let hl_end = sel_end.min(line_end) - line_start;
                            let mut spans = Vec::new();
                            if hl_start > 0 {
                                spans.push(Span::raw(line_text[..hl_start].to_string()));
                            }
                            spans.push(Span::styled(
                                line_text[hl_start..hl_end].to_string(),
                                sel_style,
                            ));
                            if hl_end < line_text.len() {
                                spans.push(Span::raw(line_text[hl_end..].to_string()));
                            }
                            lines.push(Line::from(spans));
                        } else {
                            lines.push(Line::from(Span::raw(line_text.to_string())));
                        }
                    } else {
                        lines.push(Line::from(Span::raw(line_text.to_string())));
                    }
                    // +1 for the newline separator between lines
                    byte_offset = line_end + 1;
                }
            }

            return (lines, editor_line_offset);
        }

        // Handle connector rows (no commit)
        let Some(commit) = &node.commit else {
            return (vec![Line::from(Span::styled(
                "(connector line)",
                Style::default().fg(theme.text_muted),
            ))], None);
        };

        // Build commit detail lines
        let mut lines = vec![
            // Author
            Line::from(vec![
                Span::styled("Author: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("{} <{}>", commit.author_name, commit.author_email),
                    Style::default().fg(theme.author_color),
                ),
            ]),
            // Date
            Line::from(vec![
                Span::styled("Date:   ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    commit.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                    Style::default().fg(theme.date_color),
                ),
            ]),
        ];

        lines.push(Line::from(""));

        // Message — show editor if amending this commit
        let mut editor_line_offset = None;
        if app.amending_commit && app.editing_commit_message && node.is_head {
            lines.push(Line::from(Span::styled(
                "Amend Message:",
                Style::default().fg(theme.search_cursor),
            )));
            lines.push(Line::from(""));
            editor_line_offset = Some(lines.len() as u16);
            let sel = app.commit_editor.selection.map(|s| s.ordered());
            let sel_style = Style::default().bg(theme.editor_selection_bg).fg(theme.editor_selection_fg);
            let mut byte_offset = 0usize;
            for line_text in app.commit_editor.lines() {
                let line_start = byte_offset;
                let line_end = line_start + line_text.len();
                if let Some((sel_start, sel_end)) = sel {
                    if sel_start != sel_end && sel_start < line_end && sel_end > line_start {
                        let hl_start = sel_start.max(line_start) - line_start;
                        let hl_end = sel_end.min(line_end) - line_start;
                        let mut spans = Vec::new();
                        if hl_start > 0 {
                            spans.push(Span::raw(line_text[..hl_start].to_string()));
                        }
                        spans.push(Span::styled(
                            line_text[hl_start..hl_end].to_string(),
                            sel_style,
                        ));
                        if hl_end < line_text.len() {
                            spans.push(Span::raw(line_text[hl_end..].to_string()));
                        }
                        lines.push(Line::from(spans));
                    } else {
                        lines.push(Line::from(Span::raw(line_text.to_string())));
                    }
                } else {
                    lines.push(Line::from(Span::raw(line_text.to_string())));
                }
                byte_offset = line_end + 1;
            }
        } else if node.is_head && app.focused_panel == FocusedPanel::CommitDetail && !app.editing_commit_message {
            lines.push(Line::from(Span::styled(
                "Press Enter to edit commit message (amend)",
                Style::default().fg(theme.text_muted),
            )));
            lines.push(Line::from(""));
            for line in commit.full_message.lines() {
                lines.push(Line::from(Span::raw(line.to_string())));
            }
        } else {
            for line in commit.full_message.lines() {
                lines.push(Line::from(Span::raw(line.to_string())));
            }
        }

        (lines, editor_line_offset)
    }

    fn build_file_list_lines_from(
        diff: Option<&CommitDiffInfo>,
        selected_file_index: Option<usize>,
        line_stats_loading: bool,
        theme: &Theme,
    ) -> Vec<Line<'a>> {
        let mut lines = Vec::new();

        let Some(diff) = diff else {
            return lines;
        };

        // Header row
        if line_stats_loading {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} files changed", diff.total_files),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ...", Style::default().fg(theme.text_muted)),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} files changed", diff.total_files),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("+{}", diff.total_insertions),
                    Style::default().fg(theme.file_added),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("-{}", diff.total_deletions),
                    Style::default().fg(theme.file_deleted),
                ),
            ]));
        }
        lines.push(Line::from(""));

        // File list
        for (idx, file) in diff.files.iter().enumerate() {
            let is_selected = selected_file_index == Some(idx);

            let (indicator, color) = theme.file_change_style(&file.kind);

            let path_str = file.path.to_string_lossy().to_string();

            let icon = super::file_icons::file_icon(&file.path);
            let mut spans = vec![
                Span::styled(format!("{} ", indicator), Style::default().fg(color)),
                Span::styled(format!("{} ", icon.icon), Style::default().fg(icon.color)),
                Span::raw(path_str),
            ];

            if file.is_binary {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    "(binary)",
                    Style::default().fg(theme.text_muted),
                ));
            } else if line_stats_loading {
                spans.push(Span::styled(
                    " ...",
                    Style::default().fg(theme.text_muted),
                ));
            } else if file.insertions > 0 || file.deletions > 0 {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!("+{}", file.insertions),
                    Style::default().fg(theme.file_added),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!("-{}", file.deletions),
                    Style::default().fg(theme.file_deleted),
                ));
            }

            let mut line = Line::from(spans);
            if is_selected {
                line = line.style(theme.selection_style());
            }
            lines.push(line);
        }

        // Truncation message
        if diff.truncated {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(
                    "  ...and {} more files",
                    diff.total_files - diff.files.len()
                ),
                Style::default().fg(theme.text_muted),
            )));
        }

        lines
    }
}

impl<'a> Widget for CommitDetailWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < MIN_WIDGET_WIDTH || area.height < MIN_WIDGET_HEIGHT {
            render_placeholder_block(area, buf, self.theme);
            return;
        }

        // Use vertical layout when each panel would be <= 28 chars wide
        let direction = if area.width <= VERTICAL_LAYOUT_THRESHOLD {
            Direction::Vertical
        } else {
            Direction::Horizontal
        };

        let chunks = Layout::default()
            .direction(direction)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);

        // Left: file list (files pane)
        let files_border = if self.files_filter_active {
            self.theme.border_filter_active
        } else if self.is_files_focused {
            self.theme.border_focused
        } else {
            self.theme.border_unfocused
        };
        let files_block = Block::default()
            .title(self.files_title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(files_border))
            .border_type(self.theme.border_type(self.is_files_focused));

        // Scroll file list so selected file stays visible.
        let visible_height = chunks[0].height.saturating_sub(2);
        let total_lines = self.file_lines.len() as u16;
        let max_scroll = total_lines.saturating_sub(visible_height);
        let scroll_y = if visible_height > 0 && self.selected_line_index >= visible_height {
            (self.selected_line_index - visible_height / 2).min(max_scroll)
        } else {
            0
        };

        let files_paragraph = Paragraph::new(self.file_lines)
            .block(files_block)
            .scroll((scroll_y, 0));

        Widget::render(files_paragraph, chunks[0], buf);

        // Right: commit detail
        let commit_block = Block::default()
            .title(" Commit Detail ")
            .borders(Borders::ALL)
            .border_style(self.theme.border_style(self.is_focused))
            .border_type(self.theme.border_type(self.is_focused));

        // Scroll is already clamped in new()
        let commit_paragraph = Paragraph::new(self.commit_lines)
            .block(commit_block)
            .scroll((self.commit_scroll, 0))
            .wrap(Wrap { trim: false });

        Widget::render(commit_paragraph, chunks[1], buf);
    }
}
