//! CI checks detail popup widget.
//!
//! Two views on `CiChecksView`: the check list, and (drilled in) a failed
//! check's log tail or an external check's URL. Loading/error states render
//! inline so the popup can open instantly and fill in asynchronously.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Widget},
};

use super::theme::Theme;
use crate::app::{ChecksState, CiChecksView, LogContent, LogView};
use crate::checks::CheckState;

pub struct CiChecksWidget<'a> {
    view: &'a CiChecksView,
    theme: &'a Theme,
}

impl<'a> CiChecksWidget<'a> {
    pub fn new(view: &'a CiChecksView, theme: &'a Theme) -> Self {
        Self { view, theme }
    }
}

fn state_color(state: CheckState, theme: &Theme) -> Color {
    match state {
        CheckState::Pass => theme.pr_ci_pass,
        CheckState::Fail => theme.pr_ci_fail,
        CheckState::Pending => theme.pr_ci_pending,
        CheckState::Skipped => theme.text_muted,
    }
}

impl<'a> Widget for CiChecksWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        match &self.view.log {
            Some(log) => self.render_log(log, area, buf),
            None => self.render_list(area, buf),
        }
    }
}

impl<'a> CiChecksWidget<'a> {
    fn block(&self, title: String) -> Block<'static> {
        self.theme.popup_block(title)
    }

    fn footer(&self, inner: Rect, buf: &mut Buffer, text: &str) {
        if inner.height == 0 {
            return;
        }
        let y = inner.y + inner.height - 1;
        buf.set_string(
            inner.x,
            y,
            truncate(text, inner.width as usize),
            Style::default().fg(self.theme.text_muted),
        );
    }

    fn render_list(&self, area: Rect, buf: &mut Buffer) {
        let block = self.block(format!(" PR #{} Checks ", self.view.pr_number));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 {
            return;
        }
        // Reserve the last row for the footer hint.
        let body_h = inner.height.saturating_sub(1) as usize;

        match &self.view.checks {
            ChecksState::Loading => {
                buf.set_string(
                    inner.x,
                    inner.y,
                    "  loading…",
                    Style::default().fg(self.theme.text_muted),
                );
            }
            ChecksState::Error(e) => {
                buf.set_string(
                    inner.x,
                    inner.y,
                    truncate(&format!("  {e}"), inner.width as usize),
                    Style::default().fg(self.theme.status_error_fg),
                );
            }
            ChecksState::Loaded(checks) if checks.is_empty() => {
                buf.set_string(
                    inner.x,
                    inner.y,
                    "  no checks reported",
                    Style::default().fg(self.theme.text_muted),
                );
            }
            ChecksState::Loaded(checks) => {
                // Window the list so the selection stays visible.
                let first = self.view.selected.saturating_sub(body_h.saturating_sub(1));
                for (row, check) in checks.iter().enumerate().skip(first).take(body_h) {
                    let y = inner.y + (row - first) as u16;
                    let selected = row == self.view.selected;
                    let base = if selected {
                        self.theme.list_selection_style()
                    } else {
                        Style::default().fg(self.theme.text_primary)
                    };
                    let icon = check.state.icon();
                    let dur = check.duration.as_deref().unwrap_or("");
                    let name = truncate(&check.name, inner.width.saturating_sub(10) as usize);
                    let prefix = if selected { "> " } else { "  " };
                    let line = Line::from(vec![
                        Span::styled(prefix, base),
                        Span::styled(
                            format!("{icon} "),
                            base.patch(Style::default().fg(state_color(check.state, self.theme)))
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(name, base),
                        Span::styled(format!("  {dur}"), base.patch(Style::default().fg(self.theme.text_muted))),
                    ]);
                    buf.set_line(inner.x, y, &line, inner.width);
                    if selected {
                        buf.set_style(Rect::new(inner.x, y, inner.width, 1), base);
                    }
                }
            }
        }

        self.footer(inner, buf, " ↑↓ move   Enter details   o open   Esc close");
    }

    fn render_log(&self, log: &LogView, area: Rect, buf: &mut Buffer) {
        let block = self.block(format!(" {} ", truncate(&log.title, 60)));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 {
            return;
        }
        let body_h = inner.height.saturating_sub(1) as usize;
        let w = inner.width as usize;

        match &log.content {
            LogContent::Loading => {
                buf.set_string(
                    inner.x,
                    inner.y,
                    "  loading log…",
                    Style::default().fg(self.theme.text_muted),
                );
            }
            LogContent::Error(e) => {
                buf.set_string(
                    inner.x,
                    inner.y,
                    truncate(&format!("  {e}"), w),
                    Style::default().fg(self.theme.status_error_fg),
                );
            }
            LogContent::External(url) => {
                buf.set_string(
                    inner.x,
                    inner.y,
                    "  External check — press o to open:",
                    Style::default().fg(self.theme.text_primary),
                );
                if inner.height > 1 {
                    buf.set_string(
                        inner.x,
                        inner.y + 1,
                        truncate(&format!("  {url}"), w),
                        Style::default().fg(self.theme.pr_badge),
                    );
                }
            }
            LogContent::Lines(lines) => {
                let start = log.scroll.min(lines.len().saturating_sub(1));
                for (i, line) in lines.iter().skip(start).take(body_h).enumerate() {
                    buf.set_string(
                        inner.x,
                        inner.y + i as u16,
                        truncate(line, w),
                        Style::default().fg(self.theme.text_primary),
                    );
                }
            }
        }

        let hint = match &log.content {
            LogContent::External(_) => " o open   Esc back",
            _ => " ↑↓/PgUp/PgDn scroll   g/G top/bottom   Esc back",
        };
        self.footer(inner, buf, hint);
    }
}

/// Truncate a string to `max` display columns (char-count approximation, fine
/// for log lines and names).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}
