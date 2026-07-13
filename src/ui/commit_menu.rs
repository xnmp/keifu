//! Commit context menu widget

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Widget},
};

use super::theme::Theme;
use crate::app::CommitMenuItem;

pub struct CommitMenuWidget<'a> {
    items: &'a [CommitMenuItem],
    selected: usize,
    filter: &'a str,
    theme: &'a Theme,
}

impl<'a> CommitMenuWidget<'a> {
    pub fn new(
        items: &'a [CommitMenuItem],
        selected: usize,
        filter: &'a str,
        theme: &'a Theme,
    ) -> Self {
        Self {
            items,
            selected,
            filter,
            theme,
        }
    }
}

impl<'a> Widget for CommitMenuWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let title = if self.filter.is_empty() {
            " Actions ".to_string()
        } else {
            format!(" Actions: {}_ ", self.filter)
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.popup_border))
            .style(Style::default().bg(self.theme.popup_bg));

        let inner = block.inner(area);
        block.render(area, buf);

        let matcher = SkimMatcherV2::default();
        let has_filter = !self.filter.is_empty();

        // Compute ordered items (matching first by score, then non-matching)
        type FuzzyMatch = Option<(i64, Vec<usize>)>;
        let ordered: Vec<(CommitMenuItem, FuzzyMatch)> = if has_filter {
            let mut scored: Vec<_> = self
                .items
                .iter()
                .map(|item| {
                    let result = matcher.fuzzy_indices(item.label(), self.filter);
                    (*item, result)
                })
                .collect();

            scored.sort_by(|a, b| match (&a.1, &b.1) {
                (Some((sa, _)), Some((sb, _))) => sb.cmp(sa),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });

            scored
        } else {
            self.items.iter().map(|item| (*item, None)).collect()
        };

        // Only show matching items when filter is active
        let visible: Vec<_> = if has_filter {
            ordered
                .into_iter()
                .filter(|(_, result)| result.is_some())
                .collect()
        } else {
            ordered
        };

        for (i, (item, match_result)) in visible.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }

            let y = inner.y + i as u16;
            let is_selected = i == self.selected;

            let base_style = if is_selected {
                self.theme.list_selection_style()
            } else {
                Style::default().fg(self.theme.text_primary)
            };

            let prefix = if is_selected { " > " } else { "   " };
            let label = item.label();

            // Build spans with highlight for matched characters
            let mut spans = vec![Span::styled(prefix, base_style)];

            if let Some((_, indices)) = match_result {
                for (ci, ch) in label.chars().enumerate() {
                    let style = if indices.contains(&ci) {
                        base_style.add_modifier(Modifier::BOLD).fg(self.theme.help_key)
                    } else {
                        base_style
                    };
                    spans.push(Span::styled(String::from(ch), style));
                }
            } else {
                spans.push(Span::styled(label, base_style));
            }

            let line = Line::from(spans);
            buf.set_line(inner.x, y, &line, inner.width);

            if is_selected {
                buf.set_style(Rect::new(inner.x, y, inner.width, 1), base_style);
            }
        }
    }
}
