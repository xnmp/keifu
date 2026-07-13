//! Help popup widget

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use super::theme::Theme;

pub struct HelpPopup<'a> {
    pub is_uncommitted: bool,
    pub theme: &'a Theme,
}

impl<'a> HelpPopup<'a> {
    pub fn new(is_uncommitted: bool, theme: &'a Theme) -> Self {
        Self {
            is_uncommitted,
            theme,
        }
    }
}

impl<'a> Widget for HelpPopup<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let key_style = Style::default()
            .fg(self.theme.help_key)
            .add_modifier(Modifier::BOLD);
        let desc_style = Style::default().fg(self.theme.text_primary);
        let header_style = Style::default()
            .fg(self.theme.help_header)
            .add_modifier(Modifier::BOLD);

        let mut lines = vec![
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
                Span::styled("Fetch from remote", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  p          ", key_style),
                Span::styled("Pull (fetch + integrate)", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  P          ", key_style),
                Span::styled("Push current branch (publishes if no upstream)", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  B          ", key_style),
                Span::styled("Branch filter", desc_style),
            ]),
            Line::from(""),
            Line::from(Span::styled("Files Panel", header_style)),
        ];

        if self.is_uncommitted {
            lines.extend([
                Line::from(vec![
                    Span::styled("  s          ", key_style),
                    Span::styled("Stage/unstage file", desc_style),
                ]),
                Line::from(vec![
                    Span::styled("  S / U      ", key_style),
                    Span::styled("Stage all / unstage all", desc_style),
                ]),
                Line::from(vec![
                    Span::styled("  i          ", key_style),
                    Span::styled("Add to .gitignore (folder in folder mode)", desc_style),
                ]),
                Line::from(vec![
                    Span::styled("  v          ", key_style),
                    Span::styled("Archive to .archive/ (folder in folder mode)", desc_style),
                ]),
                Line::from(vec![
                    Span::styled("  r          ", key_style),
                    Span::styled("Restore file (discard changes)", desc_style),
                ]),
                Line::from(vec![
                    Span::styled("  Delete     ", key_style),
                    Span::styled("Delete untracked file (recycle bin)", desc_style),
                ]),
                Line::from(vec![
                    Span::styled("  Ctrl+z     ", key_style),
                    Span::styled("Undo last file operation", desc_style),
                ]),
                Line::from(Span::styled("  Merge conflicts", header_style)),
                Line::from(vec![
                    Span::styled("  o          ", key_style),
                    Span::styled("Accept ours (on conflicted file)", desc_style),
                ]),
                Line::from(vec![
                    Span::styled("  t          ", key_style),
                    Span::styled("Accept theirs (on conflicted file)", desc_style),
                ]),
                Line::from(vec![
                    Span::styled("  c          ", key_style),
                    Span::styled("Continue merge/rebase/cherry-pick/revert", desc_style),
                ]),
                Line::from(vec![
                    Span::styled("  A          ", key_style),
                    Span::styled("Abort the in-progress operation", desc_style),
                ]),
            ]);
        }

        lines.extend([
            Line::from(vec![
                Span::styled("  Ctrl+f     ", key_style),
                Span::styled("Filter files", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  Space      ", key_style),
                Span::styled("Open file with default app", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  Enter      ", key_style),
                Span::styled("Open file diff", desc_style),
            ]),
            Line::from(""),
            Line::from(Span::styled("File Diff Viewer", header_style)),
            Line::from(vec![
                Span::styled("  [ / ]      ", key_style),
                Span::styled("Previous / next hunk", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  n / N      ", key_style),
                Span::styled("Next / previous file", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  s          ", key_style),
                Span::styled("Stage hunk under cursor", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  u          ", key_style),
                Span::styled("Unstage hunk under cursor", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  x          ", key_style),
                Span::styled("Discard hunk (working tree)", desc_style),
            ]),
            Line::from(""),
            Line::from(Span::styled("Commit Panel", header_style)),
            Line::from(vec![
                Span::styled("  ↑ / ↓      ", key_style),
                Span::styled("Scroll", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  Enter      ", key_style),
                Span::styled("Start editing commit message", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  Enter        ", key_style),
                Span::styled("Commit changes (or save amend)", desc_style),
            ]),
            Line::from(vec![
                Span::styled("  Ctrl+Enter   ", key_style),
                Span::styled("Amend last commit", desc_style),
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
        ]);

        let block = Block::default()
            .title(" Help ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.popup_border))
            .style(Style::default().bg(self.theme.popup_bg));

        let paragraph = Paragraph::new(lines).block(block);

        Widget::render(paragraph, area, buf);
    }
}
