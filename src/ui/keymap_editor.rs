//! Stateless keyboard-shortcut editor widget.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Widget},
};

use super::theme::Theme;
use crate::keymap::{binding_registry, ResolvedKeymap};

pub struct KeymapEditorWidget<'a> {
    keymap: &'a ResolvedKeymap,
    selected: usize,
    capturing: bool,
    theme: &'a Theme,
}

impl<'a> KeymapEditorWidget<'a> {
    pub fn new(
        keymap: &'a ResolvedKeymap,
        selected: usize,
        capturing: bool,
        theme: &'a Theme,
    ) -> Self {
        Self {
            keymap,
            selected,
            capturing,
            theme,
        }
    }
}

impl Widget for KeymapEditorWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let title = if self.capturing {
            " Keyboard shortcuts — press a key "
        } else {
            " Keyboard shortcuts "
        };
        let block = self.theme.popup_block(title);
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height < 3 || inner.width < 12 {
            return;
        }
        let dim = Style::default()
            .fg(self.theme.text_muted)
            .add_modifier(Modifier::DIM);
        let list_height = inner.height.saturating_sub(2) as usize;
        let scroll = self.selected.saturating_sub(list_height.saturating_sub(1));
        for (row, descriptor) in binding_registry()
            .iter()
            .enumerate()
            .skip(scroll)
            .take(list_height)
        {
            let y = inner.y + (row - scroll) as u16;
            let selected = row == self.selected;
            let style = if selected {
                self.theme.list_selection_style()
            } else {
                Style::default().fg(self.theme.text_primary)
            };
            let prefix = if selected { "▸ " } else { "  " };
            let binding = self.keymap.display_bindings(descriptor.id);
            let label = format!("{prefix}{}", descriptor.id);
            let padding = (inner.width as usize)
                .saturating_sub(label.len() + binding.len())
                .max(1);
            buf.set_line(
                inner.x,
                y,
                &Line::from(vec![
                    Span::styled(label, style),
                    Span::styled(" ".repeat(padding), style),
                    Span::styled(binding, style.add_modifier(Modifier::BOLD)),
                ]),
                inner.width,
            );
            if selected {
                buf.set_style(Rect::new(inner.x, y, inner.width, 1), style);
            }
        }

        let footer_y = inner.y + inner.height.saturating_sub(2);
        let detail = if self.capturing {
            "Press a supported key (Esc cancels capture)"
        } else if let Some(warning) = self.keymap.warnings().first() {
            &warning.reason
        } else {
            "Enter capture  u unassign  Ctrl+S save  Esc discard"
        };
        buf.set_line(
            inner.x,
            footer_y,
            &Line::from(Span::styled(detail, dim)),
            inner.width,
        );
        buf.set_line(
            inner.x,
            footer_y + 1,
            &Line::from(Span::styled("Changes are pending until saved", dim)),
            inner.width,
        );
    }
}
