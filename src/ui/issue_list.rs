//! GitHub issue list popup widget.
//!
//! Renders `IssueListView` as a scrollable list: state icon, number, title,
//! label chips (colored from each label's hex color when parseable), assignees,
//! and a relative-time suffix. Loading/empty/error states render inline so the
//! popup opens instantly and fills in asynchronously (never `AppMode::Error`).

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Widget},
};

use super::theme::Theme;
use crate::app::{IssueListState, IssueListView};
use crate::issue::{IssueInfo, IssueState};

pub struct IssueListWidget<'a> {
    view: &'a IssueListView,
    theme: &'a Theme,
}

impl<'a> IssueListWidget<'a> {
    pub fn new(view: &'a IssueListView, theme: &'a Theme) -> Self {
        Self { view, theme }
    }
}

impl<'a> Widget for IssueListWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let title = format!(" Issues · {} ", self.view.filter.label());
        let block = self.theme.popup_block(title);
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 {
            return;
        }
        // Reserve the last row for the footer hint.
        let body_h = inner.height.saturating_sub(1) as usize;

        match &self.view.state {
            IssueListState::Loading => {
                self.placeholder(inner, buf, "  loading issues…", self.theme.text_muted);
            }
            IssueListState::Error(e) => {
                self.placeholder(inner, buf, &format!("  {e}"), self.theme.status_error_fg);
            }
            IssueListState::Ready(issues) if issues.is_empty() => {
                self.placeholder(inner, buf, "  no issues", self.theme.text_muted);
            }
            IssueListState::Ready(issues) => {
                // Window the list so the selection stays visible.
                let first = self.view.selected.saturating_sub(body_h.saturating_sub(1));
                for (row, issue) in issues.iter().enumerate().skip(first).take(body_h) {
                    let y = inner.y + (row - first) as u16;
                    let selected = row == self.view.selected;
                    self.render_row(issue, inner.x, y, inner.width, selected, buf);
                }
            }
        }

        self.footer(
            inner,
            buf,
            " ↑↓ move   Enter open   Tab filter   n new   r refresh   o browser   Esc close",
        );
    }
}

impl<'a> IssueListWidget<'a> {
    fn placeholder(&self, inner: Rect, buf: &mut Buffer, text: &str, color: Color) {
        buf.set_string(
            inner.x,
            inner.y,
            truncate(text, inner.width as usize),
            Style::default().fg(color),
        );
    }

    fn footer(&self, inner: Rect, buf: &mut Buffer, text: &str) {
        let y = inner.y + inner.height - 1;
        buf.set_string(
            inner.x,
            y,
            truncate(text, inner.width as usize),
            Style::default().fg(self.theme.text_muted),
        );
    }

    fn render_row(
        &self,
        issue: &IssueInfo,
        x: u16,
        y: u16,
        width: u16,
        selected: bool,
        buf: &mut Buffer,
    ) {
        let base = if selected {
            self.theme.list_selection_style()
        } else {
            Style::default().fg(self.theme.text_primary)
        };
        let muted = base.patch(Style::default().fg(self.theme.text_muted));

        let (icon_color, icon) = match issue.state {
            IssueState::Open => (self.theme.pr_ci_pass, "●"),
            IssueState::Closed => (self.theme.text_muted, "○"),
        };

        let mut spans = vec![
            Span::styled(if selected { "> " } else { "  " }, base),
            Span::styled(
                format!("{icon} "),
                base.patch(Style::default().fg(icon_color))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("#{} ", issue.number), muted),
            Span::styled(issue.title.clone(), base),
        ];

        // Label chips.
        for label in &issue.labels {
            let color = hex_to_color(&label.color).unwrap_or(self.theme.pr_badge);
            spans.push(Span::styled(" ", base));
            spans.push(Span::styled(
                format!("[{}]", label.name),
                base.patch(Style::default().fg(color)),
            ));
        }

        // Assignees.
        if !issue.assignees.is_empty() {
            let who: Vec<String> = issue.assignees.iter().map(|a| format!("@{a}")).collect();
            spans.push(Span::styled(format!("  {}", who.join(" ")), muted));
        }

        let line = Line::from(spans);
        buf.set_line(x, y, &line, width);
        if selected {
            buf.set_style(Rect::new(x, y, width, 1), base);
        }
    }
}

/// Parse a 6-digit hex color (as gh emits it, no leading `#`) into an RGB color.
/// Returns `None` for anything that isn't exactly six hex digits.
fn hex_to_color(hex: &str) -> Option<Color> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

/// Truncate a string to `max` display columns (char-count approximation).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_to_color_parses_valid_and_rejects_invalid() {
        assert_eq!(hex_to_color("d73a4a"), Some(Color::Rgb(0xd7, 0x3a, 0x4a)));
        assert_eq!(hex_to_color("#a2eeef"), Some(Color::Rgb(0xa2, 0xee, 0xef)));
        assert_eq!(hex_to_color("000000"), Some(Color::Rgb(0, 0, 0)));
        assert_eq!(hex_to_color(""), None);
        assert_eq!(hex_to_color("xyz"), None);
        assert_eq!(hex_to_color("12345"), None); // too short
        assert_eq!(hex_to_color("gggggg"), None); // non-hex
    }
}
