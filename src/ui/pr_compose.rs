//! PR-compose editor popup (create PR title/body, or a review body).

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Clear, Widget},
};

use super::theme::Theme;
use crate::app::ComposePurpose;
use crate::text_editor::TextEditor;

pub struct PrComposeWidget<'a> {
    editor: &'a TextEditor,
    purpose: ComposePurpose,
    theme: &'a Theme,
}

impl<'a> PrComposeWidget<'a> {
    pub fn new(editor: &'a TextEditor, purpose: ComposePurpose, theme: &'a Theme) -> Self {
        Self {
            editor,
            purpose,
            theme,
        }
    }

    fn title(&self) -> &'static str {
        match self.purpose {
            ComposePurpose::CreatePr => " New Pull Request ",
            ComposePurpose::ReviewRequestChanges { .. } => " Request Changes ",
            ComposePurpose::ReviewComment { .. } => " Review Comment ",
        }
    }

    fn header(&self) -> &'static str {
        match self.purpose {
            ComposePurpose::CreatePr => "First line = title, the rest is the body:",
            _ => "Review body:",
        }
    }
}

/// The inner text area (where editor lines start), for cursor placement. Mirrors
/// the widget's layout: block border + 1 col horizontal padding + 1 header row.
pub fn text_area(popup: Rect) -> Rect {
    // Border (1) + the popup block's horizontal padding (1) on each side.
    let inner_x = popup.x + 2;
    let inner_y = popup.y + 1;
    let inner_w = popup.width.saturating_sub(4);
    let inner_h = popup.height.saturating_sub(2);
    // Header row on top, hint row at bottom.
    Rect::new(
        inner_x,
        inner_y + 1,
        inner_w,
        inner_h.saturating_sub(2),
    )
}

impl<'a> Widget for PrComposeWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let block = self.theme.popup_block(self.title());
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height < 2 {
            return;
        }

        // Header.
        buf.set_string(
            inner.x,
            inner.y,
            trunc(self.header(), inner.width as usize),
            Style::default().fg(self.theme.text_muted),
        );

        // Editor lines.
        let body = text_area(area);
        let is_create = matches!(self.purpose, ComposePurpose::CreatePr);
        for (row, line) in self.editor.lines().iter().enumerate() {
            if row as u16 >= body.height {
                break;
            }
            // Highlight the title line (row 0) for Create.
            let style = if is_create && row == 0 {
                Style::default()
                    .fg(self.theme.text_primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.theme.text_primary)
            };
            buf.set_string(body.x, body.y + row as u16, trunc(line, body.width as usize), style);
        }

        // Hint (bottom row).
        let hint = " Ctrl+S submit   Ctrl+E editor   Esc cancel ";
        let fy = inner.y + inner.height - 1;
        buf.set_string(
            inner.x,
            fy,
            trunc(hint, inner.width as usize),
            Style::default().fg(self.theme.text_muted),
        );
    }
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}
