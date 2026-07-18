//! PR conversation popup widget.
//!
//! Builds the conversation into styled `Line`s (title, body, chronological
//! comments/reviews, then review threads with file/line context) and renders
//! them with Ratatui's own wrapping + vertical scroll. Markdown is rendered
//! plainly: only fenced code blocks are dimmed — no inline formatting, links,
//! or lists are parsed (see module note).

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

use super::theme::Theme;
use crate::app::{PrThreadView, ThreadViewState};
use crate::pr_thread::{ConversationItem, PrThread, ReviewItemState, ReviewThread};

pub struct PrThreadWidget<'a> {
    view: &'a PrThreadView,
    theme: &'a Theme,
}

impl<'a> PrThreadWidget<'a> {
    pub fn new(view: &'a PrThreadView, theme: &'a Theme) -> Self {
        Self { view, theme }
    }
}

impl<'a> Widget for PrThreadWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = Block::default()
            .title(format!(" PR #{} Conversation ", self.view.pr_number))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.popup_border))
            .style(Style::default().bg(self.theme.popup_bg));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 {
            return;
        }
        // Reserve the last row for the footer hint.
        let body = Rect::new(inner.x, inner.y, inner.width, inner.height - 1);
        let lines = build_lines(&self.view.state, self.theme);
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.view.scroll as u16, 0))
            .render(body, buf);

        let hint = " ↑↓ scroll   o open PR   r review   Esc close";
        let fy = inner.y + inner.height - 1;
        buf.set_string(
            inner.x,
            fy,
            trunc(hint, inner.width as usize),
            Style::default().fg(self.theme.text_muted),
        );
    }
}

/// Build the conversation as styled lines. Used by the widget and by the draw
/// pre-pass that measures wrapped height for scroll clamping.
pub fn build_lines(state: &ThreadViewState, theme: &Theme) -> Vec<Line<'static>> {
    match state {
        ThreadViewState::Loading => vec![muted_line("  loading conversation…", theme)],
        ThreadViewState::Error(e) => vec![Line::from(Span::styled(
            format!("  {e}"),
            Style::default().fg(theme.status_error_fg),
        ))],
        ThreadViewState::Loaded(thread) => build_thread(thread, theme),
    }
}

fn build_thread(t: &PrThread, theme: &Theme) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();

    // Header: title + author/date.
    out.push(Line::from(Span::styled(
        t.title.clone(),
        Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD),
    )));
    out.push(muted_line(
        &format!("@{} · {}", t.author, date_only(&t.created_at)),
        theme,
    ));
    out.push(Line::default());
    push_body(&mut out, &t.body, theme, "");

    // Chronological comments + reviews.
    for item in &t.items {
        out.push(Line::default());
        match item {
            ConversationItem::Comment {
                author,
                created_at,
                body,
            } => {
                out.push(item_header(author, "commented", created_at, theme.author_color, theme));
                push_body(&mut out, body, theme, "");
            }
            ConversationItem::Review {
                author,
                created_at,
                state,
                body,
            } => {
                let color = review_color(*state, theme);
                out.push(item_header(author, state.label(), created_at, color, theme));
                push_body(&mut out, body, theme, "");
            }
        }
    }
    if t.more_items > 0 {
        out.push(muted_line(&format!("  …{} more comments/reviews", t.more_items), theme));
    }

    // Review threads.
    match &t.threads {
        None => {
            out.push(Line::default());
            out.push(muted_line(
                "  Review threads unavailable (needs GraphQL; gh fell back to REST)",
                theme,
            ));
        }
        Some(threads) if !threads.is_empty() => {
            out.push(Line::default());
            let unresolved = t.unresolved_count();
            out.push(Line::from(Span::styled(
                format!("Review threads ({} unresolved)", unresolved),
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            )));
            for th in threads {
                push_thread(&mut out, th, theme);
            }
            if t.more_threads > 0 {
                out.push(muted_line(&format!("  …{} more threads", t.more_threads), theme));
            }
        }
        Some(_) => {}
    }

    out
}

fn push_thread(out: &mut Vec<Line<'static>>, th: &ReviewThread, theme: &Theme) {
    out.push(Line::default());
    let loc = match th.line {
        Some(l) => format!("{}:{}", th.path, l),
        None => th.path.clone(),
    };
    // Unresolved threads are prominent (accent); resolved are muted.
    let (tag, style) = if th.resolved {
        (
            "[resolved]",
            Style::default().fg(theme.text_muted),
        )
    } else {
        (
            "[open]",
            Style::default()
                .fg(theme.pr_ci_fail)
                .add_modifier(Modifier::BOLD),
        )
    };
    out.push(Line::from(vec![
        Span::styled(format!("  {loc} "), Style::default().fg(theme.text_secondary)),
        Span::styled(tag.to_string(), style),
    ]));
    for c in &th.comments {
        out.push(Line::from(Span::styled(
            format!("    @{}", c.author),
            Style::default()
                .fg(theme.author_color)
                .add_modifier(Modifier::BOLD),
        )));
        push_body(out, &c.body, theme, "    ");
    }
    if th.more_comments > 0 {
        out.push(muted_line(&format!("    …{} more", th.more_comments), theme));
    }
}

/// A comment/review header: `@author label · date`.
fn item_header(
    author: &str,
    label: &str,
    created_at: &str,
    label_color: ratatui::style::Color,
    theme: &Theme,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("@{author} "),
            Style::default()
                .fg(theme.author_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            label.to_string(),
            Style::default().fg(label_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · {}", date_only(created_at)),
            Style::default().fg(theme.text_muted),
        ),
    ])
}

fn review_color(state: ReviewItemState, theme: &Theme) -> ratatui::style::Color {
    match state {
        ReviewItemState::Approved => theme.pr_ci_pass,
        ReviewItemState::ChangesRequested => theme.pr_ci_fail,
        ReviewItemState::Dismissed => theme.text_muted,
        _ => theme.author_color,
    }
}

/// Push a body, preprocessed (blank runs collapsed) and prefixed with `indent`.
/// Fenced code blocks (```) render dimmed; no other markdown is interpreted.
fn push_body(out: &mut Vec<Line<'static>>, body: &str, theme: &Theme, indent: &str) {
    let mut in_fence = false;
    for line in crate::pr_thread::preprocess_body(body) {
        let is_fence = line.trim_start().starts_with("```");
        let dim = in_fence || is_fence;
        if is_fence {
            in_fence = !in_fence;
        }
        let style = if dim {
            Style::default()
                .fg(theme.text_muted)
                .add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(theme.text_primary)
        };
        out.push(Line::from(Span::styled(format!("{indent}{line}"), style)));
    }
}

fn muted_line(text: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(theme.text_muted),
    ))
}

/// Date portion of an RFC3339 timestamp ("2026-07-01T…" → "2026-07-01").
fn date_only(ts: &str) -> String {
    ts.split('T').next().unwrap_or(ts).to_string()
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}
