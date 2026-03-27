//! Commit detail widget

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::app::{App, FocusedPanel};
use crate::git::{CommitDiffInfo, FileChangeKind};

use super::{render_placeholder_block, MIN_WIDGET_HEIGHT, MIN_WIDGET_WIDTH};

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
}

impl<'a> CommitDetailWidget<'a> {
    pub fn new(app: &mut App, detail_area: Rect) -> Self {
        let detail_width = detail_area.width;
        let commit_lines = Self::build_commit_lines(app);

        // Compute the commit pane inner dimensions for scroll calculation.
        // The commit pane is the right half (or bottom half on narrow terminals).
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
        // Use Ratatui's own line_count to match its actual wrapping behaviour.
        let commit_wrapped_total = if commit_inner_width > 0 {
            let p = Paragraph::new(commit_lines.clone()).wrap(Wrap { trim: false });
            p.line_count(commit_inner_width as u16)
        } else {
            commit_lines.len()
        };
        app.commit_detail_max_scroll =
            commit_wrapped_total.saturating_sub(commit_visible) as u16;
        // Clamp current scroll to the newly computed max
        app.commit_detail_scroll = app
            .commit_detail_scroll
            .min(app.commit_detail_max_scroll);

        // Files pane gets ~half the detail width minus borders
        let files_inner_width = if detail_width <= VERTICAL_LAYOUT_THRESHOLD {
            detail_width.saturating_sub(2) as usize
        } else {
            (detail_width / 2).saturating_sub(2) as usize
        };
        let (file_lines, selected_line_index) = Self::build_file_lines(app, files_inner_width);

        Self {
            commit_lines,
            file_lines,
            selected_line_index,
            is_focused: app.focused_panel == FocusedPanel::CommitDetail,
            is_files_focused: app.focused_panel == FocusedPanel::Files,
            files_filter_active: app.files_filter_active,
            commit_scroll: app.commit_detail_scroll,
            files_title: {
                let mut spans = vec![Span::raw(" Changed Files")];
                if app.files_group_by_folder {
                    spans.push(Span::styled(
                        " [folders]",
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                if app.files_filter_active {
                    spans.push(Span::styled(
                        format!(" filter: {}\u{2588}", app.files_filter),
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else if !app.files_filter.is_empty() {
                    spans.push(Span::styled(
                        format!(" [{}]", app.files_filter),
                        Style::default().fg(Color::Cyan),
                    ));
                }
                spans.push(Span::raw(" "));
                Line::from(spans)
            },
        }
    }

    /// Returns (lines, selected_line_index)
    fn build_file_lines(app: &App, max_width: usize) -> (Vec<Line<'a>>, u16) {
        use crate::app::FilesPaneItem;

        let selected_file_index = if app.focused_panel == FocusedPanel::Files {
            Some(app.file_selected_index)
        } else {
            None
        };

        let line_stats_loading = app.is_line_stats_loading();

        let items = app.files_pane_items();
        if items.is_empty() {
            if app.is_diff_loading() {
                return (vec![Line::from(Span::styled(
                    "Loading...",
                    Style::default().fg(Color::DarkGray),
                ))], 0);
            }
            if let Some(diff) = app.cached_diff_or_quick() {
                return (Self::build_file_list_lines_from(Some(diff), selected_file_index, line_stats_loading), 0);
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
                    Span::styled("  ...", Style::default().fg(Color::DarkGray)),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{} files changed", diff.total_files),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(format!("+{}", diff.total_insertions), Style::default().fg(Color::Green)),
                    Span::raw(" "),
                    Span::styled(format!("-{}", diff.total_deletions), Style::default().fg(Color::Red)),
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
                            .fg(Color::Yellow)
                            .bg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                            .fg(Color::Yellow)
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
                    let (indicator, color) = match file.kind {
                        FileChangeKind::Added => ("A", Color::Green),
                        FileChangeKind::Modified => ("M", Color::Yellow),
                        FileChangeKind::Deleted => ("D", Color::Red),
                        FileChangeKind::Renamed => ("R", Color::Cyan),
                        FileChangeKind::Copied => ("C", Color::Cyan),
                    };

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
                        Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };

                    if avail == 0 || full_text.len() <= avail {
                        // Fits on one line
                        let mut spans = vec![
                            Span::styled(format!("{} ", indicator), Style::default().fg(color)),
                        ];
                        spans.push(Span::raw(path_str));
                        if file.is_binary {
                            spans.push(Span::styled(" (binary)", Style::default().fg(Color::DarkGray)));
                        } else if line_stats_loading {
                            spans.push(Span::styled(" ...", Style::default().fg(Color::DarkGray)));
                        } else if file.insertions > 0 || file.deletions > 0 {
                            spans.push(Span::raw(" "));
                            spans.push(Span::styled(format!("+{}", file.insertions), Style::default().fg(Color::Green)));
                            spans.push(Span::raw(" "));
                            spans.push(Span::styled(format!("-{}", file.deletions), Style::default().fg(Color::Red)));
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

    fn build_commit_lines(app: &App) -> Vec<Line<'a>> {
        let Some(selected) = app.graph_list_state.selected() else {
            return vec![Line::from(Span::styled(
                "Select a commit",
                Style::default().fg(Color::DarkGray),
            ))];
        };

        let Some(node) = app.graph_layout.nodes.get(selected) else {
            return Vec::new();
        };

        // Handle uncommitted changes node
        if node.is_uncommitted {
            let mut lines = vec![
                Line::from(Span::styled(
                    "Uncommitted Changes",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ];

            if app.editing_commit_message {
                lines.push(Line::from(Span::styled(
                    "Commit Message (Alt+Enter to commit, Esc to stop):",
                    Style::default().fg(Color::Cyan),
                )));
            } else if !app.commit_editor.text.is_empty() {
                lines.push(Line::from(Span::styled(
                    "Commit Message (Enter to edit):",
                    Style::default().fg(Color::DarkGray),
                )));
            } else if app.focused_panel == FocusedPanel::CommitDetail {
                lines.push(Line::from(Span::styled(
                    "Press Enter to type a commit message",
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    match node.uncommitted_count {
                        Some(count) => format!("{} files with changes", count),
                        None => "files with changes".to_string(),
                    },
                    Style::default().fg(Color::DarkGray),
                )));
            }

            // Show editor content
            if app.editing_commit_message || !app.commit_editor.text.is_empty() {
                lines.push(Line::from(""));
                for line_text in app.commit_editor.lines() {
                    lines.push(Line::from(Span::raw(line_text.to_string())));
                }
            }

            return lines;
        }

        // Handle connector rows (no commit)
        let Some(commit) = &node.commit else {
            return vec![Line::from(Span::styled(
                "(connector line)",
                Style::default().fg(Color::DarkGray),
            ))];
        };

        // Build commit detail lines
        let mut lines = vec![
            // Author
            Line::from(vec![
                Span::styled("Author: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("{} <{}>", commit.author_name, commit.author_email),
                    Style::default().fg(Color::Blue),
                ),
            ]),
            // Date
            Line::from(vec![
                Span::styled("Date:   ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    commit.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
        ];

        lines.push(Line::from(""));

        // Message
        for line in commit.full_message.lines() {
            lines.push(Line::from(Span::raw(line.to_string())));
        }

        lines
    }

    fn build_file_list_lines_from(
        diff: Option<&CommitDiffInfo>,
        selected_file_index: Option<usize>,
        line_stats_loading: bool,
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
                Span::styled("  ...", Style::default().fg(Color::DarkGray)),
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
                    Style::default().fg(Color::Green),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("-{}", diff.total_deletions),
                    Style::default().fg(Color::Red),
                ),
            ]));
        }
        lines.push(Line::from(""));

        // File list
        for (idx, file) in diff.files.iter().enumerate() {
            let is_selected = selected_file_index == Some(idx);

            let (indicator, color) = match file.kind {
                FileChangeKind::Added => ("A", Color::Green),
                FileChangeKind::Modified => ("M", Color::Yellow),
                FileChangeKind::Deleted => ("D", Color::Red),
                FileChangeKind::Renamed => ("R", Color::Cyan),
                FileChangeKind::Copied => ("C", Color::Cyan),
            };

            let path_str = file.path.to_string_lossy().to_string();

            let mut spans = vec![
                Span::styled(format!("{} ", indicator), Style::default().fg(color)),
                Span::raw(path_str),
            ];

            if file.is_binary {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    "(binary)",
                    Style::default().fg(Color::DarkGray),
                ));
            } else if line_stats_loading {
                spans.push(Span::styled(
                    " ...",
                    Style::default().fg(Color::DarkGray),
                ));
            } else if file.insertions > 0 || file.deletions > 0 {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!("+{}", file.insertions),
                    Style::default().fg(Color::Green),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!("-{}", file.deletions),
                    Style::default().fg(Color::Red),
                ));
            }

            let mut line = Line::from(spans);
            if is_selected {
                line = line.style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                );
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
                Style::default().fg(Color::DarkGray),
            )));
        }

        lines
    }
}

impl<'a> Widget for CommitDetailWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < MIN_WIDGET_WIDTH || area.height < MIN_WIDGET_HEIGHT {
            render_placeholder_block(area, buf);
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
            Color::Yellow
        } else if self.is_files_focused {
            Color::Green
        } else {
            Color::DarkGray
        };
        let files_block = Block::default()
            .title(self.files_title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(files_border));

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
        let commit_border = if self.is_focused {
            Color::Green
        } else {
            Color::DarkGray
        };
        let commit_block = Block::default()
            .title(" Commit Detail ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(commit_border));

        // Scroll is already clamped in new()
        let commit_paragraph = Paragraph::new(self.commit_lines)
            .block(commit_block)
            .scroll((self.commit_scroll, 0))
            .wrap(Wrap { trim: false });

        Widget::render(commit_paragraph, chunks[1], buf);
    }
}
