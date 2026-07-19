//! Issue detail popup + label picker widgets.
//!
//! The detail view builds the issue (title, meta, body, comments) into styled
//! `Line`s rendered with Ratatui's wrapping + vertical scroll — the same
//! approach as `pr_thread`. The label picker is a checkbox list over the repo's
//! labels. Loading/error states render inline (never `AppMode::Error`).

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Widget, Wrap},
};

use super::theme::Theme;
use crate::app::{IssueDetailState, IssueDetailView, IssueLabelPicker};
use crate::issue::{IssueComment, IssueDetail, IssueState};

pub struct IssueDetailWidget<'a> {
    view: &'a IssueDetailView,
    theme: &'a Theme,
}

impl<'a> IssueDetailWidget<'a> {
    pub fn new(view: &'a IssueDetailView, theme: &'a Theme) -> Self {
        Self { view, theme }
    }
}

impl<'a> Widget for IssueDetailWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = self
            .theme
            .popup_block(format!(" Issue #{} ", self.view.number));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 {
            return;
        }
        // Full-screen: the shared status bar carries the key hints, so the whole
        // inner area is available for the (scrollable) body.
        let lines = build_lines(&self.view.state, self.theme);
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.view.scroll as u16, 0))
            .render(inner, buf);
    }
}

/// Build the detail as styled lines. Shared by the widget and the draw pre-pass
/// that measures wrapped height for scroll clamping.
pub fn build_lines(state: &IssueDetailState, theme: &Theme) -> Vec<Line<'static>> {
    match state {
        IssueDetailState::Loading => vec![muted_line("  loading issue…", theme)],
        IssueDetailState::Error(e) => vec![Line::from(Span::styled(
            format!("  {e}"),
            Style::default().fg(theme.status_error_fg),
        ))],
        IssueDetailState::Ready(detail) => build_detail(detail, theme),
    }
}

fn build_detail(d: &IssueDetail, theme: &Theme) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();

    // Title.
    out.push(Line::from(Span::styled(
        d.title.clone(),
        Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD),
    )));

    // State + author + dates.
    let (state_color, state_text) = match d.state {
        IssueState::Open => (theme.pr_ci_pass, "open".to_string()),
        IssueState::Closed => {
            let reason = d
                .state_reason
                .as_deref()
                .map(|r| format!("closed ({})", r.to_lowercase()))
                .unwrap_or_else(|| "closed".to_string());
            (theme.text_muted, reason)
        }
    };
    out.push(Line::from(vec![
        Span::styled(
            state_text,
            Style::default().fg(state_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  @{} · opened {}", d.author, date_only(&d.created_at)),
            Style::default().fg(theme.text_muted),
        ),
    ]));

    // Labels: colored chips (reusing the list's hex→color), with a muted caption.
    if !d.labels.is_empty() {
        let mut spans = vec![Span::styled("labels: ", theme.metadata_label_style())];
        for label in &d.labels {
            let color = super::issue_list::hex_to_color(&label.color).unwrap_or(theme.pr_badge);
            spans.push(Span::styled(
                format!("[{}] ", label.name),
                Style::default().fg(color),
            ));
        }
        out.push(Line::from(spans));
    }
    if !d.assignees.is_empty() {
        let who: Vec<String> = d.assignees.iter().map(|a| format!("@{a}")).collect();
        out.push(Line::from(vec![
            Span::styled("assignees: ", theme.metadata_label_style()),
            Span::styled(who.join(" "), Style::default().fg(theme.text_secondary)),
        ]));
    }

    out.push(Line::default());
    push_body(&mut out, &d.body, theme, "");

    for c in &d.comments {
        out.push(Line::default());
        push_comment(&mut out, c, theme);
    }

    out
}

fn push_comment(out: &mut Vec<Line<'static>>, c: &IssueComment, theme: &Theme) {
    // Author + relative time header, then the body indented under it.
    out.push(Line::from(vec![
        Span::styled(
            format!("@{} ", c.author),
            Style::default()
                .fg(theme.author_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("commented · {}", crate::issue::relative_time(&c.created_at)),
            Style::default().fg(theme.text_muted),
        ),
    ]));
    push_body(out, &c.body, theme, "  ");
}

/// Push a body (one styled line per source line), each prefixed with `indent`.
/// An empty body reads as a muted placeholder so a blank issue/comment isn't a
/// void.
fn push_body(out: &mut Vec<Line<'static>>, body: &str, theme: &Theme, indent: &str) {
    if body.trim().is_empty() {
        out.push(muted_line(&format!("{indent}(no description)"), theme));
        return;
    }
    for line in body.lines() {
        out.push(Line::from(Span::styled(
            format!("{indent}{line}"),
            Style::default().fg(theme.text_primary),
        )));
    }
}

fn muted_line(text: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(theme.text_muted),
    ))
}

fn date_only(ts: &str) -> String {
    ts.split('T').next().unwrap_or(ts).to_string()
}

// ── label picker ───────────────────────────────────────────────────────

pub struct IssueLabelPickerWidget<'a> {
    picker: &'a IssueLabelPicker,
    selected: usize,
    theme: &'a Theme,
}

impl<'a> IssueLabelPickerWidget<'a> {
    pub fn new(picker: &'a IssueLabelPicker, selected: usize, theme: &'a Theme) -> Self {
        Self {
            picker,
            selected,
            theme,
        }
    }
}

impl<'a> Widget for IssueLabelPickerWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = self
            .theme
            .popup_block(format!(" Labels · #{} ", self.picker.number));
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
            let line = Line::from(vec![
                Span::styled(prefix, base),
                Span::styled(mark, base),
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
                " ↑↓ move   Space toggle   Enter apply   Esc cancel",
                inner.width as usize,
            ),
            Style::default().fg(self.theme.text_muted),
        );
    }
}
