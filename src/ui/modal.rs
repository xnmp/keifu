//! Simple modal popup.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

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
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(""));
        for line in self.message.lines() {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(line.to_string(), Style::default().add_modifier(Modifier::BOLD)),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("  Esc: close", hint_style)));

        let paragraph = Paragraph::new(lines).block(block);
        Widget::render(paragraph, area, buf);
    }
}
