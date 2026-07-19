//! Metadata-columns toggle menu widget.
//!
//! A small checkbox overlay (modelled on the commit-menu overlay) listing the
//! toggleable right-side commit columns. Space/Enter toggles, j/k moves.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Widget},
};

use super::theme::Theme;
use crate::config::{MetadataColumn, MetadataColumns};

pub struct MetadataMenuWidget<'a> {
    columns: MetadataColumns,
    selected: usize,
    theme: &'a Theme,
}

impl<'a> MetadataMenuWidget<'a> {
    pub fn new(columns: MetadataColumns, selected: usize, theme: &'a Theme) -> Self {
        Self {
            columns,
            selected,
            theme,
        }
    }
}

impl<'a> Widget for MetadataMenuWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let block = self.theme.popup_block(" Columns ");

        let inner = block.inner(area);
        block.render(area, buf);

        for (i, col) in MetadataColumn::ALL.iter().enumerate() {
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

            let prefix = if is_selected { "> " } else { "  " };
            let check = if self.columns.is_visible(*col) {
                "[x] "
            } else {
                "[ ] "
            };
            let line = Line::from(vec![
                Span::styled(prefix, base_style),
                Span::styled(check, base_style.add_modifier(Modifier::BOLD)),
                Span::styled(col.label(), base_style),
            ]);
            buf.set_line(inner.x, y, &line, inner.width);
            if is_selected {
                buf.set_style(Rect::new(inner.x, y, inner.width, 1), base_style);
            }
        }
    }
}
