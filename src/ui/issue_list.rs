//! GitHub issue list widget (full-screen).
//!
//! Renders `IssueListView` as a scrollable, filtered list: a header line (repo +
//! active filters + shown/total counts) followed by aligned rows — colored state
//! glyph, right-aligned number, blocked marker, truncated title, colored label
//! chips, relative updated time, and a muted author. Loading/empty/error states
//! render inline so the view opens instantly and fills in asynchronously. The
//! label-filter picker (`IssueLabelFilterWidget`) is a checkbox multi-select over
//! the repo's labels.

use std::collections::HashSet;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Widget},
};
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;
use crate::app::{IssueLabelFilter, IssueListState, IssueListView};
use crate::issue::{relative_time, visible_issues, IssueInfo, IssueState};

/// Max label chips shown on a single list row before collapsing to `+N`.
const MAX_ROW_CHIPS: usize = 3;

pub struct IssueListWidget<'a> {
    view: &'a IssueListView,
    blocked: &'a HashSet<u64>,
    repo_name: &'a str,
    theme: &'a Theme,
}

impl<'a> IssueListWidget<'a> {
    pub fn new(
        view: &'a IssueListView,
        blocked: &'a HashSet<u64>,
        repo_name: &'a str,
        theme: &'a Theme,
    ) -> Self {
        Self {
            view,
            blocked,
            repo_name,
            theme,
        }
    }
}

impl<'a> Widget for IssueListWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = self.theme.popup_block(" Issues ");
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 {
            return;
        }

        match &self.view.state {
            IssueListState::Loading => {
                self.placeholder(inner, buf, "  loading issues…", self.theme.text_muted);
            }
            IssueListState::Error(e) => {
                self.placeholder(inner, buf, &format!("  {e}"), self.theme.status_error_fg);
            }
            IssueListState::Ready(issues) => {
                let visible = visible_issues(issues, &self.view.view_filter, self.blocked);
                self.render_header(issues, visible.len(), inner, buf);
                if inner.height < 2 {
                    return;
                }
                let list_area = Rect::new(
                    inner.x,
                    inner.y + 1,
                    inner.width,
                    inner.height.saturating_sub(1),
                );
                if visible.is_empty() {
                    let msg = if self.view.view_filter.is_active() {
                        "no issues match filters"
                    } else {
                        "no issues"
                    };
                    buf.set_line(
                        list_area.x,
                        list_area.y,
                        &Line::from(Span::styled(msg, self.theme.placeholder_style())),
                        list_area.width,
                    );
                    return;
                }
                self.render_rows(issues, &visible, list_area, buf);
            }
        }
    }
}

impl<'a> IssueListWidget<'a> {
    fn placeholder(&self, inner: Rect, buf: &mut Buffer, text: &str, color: Color) {
        buf.set_string(
            inner.x,
            inner.y,
            super::truncate_str(text, inner.width as usize),
            Style::default().fg(color),
        );
    }

    /// Header: repo · status-filter [label chips] ⛔unblocked … shown/total.
    fn render_header(&self, issues: &[IssueInfo], shown: usize, inner: Rect, buf: &mut Buffer) {
        let accent = self.theme.accent();
        let muted = Style::default().fg(self.theme.text_muted);
        let mut spans: Vec<Span<'static>> = vec![
            Span::styled(
                self.repo_name.to_string(),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ·  ", muted),
            Span::styled(
                self.view.filter.label().to_string(),
                Style::default().fg(self.theme.text_secondary),
            ),
        ];
        // Active label-filter chips.
        for name in &self.view.view_filter.labels {
            let color = issues
                .iter()
                .flat_map(|i| &i.labels)
                .find(|l| &l.name == name)
                .and_then(|l| hex_to_color(&l.color))
                .unwrap_or(self.theme.pr_badge);
            spans.push(Span::styled(" ", muted));
            spans.push(Span::styled(
                format!("[{name}]"),
                Style::default().fg(color),
            ));
        }
        if self.view.view_filter.unblocked_only {
            spans.push(Span::styled(
                "  ⛔ unblocked".to_string(),
                Style::default().fg(self.theme.pr_ci_pending),
            ));
        }

        let left = Line::from(spans);
        buf.set_line(inner.x, inner.y, &left, inner.width);

        // Right-aligned shown/total count.
        let count = format!("{shown}/{} ", issues.len());
        let cw = UnicodeWidthStr::width(count.as_str()) as u16;
        if cw < inner.width {
            buf.set_string(inner.x + inner.width - cw, inner.y, count, muted);
        }
    }

