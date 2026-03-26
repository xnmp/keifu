//! Help popup widget

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

pub struct HelpPopup;

impl Widget for HelpPopup {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let key_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let desc_style = Style::default().fg(Color::White);
        let header_style = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);

        let lines = vec![
            Line::from(Span::styled("Navigation", header_style)),
            Line::from(vec![
                Span::styled("  ↑ / ↓      ", key_style),
                Span::styled("Move up/down", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  ← / →      ", key_style),
                Span::styled("Switch panels", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  ] / Tab    ", key_style),
                Span::styled("Next branch", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  [ / S-Tab  ", key_style),
                Span::styled("Previous branch", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  h / l      ", key_style),
                Span::styled("Select branch (same commit)", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  Ctrl+d/u   ", key_style),
                Span::styled("Page down/up", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  g / Home   ", key_style),
                Span::styled("Go to top", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  G / End    ", key_style),
                Span::styled("Go to bottom", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  @          ", key_style),
                Span::styled("Jump to HEAD", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  Esc        ", key_style),
                Span::styled("Return to graph / stop editing", desc_style),
            ]),
            Line::from(""),
            Line::from(Span::styled("Graph Panel", header_style)),
            Line::from(vec![
                Span::styled("  Enter      ", key_style),
                Span::styled("Open actions menu", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  Space      ", key_style),
                Span::styled("Open file select", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  b          ", key_style),
                Span::styled("Create new branch", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  d          ", key_style),
                Span::styled("Delete branch", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  f          ", key_style),
                Span::styled("Fetch from origin", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  B          ", key_style),
                Span::styled("Branch filter", desc_style),
            ]),
            Line::from(""),
            Line::from(Span::styled("Files Panel", header_style)),
            Line::from(vec![
                Span::styled("  s          ", key_style),
                Span::styled("Stage/unstage file", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  i          ", key_style),
                Span::styled("Add to .gitignore (folder in folder mode)", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  Enter      ", key_style),
                Span::styled("Open file diff", desc_style),
            ]),
            Line::from(""),
            Line::from(Span::styled("Commit Panel", header_style)),
            Line::from(vec![
                Span::styled("  Enter      ", key_style),
                Span::styled("Start editing commit message", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  Alt+Enter  ", key_style),
                Span::styled("Commit changes", desc_style),
            ]),
            Line::from(""),
            Line::from(Span::styled("Search", header_style)),
            Line::from(vec![
                Span::styled("  /          ", key_style),
                Span::styled("Search branches", desc_style),
            ]),
            Line::from(""),
            Line::from(Span::styled("Other", header_style)),
            Line::from(vec![
                Span::styled("  R          ", key_style),
                Span::styled("Refresh", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  ?          ", key_style),
                Span::styled("Toggle this help", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  Ctrl+Q     ", key_style),
                Span::styled("Quit (from anywhere)", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  q          ", key_style),
                Span::styled("Quit", desc_style),
            ]),
        ];

        let block = Block::default()
            .title(" Help ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Black));

        let paragraph = Paragraph::new(lines).block(block);

        Widget::render(paragraph, area, buf);
    }
}
