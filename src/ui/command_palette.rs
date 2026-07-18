//! Command palette popup widget: query line, ranked rows with a dim category
//! tag and right-aligned keybind/hash hint, and a "…N more" footer.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Widget},
};
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;
use crate::palette::Candidate;

pub struct CommandPaletteWidget<'a> {
    query: &'a str,
    items: &'a [Candidate],
    more: usize,
    selected: usize,
    theme: &'a Theme,
}

impl<'a> CommandPaletteWidget<'a> {
    pub fn new(
        query: &'a str,
        items: &'a [Candidate],
        more: usize,
        selected: usize,
        theme: &'a Theme,
    ) -> Self {
        Self {
            query,
            items,
            more,
            selected,
            theme,
        }
    }
}

impl<'a> Widget for CommandPaletteWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let block = Block::default()
            .title(" Command Palette ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.popup_border))
            .style(Style::default().bg(self.theme.popup_bg));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let width = inner.width as usize;

        let dim = Style::default()
            .fg(self.theme.text_muted)
            .add_modifier(Modifier::DIM);

        // Query line (with a block cursor).
        let query_line = Line::from(vec![
            Span::styled("› ", Style::default().fg(self.theme.help_key)),
            Span::styled(
                self.query.to_string(),
                Style::default().fg(self.theme.text_primary),
            ),
            Span::styled("▏", Style::default().fg(self.theme.help_key)),
        ]);
        buf.set_line(inner.x, inner.y, &query_line, inner.width);

        // Rows start below the query line; leave the last line for the footer
        // when there are more matches than fit.
        let footer = self.more > 0;
        let list_top = inner.y + 1;
        let list_rows = inner
            .height
            .saturating_sub(1)
            .saturating_sub(footer as u16) as usize;

        for (i, item) in self.items.iter().take(list_rows).enumerate() {
            let y = list_top + i as u16;
            let is_selected = i == self.selected;
            let base = if is_selected {
                self.theme.list_selection_style()
            } else {
                Style::default().fg(self.theme.text_primary)
            };

            let prefix = if is_selected { "▸ " } else { "  " };
            let tag = format!("{:<7}", item.kind.tag());
            let hint = item.hint.clone().unwrap_or_default();

            // Truncate the label to whatever space is left after prefix, tag,
            // and the right-aligned hint (+1 gap).
            let fixed = prefix.width() + tag.width() + hint.width() + 1;
            let label_budget = width.saturating_sub(fixed);
            let label = truncate(&item.label, label_budget);

            let used = prefix.width() + tag.width() + label.width() + hint.width();
            let pad = width.saturating_sub(used);

            let tag_style = if is_selected { base } else { dim };
            let hint_style = if is_selected { base } else { dim };
            let spans = vec![
                Span::styled(prefix, base),
                Span::styled(tag, tag_style),
                Span::styled(label, base),
                Span::styled(" ".repeat(pad), base),
                Span::styled(hint, hint_style),
            ];
            buf.set_line(inner.x, y, &Line::from(spans), inner.width);
            if is_selected {
                buf.set_style(Rect::new(inner.x, y, inner.width, 1), base);
            }
        }

        if footer {
            let y = inner.y + inner.height - 1;
            let text = format!("…{} more", self.more);
            buf.set_line(inner.x, y, &Line::from(Span::styled(text, dim)), inner.width);
        }
    }
}

/// Truncate `s` to at most `max` display columns, ending in `…` when clipped.
fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.to_string().width();
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}
