//! Input, confirmation, and picker dialog widgets

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Widget},
};

use crate::app::FileHistoryEntry;

use super::theme::Theme;

/// Input dialog
pub struct InputDialog<'a> {
    title: &'a str,
    input: &'a str,
    theme: &'a Theme,
    /// When true, the input is rendered as bullets (password/token entry).
    mask: bool,
}

impl<'a> InputDialog<'a> {
    pub fn new(title: &'a str, input: &'a str, theme: &'a Theme) -> Self {
        Self { title, input, theme, mask: false }
    }

    /// Render the input masked (bullets) — for password/token entry.
    pub fn masked(title: &'a str, input: &'a str, theme: &'a Theme) -> Self {
        Self { title, input, theme, mask: true }
    }
}

impl<'a> Widget for InputDialog<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let block = self
            .theme
            .popup_block_in(format!(" {} ", self.title), self.theme.input_border);

        let input_style = Style::default()
            .fg(self.theme.text_primary)
            .add_modifier(Modifier::UNDERLINED);

        // Mask the secret: one bullet per character, never the text itself.
        let shown = if self.mask {
            "\u{2022}".repeat(self.input.chars().count())
        } else {
            self.input.to_string()
        };

        let hint_style = Style::default().fg(self.theme.text_muted);
        let lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(shown, input_style),
                Span::styled("_", Style::default().fg(self.theme.search_cursor)),
            ]),
            Line::from(""),
            Line::from(Span::styled("  Enter: confirm  Esc: cancel", hint_style)),
        ];

        let paragraph = Paragraph::new(lines).block(block);
        Widget::render(paragraph, area, buf);
    }
}

/// Confirmation dialog
pub struct ConfirmDialog<'a> {
    message: &'a str,
    theme: &'a Theme,
}

impl<'a> ConfirmDialog<'a> {
    pub fn new(message: &'a str, theme: &'a Theme) -> Self {
        Self { message, theme }
    }
}

impl<'a> Widget for ConfirmDialog<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let block = self
            .theme
            .popup_block_in(" Confirm ", self.theme.confirm_border);

        // The first line of the message is the prompt (primary color); any
        // following `\n`-separated lines are hint/advertisement text (muted),
        // matching the muted-hint style used elsewhere in these dialogs.
        let mut lines = vec![Line::from("")];
        for (i, segment) in self.message.split('\n').enumerate() {
            let style = if i == 0 {
                Style::default().fg(self.theme.text_primary)
            } else {
                Style::default().fg(self.theme.text_muted)
            };
            lines.push(Line::from(Span::styled(format!("  {segment}"), style)));
        }
        lines.extend([
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "  y/Enter",
                    Style::default()
                        .fg(self.theme.button_yes)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(": Yes  "),
                Span::styled(
                    "n/Esc",
                    Style::default().fg(self.theme.button_no).add_modifier(Modifier::BOLD),
                ),
                Span::raw(": No"),
            ]),
        ]);

        let paragraph = Paragraph::new(lines).block(block);
        Widget::render(paragraph, area, buf);
    }
}

/// Divergence prompt shown when a `--ff-only` pull fails: pick merge or rebase.
pub struct PullDivergenceDialog<'a> {
    selected: usize,
    theme: &'a Theme,
}

impl<'a> PullDivergenceDialog<'a> {
    /// Menu order matches the handler: 0 = Merge, 1 = Rebase.
    pub const OPTIONS: [&'static str; 2] = ["Merge (create a merge commit)", "Rebase (replay your commits)"];

    pub fn new(selected: usize, theme: &'a Theme) -> Self {
        Self { selected, theme }
    }
}

impl<'a> Widget for PullDivergenceDialog<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let block = self
            .theme
            .popup_block_in(" Branches Diverged ", self.theme.confirm_border);

        let mut lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "  Can't fast-forward. Reconcile with:",
                Style::default().fg(self.theme.text_primary),
            )),
            Line::from(""),
        ];
        for (i, opt) in Self::OPTIONS.iter().enumerate() {
            let selected = i == self.selected;
            let style = if selected {
                self.theme.list_selection_style()
            } else {
                Style::default().fg(self.theme.text_primary)
            };
            let prefix = if selected { "> " } else { "  " };
            lines.push(Line::from(Span::styled(format!("{prefix}{opt}"), style)));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  ↑↓ move   Enter choose   Esc cancel",
            Style::default().fg(self.theme.text_muted),
        )));

        let paragraph = Paragraph::new(lines).block(block);
        Widget::render(paragraph, area, buf);
    }
}

