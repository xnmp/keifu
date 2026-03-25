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
    files_title: String,
}

impl<'a> CommitDetailWidget<'a> {
    pub fn new(app: &App) -> Self {
        let commit_lines = Self::build_commit_lines(app);
        let file_lines = Self::build_file_lines(app);
        // Find the rendered line index for the selected file
        let mut selected_line_index = 0u16;
        let mut file_idx = 0usize;
        for (line_idx, item) in app.files_pane_items().iter().enumerate() {
            if matches!(item, crate::app::FilesPaneItem::File(_)) {
                if file_idx == app.file_selected_index {
                    // +2 for the summary header and blank line
                    selected_line_index = (line_idx + 2) as u16;
                    break;
                }
                file_idx += 1;
            }
        }

        Self {
            commit_lines,
            file_lines,
            selected_line_index,
            is_focused: app.focused_panel == FocusedPanel::CommitDetail,
            is_files_focused: app.focused_panel == FocusedPanel::Files,
            files_title: {
                let mut title = String::from(" Changed Files");
                if app.files_group_by_folder {
                    title.push_str(" [folders]");
                }
                if !app.files_filter.is_empty() {
                    title.push_str(&format!(" [{}]", app.files_filter));
                }
                title.push(' ');
                title
            },
        }
    }

    fn build_file_lines(app: &App) -> Vec<Line<'a>> {
        use crate::app::FilesPaneItem;

        let selected_file_index = if app.focused_panel == FocusedPanel::Files {
            Some(app.file_selected_index)
        } else {
            None
        };

        let line_stats_loading = app.is_line_stats_loading();

        // Use files_pane_items which respects folder grouping and fuzzy filter
        let items = app.files_pane_items();
        if items.is_empty() {
            if app.is_diff_loading() {
                return vec![Line::from(Span::styled(
                    "Loading...",
                    Style::default().fg(Color::DarkGray),
                ))];
            }
            // Show summary from diff if available
            if let Some(diff) = app.cached_diff_or_quick() {
                return Self::build_file_list_lines_from(Some(diff), selected_file_index, line_stats_loading);
            }
            return Vec::new();
        }

        let mut lines = Vec::new();

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

        // Track file index for selection highlighting
        let mut file_idx = 0;
        for item in &items {
            match item {
                FilesPaneItem::Header(text) => {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", text),
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )));
                }
                FilesPaneItem::File(file) => {
                    let is_selected = selected_file_index == Some(file_idx);
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
                        spans.push(Span::styled("(binary)", Style::default().fg(Color::DarkGray)));
                    } else if line_stats_loading {
                        spans.push(Span::styled(" ...", Style::default().fg(Color::DarkGray)));
                    } else if file.insertions > 0 || file.deletions > 0 {
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(format!("+{}", file.insertions), Style::default().fg(Color::Green)));
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(format!("-{}", file.deletions), Style::default().fg(Color::Red)));
                    }

                    let mut line = Line::from(spans);
                    if is_selected {
                        line = line.style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
                    }
                    lines.push(line);
                    file_idx += 1;
                }
            }
        }

        lines
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
            // Commit hash
            Line::from(vec![
                Span::styled("Commit: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(commit.oid.to_string(), Style::default().fg(Color::Yellow)),
            ]),
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

        // Parent commits
        if !commit.parent_oids.is_empty() {
            let parents: Vec<String> = commit
                .parent_oids
                .iter()
                .map(|oid| oid.to_string()[..7].to_string())
                .collect();
            lines.push(Line::from(vec![
                Span::styled("Parent: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(parents.join(", "), Style::default().fg(Color::DarkGray)),
            ]));
        }

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
        let files_border = if self.is_files_focused {
            Color::Green
        } else {
            Color::DarkGray
        };
        let files_block = Block::default()
            .title(self.files_title.as_str())
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
            .wrap(Wrap { trim: false })
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

        let commit_paragraph = Paragraph::new(self.commit_lines)
            .block(commit_block)
            .wrap(Wrap { trim: false });

        Widget::render(commit_paragraph, chunks[1], buf);
    }
}
