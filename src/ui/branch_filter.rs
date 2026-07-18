//! Branch filter popup widget

use std::collections::{HashMap, HashSet};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Widget},
};

use super::theme::Theme;

/// Whether a branch matches the picker's live filter.
///
/// A filter beginning with `@` matches the branch's `author` (case-insensitive
/// substring, `@` stripped); any other filter matches the branch `name`
/// (case-insensitive substring). An empty filter — or a bare `@` — matches
/// everything.
///
/// Shared by the widget (display), the action handler (navigation + bulk ops)
/// and the popup-size calculation so all three agree on the visible set.
pub fn matches_branch_filter(name: &str, author: &str, filter: &str) -> bool {
    match filter.strip_prefix('@') {
        Some(author_query) => author.to_lowercase().contains(&author_query.to_lowercase()),
        None => name.to_lowercase().contains(&filter.to_lowercase()),
    }
}

pub struct BranchFilterWidget<'a> {
    all_branches: &'a [String],
    hidden_branches: &'a HashSet<String>,
    branch_authors: &'a HashMap<String, String>,
    filter: &'a str,
    selected: usize,
    theme: &'a Theme,
}

impl<'a> BranchFilterWidget<'a> {
    pub fn new(
        all_branches: &'a [String],
        hidden_branches: &'a HashSet<String>,
        branch_authors: &'a HashMap<String, String>,
        filter: &'a str,
        selected: usize,
        theme: &'a Theme,
    ) -> Self {
        Self {
            all_branches,
            hidden_branches,
            branch_authors,
            filter,
            selected,
            theme,
        }
    }

    fn author_of(&self, branch: &str) -> &'a str {
        self.branch_authors
            .get(branch)
            .map(String::as_str)
            .unwrap_or("")
    }

    fn filtered_branches(&self) -> Vec<&'a String> {
        self.all_branches
            .iter()
            .filter(|b| matches_branch_filter(b, self.author_of(b), self.filter))
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
            .border_style(Style::default().fg(self.theme.popup_border))
            .style(Style::default().bg(self.theme.popup_bg));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height < 2 {
            return;
        }

        let filtered = self.filtered_branches();

        // Reserve last row for footer
        let list_height = inner.height.saturating_sub(1) as usize;

        // Calculate scroll offset to keep selected item visible
        let scroll_offset = if self.selected >= list_height {
            self.selected - list_height + 1
        } else {
            0
        };

        let width = inner.width as usize;
        let muted = Style::default().fg(self.theme.text_muted);

        for (i, branch) in filtered.iter().skip(scroll_offset).enumerate() {
            if i >= list_height {
                break;
            }

            let actual_index = i + scroll_offset;
            let y = inner.y + i as u16;
            let is_selected = actual_index == self.selected;
            let is_visible = !self.hidden_branches.contains(branch.as_str());

            let checkbox = if is_visible { "[x] " } else { "[ ] " };
            let name = format!("{}{}", checkbox, branch);
            let author = self.author_of(branch);

            let name_style = if is_selected {
                self.theme.list_selection_style()
            } else if is_visible {
                Style::default().fg(self.theme.text_primary)
            } else {
                muted
            };

            // Right-align the author after the name when both fit; otherwise
            // just render the (possibly clipped) name.
            let name_w = name.chars().count();
            let author_w = author.chars().count();
            let spans = if !author.is_empty() && name_w + 1 + author_w <= width {
                let gap = width - name_w - author_w;
                vec![
                    Span::styled(name, name_style),
                    Span::styled(" ".repeat(gap), name_style),
                    // Selection restyles the whole row below, so muted here
                    // only affects non-selected rows.
                    Span::styled(author.to_string(), muted),
                ]
            } else {
                vec![Span::styled(name, name_style)]
            };

            let line = Line::from(spans);
            buf.set_line(inner.x, y, &line, inner.width);

            if is_selected {
                buf.set_style(
                    Rect::new(inner.x, y, inner.width, 1),
                    self.theme.list_selection_style(),
                );
            }
        }

        // Footer
        let footer_y = inner.y + inner.height.saturating_sub(1);
        let footer_style = Style::default().fg(self.theme.text_muted);
        let footer = Line::from(Span::styled(
            "Space toggle  ^a all  ^o none  @author filter  Esc close",
            footer_style,
        ));
        buf.set_line(inner.x, footer_y, &footer, inner.width);
    }
}
