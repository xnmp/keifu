//! Commit context menu widget

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Widget},
};

use super::theme::Theme;
use crate::app::CommitMenuItem;

pub struct CommitMenuWidget<'a> {
    items: &'a [CommitMenuItem],
    selected: usize,
    theme: &'a Theme,
}

impl<'a> CommitMenuWidget<'a> {
    pub fn new(items: &'a [CommitMenuItem], selected: usize, theme: &'a Theme) -> Self {
        Self { items, selected, theme }
    }
}

impl<'a> Widget for CommitMenuWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let block = Block::default()
            .title(" Actions ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.popup_border))
            .style(Style::default().bg(self.theme.popup_bg));

        let inner = block.inner(area);
        block.render(area, buf);

        for (i, item) in self.items.iter().enumerate() {
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

            let prefix = if is_selected { " > " } else { "   " };
            let label = format!("{}{}", prefix, item.label());
            let line = Line::from(Span::styled(label, style));
            buf.set_line(inner.x, y, &line, inner.width);

            if is_selected {
                buf.set_style(Rect::new(inner.x, y, inner.width, 1), style);
            }
        }
    }
}