/// A small titled options picker (merge method, review disposition, …).
pub struct OptionsDialog<'a> {
    title: &'a str,
    prompt: &'a str,
    options: &'a [&'a str],
    selected: usize,
    theme: &'a Theme,
}

impl<'a> OptionsDialog<'a> {
    pub fn new(
        title: &'a str,
        prompt: &'a str,
        options: &'a [&'a str],
        selected: usize,
        theme: &'a Theme,
    ) -> Self {
        Self {
            title,
            prompt,
            options,
            selected,
            theme,
        }
    }
}

impl<'a> Widget for OptionsDialog<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = self
            .theme
            .popup_block_in(format!(" {} ", self.title), self.theme.confirm_border);

        let mut lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {}", self.prompt),
                Style::default().fg(self.theme.text_primary),
            )),
            Line::from(""),
        ];
        for (i, opt) in self.options.iter().enumerate() {
            let selected = i == self.selected;
            let style = if selected {
                self.theme.list_selection_style()
            } else {
                Style::default().fg(self.theme.text_primary)
            };
            let prefix = if selected { "> " } else { "  " };
            lines.push(Line::from(Span::styled(format!("{prefix}{opt}"), style)));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  ↑↓ move   Enter choose   Esc cancel",
            Style::default().fg(self.theme.text_muted),
        )));

        Widget::render(Paragraph::new(lines).block(block), area, buf);
    }
}

/// Branch picker dialog (shown when selecting from multiple branches on a commit)
pub struct BranchPickerWidget<'a> {
    branches: &'a [String],
    selected: usize,
    theme: &'a Theme,
    title: &'a str,
}

impl<'a> BranchPickerWidget<'a> {
    pub fn new(branches: &'a [String], selected: usize, theme: &'a Theme) -> Self {
        Self {
            branches,
            selected,
            theme,
            title: " Checkout Branch ",
        }
    }

    pub fn with_title(
        branches: &'a [String],
        selected: usize,
        theme: &'a Theme,
        title: &'a str,
    ) -> Self {
        Self {
            branches,
            selected,
            theme,
            title,
        }
    }
}

impl<'a> Widget for BranchPickerWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let block = self.theme.popup_block(self.title);

        let inner = block.inner(area);
        block.render(area, buf);

        for (i, branch) in self.branches.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }

            let y = inner.y + i as u16;
            let is_selected = i == self.selected;
            let style = if is_selected {
                self.theme.list_selection_style()
            } else {
                Style::default().fg(self.theme.text_primary)
            };

            let prefix = if is_selected { "> " } else { "  " };
            let max_width = inner.width as usize;
            let display = if branch.len() + 2 > max_width {
                format!("{}{}...", prefix, &branch[..max_width.saturating_sub(5)])
            } else {
                format!("{}{}", prefix, branch)
            };

            buf.set_string(inner.x, y, &display, style);
        }
    }
}

/// Per-file history picker: commits that touched a path.
pub struct FileHistoryWidget<'a> {
    entries: &'a [FileHistoryEntry],
    selected: usize,
    theme: &'a Theme,
    title: String,
}

impl<'a> FileHistoryWidget<'a> {
    pub fn new(
        entries: &'a [FileHistoryEntry],
        selected: usize,
        theme: &'a Theme,
        path: &std::path::Path,
    ) -> Self {
        Self {
            entries,
            selected,
            theme,
            title: format!(" History: {} ", path.display()),
        }
    }
}

impl<'a> Widget for FileHistoryWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let block = self.theme.popup_block(self.title.clone());

        let inner = block.inner(area);
        block.render(area, buf);

        // Keep the selected row visible with a simple windowed scroll.
        let height = inner.height as usize;
        let first = if height == 0 {
            0
        } else {
            self.selected.saturating_sub(height.saturating_sub(1))
        };

        for (row, entry) in self.entries.iter().enumerate().skip(first).take(height) {
            let y = inner.y + (row - first) as u16;
            let is_selected = row == self.selected;
            let base_style = if is_selected {
                self.theme.list_selection_style()
            } else {
                Style::default().fg(self.theme.text_primary)
            };

            let prefix = if is_selected { "> " } else { "  " };
            let line = format!(
                "{}{}  {}  {}",
                prefix, entry.short_id, entry.date, entry.subject
            );
            let max_width = inner.width as usize;
            let truncated: String = line.chars().take(max_width).collect();
            buf.set_string(inner.x, y, &truncated, base_style);
        }
    }
}

