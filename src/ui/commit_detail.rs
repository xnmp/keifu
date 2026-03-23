//! Commit detail widget

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::app::{App, AppMode};
use crate::git::{CommitDiffInfo, FileChangeKind};

use super::{render_placeholder_block, MIN_WIDGET_HEIGHT, MIN_WIDGET_WIDTH};

/// Width threshold for switching to vertical layout
/// When panel width would be <= 28 chars, use vertical layout
const VERTICAL_LAYOUT_THRESHOLD: u16 = 56;

pub struct CommitDetailWidget<'a> {
    app: &'a mut App,
    commit_lines: Vec<Line<'a>>,
    file_lines: Vec<Line<'a>>,
}

impl<'a> CommitDetailWidget<'a> {
    pub fn new(app: &'a mut App) -> Self {
        let commit_lines = Self::build_commit_lines(app);
        let file_lines = Self::build_file_lines(app);
        Self {
            app,
            commit_lines,
            file_lines,
        }
    }

    fn build_file_lines(app: &mut App) -> Vec<Line<'a>> {
        let show_staged_sections = app
            .graph_list_state
            .selected()
            .and_then(|idx| app.graph_layout.nodes.get(idx))
            .is_some_and(|node| node.is_uncommitted);

        let selected = if matches!(app.mode, crate::app::AppMode::Files) {
            app.files_pane.list_state.selected()
        } else {
            None
        };
        // Prefer cached data (even if stale) over a loading indicator so that
        // auto-refresh doesn't cause the file list to flicker.
        if let Some(diff) = app.cached_diff() {
            return Self::build_file_list_lines_from(Some(diff), selected, show_staged_sections);
        }

        // Fast path: for commit diffs, show an instant file list (no +/- stats)
        // from a cached git CLI query while the full diff loads.
        if !show_staged_sections {
            if let Some(entries) = app.cached_commit_files() {
                return Self::build_file_list_lines_from_commit_paths(entries, selected);
            }
        }

        // Fast path: for uncommitted changes, show an instant file list from
        // the working tree status while the full diff (with +/- stats) loads.
        if show_staged_sections {
            if let Some(status) = app.working_tree_status() {
                return Self::build_file_list_lines_from_worktree_status(status, selected);
            }
        }

