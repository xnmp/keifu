//! Issue-compose editor popup (new issue title/body, or a comment body).
//!
//! A small sibling to `pr_compose`: the PR widget is bound to `ComposePurpose`,
//! so issues get their own thin widget rather than an awkward generalization.
//! Cursor placement reuses `pr_compose::text_area` since the layout is identical.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Clear, Widget},
};

use super::pr_compose::text_area;
use super::theme::Theme;
use crate::app::IssueComposePurpose;
use crate::text_editor::TextEditor;

pub struct IssueComposeWidget<'a> {
    editor: &'a TextEditor,
    purpose: IssueComposePurpose,
    theme: &'a Theme,
}

impl<'a> IssueComposeWidget<'a> {
    pub fn new(editor: &'a TextEditor, purpose: IssueComposePurpose, theme: &'a Theme) -> Self {
        Self {
            editor,
            purpose,
            theme,
        }
    }

    fn title(&self) -> &'static str {
        match self.purpose {
            IssueComposePurpose::NewIssue => " New Issue ",
            IssueComposePurpose::Comment { .. } => " New Comment ",
        }
    }

    fn header(&self) -> &'static str {
        match self.purpose {
            IssueComposePurpose::NewIssue => "First line = title, the rest is the body:",
            IssueComposePurpose::Comment { .. } => "Comment body:",
        }
    }
}

impl<'a> Widget for IssueComposeWidget<'a> {
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
        let is_new = matches!(self.purpose, IssueComposePurpose::NewIssue);
        for (row, line) in self.editor.lines().iter().enumerate() {
            if row as u16 >= body.height {
                break;
            }
            // Highlight the title line (row 0) for a new issue.
            let style = if is_new && row == 0 {
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
