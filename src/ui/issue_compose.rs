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

use super::pr_compose::text_area as compose_text_area;
use super::theme::Theme;
use crate::app::{IssueClipboardAttachment, IssueComposePurpose};
use crate::text_editor::TextEditor;

pub struct IssueComposeWidget<'a> {
    editor: &'a TextEditor,
    purpose: IssueComposePurpose,
    attachment: &'a IssueClipboardAttachment,
    submitting: bool,
    theme: &'a Theme,
}

impl<'a> IssueComposeWidget<'a> {
    pub fn new(
        editor: &'a TextEditor,
        purpose: IssueComposePurpose,
        attachment: &'a IssueClipboardAttachment,
        submitting: bool,
        theme: &'a Theme,
    ) -> Self {
        Self {
            editor,
            purpose,
            attachment,
            submitting,
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

fn checkbox_label(available: bool, selected: bool) -> &'static str {
    match (available, selected) {
        (true, true) => "[x] Include clipboard image",
        (true, false) => "[ ] Include clipboard image",
        (false, _) => "[-] Include clipboard image (no supported image in clipboard)",
    }
}

fn compose_hint(purpose: IssueComposePurpose, submitting: bool) -> &'static str {
    match (purpose, submitting) {
        (IssueComposePurpose::NewIssue, true) => " Submitting… please wait ",
        (IssueComposePurpose::NewIssue, false) => {
            " Tab toggle image   Ctrl+S submit   Ctrl+E editor   Esc cancel "
        }
        (IssueComposePurpose::Comment { .. }, _) => " Ctrl+S submit   Ctrl+E editor   Esc cancel ",
    }
}

/// Editor rectangle used by both rendering and cursor placement. New issues
/// reserve one row for the clipboard-image checkbox.
pub fn text_area(popup: Rect, purpose: IssueComposePurpose) -> Rect {
    let base = compose_text_area(popup);
    if matches!(purpose, IssueComposePurpose::NewIssue) {
        Rect::new(
            base.x,
            base.y.saturating_add(1),
            base.width,
            base.height.saturating_sub(1),
        )
    } else {
        base
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
            super::truncate_str(self.header(), inner.width as usize),
            Style::default().fg(self.theme.text_muted),
        );

        if matches!(self.purpose, IssueComposePurpose::NewIssue) {
            let available = self.attachment.is_available();
            let checkbox = checkbox_label(available, self.attachment.selected);
            let style = if available {
                Style::default().fg(if self.attachment.selected {
                    self.theme.pr_ci_pass
                } else {
                    self.theme.text_primary
                })
            } else {
                Style::default()
                    .fg(self.theme.text_muted)
                    .add_modifier(Modifier::DIM)
            };
            buf.set_string(
                inner.x,
                inner.y + 1,
                super::truncate_str(checkbox, inner.width as usize),
                style,
            );
        }

        // Editor lines.
        let body = self::text_area(area, self.purpose);
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
            buf.set_string(
                body.x,
                body.y + row as u16,
                super::truncate_str(line, body.width as usize),
                style,
            );
        }

        // Hint (bottom row).
        let hint = compose_hint(self.purpose, self.submitting);
        let fy = inner.y + inner.height - 1;
        buf.set_string(
            inner.x,
            fy,
            super::truncate_str(hint, inner.width as usize),
            Style::default().fg(self.theme.text_muted),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_checkbox_distinguishes_enabled_checked_and_disabled() {
        assert_eq!(checkbox_label(true, false), "[ ] Include clipboard image");
        assert_eq!(checkbox_label(true, true), "[x] Include clipboard image");
        assert_eq!(
            checkbox_label(false, false),
            "[-] Include clipboard image (no supported image in clipboard)"
        );
    }

    #[test]
    fn new_issue_reserves_a_checkbox_row_above_the_editor() {
        let popup = Rect::new(10, 5, 60, 20);
        let new_issue = text_area(popup, IssueComposePurpose::NewIssue);
        let comment = text_area(popup, IssueComposePurpose::Comment { number: 1 });
        assert_eq!(new_issue.y, comment.y + 1);
        assert_eq!(new_issue.height + 1, comment.height);
    }

    #[test]
    fn submitted_new_issue_does_not_claim_escape_will_cancel_the_worker() {
        let hint = compose_hint(IssueComposePurpose::NewIssue, true);
        assert!(hint.contains("Submitting"));
        assert!(!hint.contains("Esc"));
        assert_eq!(
            compose_hint(IssueComposePurpose::Comment { number: 1 }, true),
            " Ctrl+S submit   Ctrl+E editor   Esc cancel "
        );
    }
}