    fn render_rows(&self, issues: &[IssueInfo], visible: &[usize], area: Rect, buf: &mut Buffer) {
        let body_h = area.height as usize;
        // Window so the selection stays on screen.
        let first = self.view.selected.saturating_sub(body_h.saturating_sub(1));
        // Number column width from the widest visible number.
        let num_w = visible
            .iter()
            .filter_map(|&i| issues.get(i))
            .map(|i| format!("#{}", i.number).len())
            .max()
            .unwrap_or(3);

        for (row, &idx) in visible.iter().enumerate().skip(first).take(body_h) {
            let Some(issue) = issues.get(idx) else {
                continue;
            };
            let y = area.y + (row - first) as u16;
            let selected = row == self.view.selected;
            let blocked = self.blocked.contains(&issue.number);
            self.render_row(issue, num_w, blocked, area.x, y, area.width, selected, buf);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_row(
        &self,
        issue: &IssueInfo,
        num_w: usize,
        blocked: bool,
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

        let (glyph_color, glyph) = match issue.state {
            IssueState::Open => (self.theme.pr_ci_pass, "●"),
            IssueState::Closed => (self.theme.issue_closed, "✓"),
        };

        // Left column: arrow, glyph, right-aligned number, blocked marker.
        let mut left: Vec<Span<'static>> = vec![
            Span::styled(if selected { "> " } else { "  " }, base),
            Span::styled(
                format!("{glyph} "),
                base.patch(Style::default().fg(glyph_color))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{:>num_w$} ", format!("#{}", issue.number)), muted),
        ];
        if blocked {
            left.push(Span::styled(
                "⛔ ".to_string(),
                base.patch(Style::default().fg(self.theme.pr_ci_fail)),
            ));
        }

        // Right column: label chips, relative time, author.
        let right = self.right_spans(issue, base, muted);
        let right_w = spans_width(&right);
        let left_fixed = spans_width(&left);

        // Title fills the gap between the fixed-left and the right column.
        let gap_min = 1usize;
        let avail = (width as usize)
            .saturating_sub(left_fixed)
            .saturating_sub(right_w)
            .saturating_sub(gap_min);
        let title = super::truncate_str(&issue.title, avail.max(1));
        let title_w = UnicodeWidthStr::width(title.as_str());
        left.push(Span::styled(title, base));

        // Pad so the right column is right-aligned, when it fits.
        let used = left_fixed + title_w;
        let mut spans = left;
        if right_w > 0 && used + gap_min + right_w <= width as usize {
            let pad = width as usize - used - right_w;
            spans.push(Span::styled(" ".repeat(pad), base));
            spans.extend(right);
        }

        let line = Line::from(spans);
        buf.set_line(x, y, &line, width);
        if selected {
            buf.set_style(Rect::new(x, y, width, 1), base);
        }
    }

    /// The right-aligned column for a row: up to [`MAX_ROW_CHIPS`] colored label
    /// chips (then `+N`), the relative updated time, and a muted first assignee.
    fn right_spans(&self, issue: &IssueInfo, base: Style, muted: Style) -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for label in issue.labels.iter().take(MAX_ROW_CHIPS) {
            let color = hex_to_color(&label.color).unwrap_or(self.theme.pr_badge);
            spans.push(Span::styled(
                format!("[{}] ", label.name),
                base.patch(Style::default().fg(color)),
            ));
        }
        if issue.labels.len() > MAX_ROW_CHIPS {
            spans.push(Span::styled(
                format!("+{} ", issue.labels.len() - MAX_ROW_CHIPS),
                muted,
            ));
        }
        if !issue.updated_at.is_empty() {
            spans.push(Span::styled(format!("{} ", relative_time(&issue.updated_at)), muted));
        }
        if let Some(who) = issue.assignees.first() {
            spans.push(Span::styled(format!("@{who}"), muted));
        }
        spans
    }
}

/// Total display width of a span list.
fn spans_width(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
}

/// Parse a 6-digit hex color (as gh emits it, no leading `#`) into an RGB color.
/// Returns `None` for anything that isn't exactly six hex digits. Shared with the
/// detail widget's chip rendering.
pub(crate) fn hex_to_color(hex: &str) -> Option<Color> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

// ── label-filter picker ──────────────────────────────────────────────────────

/// Checkbox multi-select over the repo's labels, applied as the list's label
/// view-filter. Mirrors the branch-filter / label-picker checkbox idiom.
pub struct IssueLabelFilterWidget<'a> {
    picker: &'a IssueLabelFilter,
    selected: usize,
    theme: &'a Theme,
}

impl<'a> IssueLabelFilterWidget<'a> {
    pub fn new(picker: &'a IssueLabelFilter, selected: usize, theme: &'a Theme) -> Self {
        Self {
            picker,
            selected,
            theme,
        }
    }
}

impl<'a> Widget for IssueLabelFilterWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = self.theme.popup_block(" Filter by Label ");
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 {
            return;
        }
        let body_h = inner.height.saturating_sub(1) as usize;
        let first = self.selected.saturating_sub(body_h.saturating_sub(1));

        for (row, label) in self
            .picker
            .labels
            .iter()
            .enumerate()
            .skip(first)
            .take(body_h)
        {
            let y = inner.y + (row - first) as u16;
            let selected = row == self.selected;
            let base = if selected {
                self.theme.list_selection_style()
            } else {
                Style::default().fg(self.theme.text_primary)
            };
            let checked = self.picker.chosen.get(row).copied().unwrap_or(false);
            let mark = if checked { "[x] " } else { "[ ] " };
            let prefix = if selected { "> " } else { "  " };
            let chip = hex_to_color(&label.color).unwrap_or(self.theme.pr_badge);
            let line = Line::from(vec![
                Span::styled(prefix, base),
                Span::styled(mark, base),
                Span::styled("● ", base.patch(Style::default().fg(chip))),
                Span::styled(label.name.clone(), base),
            ]);
            buf.set_line(inner.x, y, &line, inner.width);
            if selected {
                buf.set_style(Rect::new(inner.x, y, inner.width, 1), base);
            }
        }

        let y = inner.y + inner.height - 1;
        buf.set_string(
            inner.x,
            y,
            super::truncate_str(
                " Space toggle   ^a all   ^o none   Enter apply   Esc cancel",
                inner.width as usize,
            ),
            Style::default().fg(self.theme.text_muted),
        );
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

    #[test]
    fn spans_width_sums_display_columns() {
        let spans = vec![
            Span::raw("ab"),
            Span::raw("↑↓"), // 2 display cols
            Span::raw("c"),
        ];
        assert_eq!(spans_width(&spans), 5);
    }
}