        if app.is_diff_loading() {
            return vec![Line::from(Span::styled(
                "Loading...",
                Style::default().fg(Color::DarkGray),
            ))];
        }
        Self::build_file_list_lines_from(None, selected, show_staged_sections)
    }

    fn build_file_list_lines_from_commit_paths(
        entries: &[(crate::git::FileChangeKind, std::path::PathBuf)],
        selected: Option<usize>,
    ) -> Vec<Line<'a>> {
        let mut lines = Vec::new();

        lines.push(Line::from(vec![
            Span::styled(
                format!("{} files changed", entries.len()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("(stats loading…)", Style::default().fg(Color::DarkGray)),
        ]));
        lines.push(Line::from(""));

        for (idx, (kind, path)) in entries.iter().enumerate() {
            let is_selected = selected == Some(idx);
            let row_style = if is_selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            let path_str = path.to_string_lossy().to_string();

            let (indicator, color) = match kind {
                crate::git::FileChangeKind::Added => ("A", Color::Green),
                crate::git::FileChangeKind::Modified => ("M", Color::Yellow),
                crate::git::FileChangeKind::Deleted => ("D", Color::Red),
                crate::git::FileChangeKind::Renamed => ("R", Color::Cyan),
                crate::git::FileChangeKind::Copied => ("C", Color::Cyan),
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", indicator), row_style.fg(color)),
                Span::styled(path_str, row_style),
            ]));
        }

        lines
    }

    fn build_file_list_lines_from_worktree_status(
        status: &crate::git::WorkingTreeStatus,
        selected: Option<usize>,
    ) -> Vec<Line<'a>> {
        let mut lines = Vec::new();

        lines.push(Line::from(vec![
            Span::styled(
                format!("{} files changed", status.file_count()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("(stats loading…)", Style::default().fg(Color::DarkGray)),
        ]));
        lines.push(Line::from(""));

        // We don't yet have per-file staged status here; the grouping still
        // renders (headers) but files will be shown under Unstaged for now.
        // Once the full diff loads, staging grouping and +/- stats will appear.
        lines.push(Line::from(Span::styled(
            "Unstaged",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));

        for (idx, path) in status.file_paths.iter().enumerate() {
            let is_selected = selected == Some(idx);
            let row_style = if is_selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            let path_str = path.to_string_lossy().to_string();
            lines.push(Line::from(vec![
                Span::styled(" ? ".to_string(), row_style.fg(Color::DarkGray)),
                Span::styled(path_str, row_style),
            ]));
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
            return vec![
                Line::from(Span::styled(
                    "Uncommitted Changes",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    match node.uncommitted_count {
                        Some(count) => format!("{} files with changes", count),
                        None => "files with changes".to_string(),
                    },
                    Style::default().fg(Color::DarkGray),
                )),
            ];
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
        selected: Option<usize>,
        show_staged_sections: bool,
    ) -> Vec<Line<'a>> {
        let mut lines = Vec::new();

        let Some(diff) = diff else {
            return lines;
        };

        // Header row
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
        lines.push(Line::from(""));

        // File list
        // (Selection highlighting is done here; scrolling is handled by the widget via Paragraph::scroll.)

        if show_staged_sections {
            let staged: Vec<(usize, &crate::git::FileDiffInfo)> = diff
                .files
                .iter()
                .enumerate()
                .filter(|(_, f)| f.is_staged)
                .collect();
            let unstaged: Vec<(usize, &crate::git::FileDiffInfo)> = diff
                .files
                .iter()
                .enumerate()
                .filter(|(_, f)| !f.is_staged)
                .collect();

            if !staged.is_empty() {
                lines.push(Line::from(Span::styled(
                    "Staged",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )));
                for (idx, file) in staged {
                    Self::push_file_line(&mut lines, idx, file, selected);
                }
            }

            if !unstaged.is_empty() {
                lines.push(Line::from(Span::styled(
                    "Unstaged",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )));
                for (idx, file) in unstaged {
                    Self::push_file_line(&mut lines, idx, file, selected);
                }
            }
        } else {
            for (idx, file) in diff.files.iter().enumerate() {
                Self::push_file_line(&mut lines, idx, file, selected);
            }
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

    fn push_file_line(
        lines: &mut Vec<Line<'a>>,
        idx: usize,
        file: &crate::git::FileDiffInfo,
        selected: Option<usize>,
    ) {
        let (indicator, color) = match file.kind {
            FileChangeKind::Added => ("A", Color::Green),
            FileChangeKind::Modified => ("M", Color::Yellow),
            FileChangeKind::Deleted => ("D", Color::Red),
            FileChangeKind::Renamed => ("R", Color::Cyan),
            FileChangeKind::Copied => ("C", Color::Cyan),
        };

        let path_str = file.path.to_string_lossy().to_string();

        let is_selected = selected == Some(idx);
        let row_style = if is_selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };

        let path_style = if file.is_staged {
            row_style.fg(Color::Green)
        } else {
            row_style
        };

        let mut spans = vec![
            Span::styled(format!(" {} ", indicator), row_style.fg(color)),
            Span::styled(path_str, path_style),
        ];

        if file.is_binary {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                "(binary)",
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

        lines.push(Line::from(spans));
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

        let left_border = if matches!(self.app.mode, AppMode::Files) {
            Color::Green
        } else {
            Color::DarkGray
        };
        let right_border = if matches!(self.app.mode, AppMode::Detail) {
            Color::Green
        } else {
            Color::DarkGray
        };

        // Left: file list
        let left_block = Block::default()
            .title(" Changed Files ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(left_border));

        let mut left_paragraph = Paragraph::new(self.file_lines)
            .block(left_block)
            .wrap(Wrap { trim: false });

        // Scroll the file list so the selected entry stays visible.
        // Approximate: header (2 lines) + selected index.
        if matches!(self.app.mode, AppMode::Files) {
            if let Some(selected) = self.app.files_pane.list_state.selected() {
                let inner_height = chunks[0].height.saturating_sub(2) as usize; // borders
                if inner_height > 0 {
                    let mut line_idx = 2usize + selected;

                    // When the uncommitted node is selected, we render additional
                    // "Staged"/"Unstaged" section headers above the file entries.
                    let show_staged_sections = self
                        .app
                        .graph_list_state
                        .selected()
                        .and_then(|idx| self.app.graph_layout.nodes.get(idx))
                        .is_some_and(|node| node.is_uncommitted);
                    if show_staged_sections {
                        if let Some(diff) = self.app.cached_diff() {
                            let has_staged = diff.files.iter().any(|f| f.is_staged);
                            let has_unstaged = diff.files.iter().any(|f| !f.is_staged);
                            if has_staged {
                                line_idx += 1;
                            }
                            if has_unstaged {
                                // If the selected file is in the unstaged section, add the
                                // "Unstaged" header line as well.
                                if let Some(file) = diff.files.get(selected) {
                                    if !file.is_staged {
                                        line_idx += 1;
                                    }
                                }
                            }
                        }
                    }

                    // Keep selection within viewport.
                    let scroll = line_idx.saturating_sub(inner_height.saturating_sub(1));
                    left_paragraph = left_paragraph.scroll((scroll as u16, 0));
                }
            }
        }

        Widget::render(left_paragraph, chunks[0], buf);

        // Right: commit info
        let right_block = Block::default()
            .title(" Commit Detail ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(right_border));

        let right_paragraph = Paragraph::new(self.commit_lines)
            .block(right_block)
            .wrap(Wrap { trim: false });

        Widget::render(right_paragraph, chunks[1], buf);
    }
}
