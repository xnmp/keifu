//! Commit detail widget

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::app::{App, FocusedPanel};

use super::{render_placeholder_block, theme::Theme, MIN_WIDGET_HEIGHT, MIN_WIDGET_WIDTH};

pub struct CommitDetailWidget<'a> {
    commit_lines: Vec<Line<'a>>,
    is_focused: bool,
    commit_scroll: u16,
    theme: &'a Theme,
}

/// Pre-render layout metrics for the commit detail panel.
/// Call this before constructing `CommitDetailWidget` to update App scroll state.
/// `commit_area` is the area for the commit detail block only (not the files pane).
/// Returns the built lines so the caller can hand them to `CommitDetailWidget::new`
/// without rebuilding them.
pub fn compute_commit_detail_layout<'a>(
    app: &mut App,
    commit_area: Rect,
    theme: &'a Theme,
) -> Vec<Line<'a>> {
    let (commit_lines, raw_editor_offset) =
        CommitDetailWidget::build_commit_lines_with_offset(app, theme);

    if let Some(offset) = raw_editor_offset {
        app.commit_editor_line_offset = offset;
    }

    let commit_inner_width = commit_area.width.saturating_sub(2) as usize;
    let commit_visible = commit_area.height.saturating_sub(2) as usize;
    app.commit_detail_visible_rows = commit_visible as u16;

    // Use Ratatui's own line_count to match its actual wrapping behaviour.
    // Word-wrap never splits a line whose total width already fits within
    // the available width, so when every line fits, the wrapped count is
    // just the line count — skip building (and cloning into) a Paragraph.
    let commit_wrapped_total = if commit_inner_width == 0
        || commit_lines.iter().all(|l| l.width() <= commit_inner_width)
    {
        commit_lines.len()
    } else {
        let p = Paragraph::new(commit_lines.clone()).wrap(Wrap { trim: false });
        p.line_count(commit_inner_width as u16)
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

    commit_lines
}

impl<'a> CommitDetailWidget<'a> {
    pub fn new(app: &App, _commit_area: Rect, theme: &'a Theme, commit_lines: Vec<Line<'a>>) -> Self {
        Self {
            commit_lines,
            is_focused: app.focused_panel == FocusedPanel::CommitDetail,
            commit_scroll: app.commit_detail_scroll,
            theme,
        }
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

}

impl<'a> Widget for CommitDetailWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < MIN_WIDGET_WIDTH || area.height < MIN_WIDGET_HEIGHT {
            render_placeholder_block(area, buf, self.theme);
            return;
        }

        let commit_block = Block::default()
            .title(" Commit Detail ")
            .borders(Borders::ALL)
            .border_style(self.theme.border_style(self.is_focused))
            .border_type(self.theme.border_type(self.is_focused));

        let commit_paragraph = Paragraph::new(self.commit_lines)
            .block(commit_block)
            .scroll((self.commit_scroll, 0))
            .wrap(Wrap { trim: false });

        Widget::render(commit_paragraph, area, buf);
    }
}
