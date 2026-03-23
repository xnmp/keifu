//! Simple modal popup.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use ansi_to_tui::IntoText;

pub struct Modal<'a> {
    title: &'a str,
    message: &'a str,
}

impl<'a> Modal<'a> {
    pub fn new(title: &'a str, message: &'a str) -> Self {
        Self { title, message }
    }
}

impl<'a> Widget for Modal<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let block = Block::default()
            .title(format!(" {} ", self.title))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Black));

        let hint_style = Style::default().fg(Color::DarkGray);

        let mut text = self.message.as_bytes().into_text().unwrap_or_default();
        for line in &mut text.lines {
            line.spans.insert(0, Span::raw("  "));
        }

        // Add footer hint.
        if !text.lines.is_empty() {
            text.lines.push(Line::from(""));
        }
        text.lines
            .push(Line::from(Span::styled("  Esc: close", hint_style)));

        let paragraph = Paragraph::new(text).block(block);
        Widget::render(paragraph, area, buf);
    }
}
