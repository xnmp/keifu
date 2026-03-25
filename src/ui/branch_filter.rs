//! Branch filter popup widget

use std::collections::HashSet;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Widget},
};

pub struct BranchFilterWidget<'a> {
    all_branches: &'a [String],
    hidden_branches: &'a HashSet<String>,
    filter: &'a str,
    selected: usize,
}

impl<'a> BranchFilterWidget<'a> {
    pub fn new(
        all_branches: &'a [String],
        hidden_branches: &'a HashSet<String>,
        filter: &'a str,
        selected: usize,
    ) -> Self {
        Self {
            all_branches,
            hidden_branches,
            filter,
            selected,
        }
    }

    fn filtered_branches(&self) -> Vec<&'a String> {
        self.all_branches
            .iter()
            .filter(|b| b.to_lowercase().contains(&self.filter.to_lowercase()))
            .collect()
    }
}

impl<'a> Widget for BranchFilterWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let title = if self.filter.is_empty() {
            " Branch Filter ".to_string()
        } else {
            format!(" Branch Filter [{}] ", self.filter)
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height < 2 {
            return;
        }

        let filtered = self.filtered_branches();

        // Reserve last row for footer
        let list_height = inner.height.saturating_sub(1) as usize;

        for (i, branch) in filtered.iter().enumerate() {
            if i >= list_height {
                break;
            }

            let y = inner.y + i as u16;
            let is_selected = i == self.selected;
            let is_visible = !self.hidden_branches.contains(branch.as_str());

            let checkbox = if is_visible { "[x] " } else { "[ ] " };
            let label = format!("{}{}", checkbox, branch);

            let style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if is_visible {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let line = Line::from(Span::styled(label, style));
            buf.set_line(inner.x, y, &line, inner.width);

            if is_selected {
                buf.set_style(Rect::new(inner.x, y, inner.width, 1), style);
            }
        }

        // Footer
        let footer_y = inner.y + inner.height.saturating_sub(1);
        let footer_style = Style::default().fg(Color::DarkGray);
        let footer = Line::from(Span::styled(
            "Space: toggle  a: all  n: none  Esc: close",
            footer_style,
        ));
        buf.set_line(inner.x, footer_y, &footer, inner.width);
    }
}
