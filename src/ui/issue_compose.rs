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
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::pr_compose::text_area as compose_text_area;
use super::theme::Theme;
use crate::app::{IssueClipboardAttachment, IssueComposePurpose, IssueCreateTarget};
use crate::text_editor::TextEditor;

pub struct IssueComposeWidget<'a> {
    editor: &'a TextEditor,
    purpose: IssueComposePurpose,
    attachment: &'a IssueClipboardAttachment,
    theme: &'a Theme,
}

impl<'a> IssueComposeWidget<'a> {
    pub fn new(
        editor: &'a TextEditor,
        purpose: IssueComposePurpose,
        attachment: &'a IssueClipboardAttachment,
        theme: &'a Theme,
    ) -> Self {
        Self {
            editor,
            purpose,
            attachment,
            theme,
        }
    }

    fn title(&self) -> String {
        match self.purpose {
            IssueComposePurpose::NewIssue {
                target: IssueCreateTarget::CurrentRepository,
            } => " New Issue ".to_string(),
            IssueComposePurpose::NewIssue {
                target: IssueCreateTarget::Keifu,
            } => " Report a Keifu Issue ".to_string(),
            IssueComposePurpose::EditIssue { number } => format!(" Edit Issue #{number} "),
            IssueComposePurpose::Comment { .. } => " New Comment ".to_string(),
        }
    }

    fn header(&self) -> &'static str {
        match self.purpose {
            IssueComposePurpose::NewIssue {
                target: IssueCreateTarget::CurrentRepository,
            } => "First line = title, the rest is the body:",
            IssueComposePurpose::NewIssue {
                target: IssueCreateTarget::Keifu,
            } => "Target: xnmp/keifu · first line = title, rest = body:",
            IssueComposePurpose::EditIssue { .. } => {
                "First line = title, the rest replaces the issue body:"
            }
            IssueComposePurpose::Comment { .. } => "Comment body:",
        }
    }
}

fn checkbox_label(has_image: bool, uploader_available: bool, selected: bool) -> &'static str {
    match (has_image, uploader_available, selected) {
        (false, _, _) => "[-] Include clipboard image (no supported image in clipboard)",
        (true, false, _) => "[-] Include clipboard image (install gh-image to enable)",
        (true, true, true) => "[x] Include clipboard image",
        (true, true, false) => "[ ] Include clipboard image",
    }
}

fn compose_hint(purpose: IssueComposePurpose) -> &'static str {
    match purpose {
        IssueComposePurpose::NewIssue { .. } => {
            " Tab toggle image   Ctrl+S submit   Ctrl+E editor   Esc cancel "
        }
        IssueComposePurpose::EditIssue { .. } => " Ctrl+S save   Ctrl+E editor   Esc cancel ",
        IssueComposePurpose::Comment { .. } => " Ctrl+S submit   Ctrl+E editor   Esc cancel ",
    }
}

/// Editor rectangle used by both rendering and cursor placement. New issues
/// reserve one row for the clipboard-image checkbox.
pub fn text_area(popup: Rect, purpose: IssueComposePurpose) -> Rect {
    let base = compose_text_area(popup);
    if purpose.is_new_issue() {
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct WrappedEditorLine {
    text: String,
    logical_row: usize,
    start_byte: usize,
    end_byte: usize,
}

/// Visual layout shared by rendering and cursor placement. `scroll` is chosen
/// to keep the cursor visible when wrapping produces more rows than fit.
pub struct EditorLayout {
    lines: Vec<WrappedEditorLine>,
    scroll: usize,
    pub cursor: Option<(u16, u16)>,
}

pub fn editor_layout(editor: &TextEditor, width: u16, height: u16) -> EditorLayout {
    if width == 0 || height == 0 {
        return EditorLayout {
            lines: Vec::new(),
            scroll: 0,
            cursor: None,
        };
    }

    let mut lines: Vec<WrappedEditorLine> = editor
        .text
        .split('\n')
        .enumerate()
        .flat_map(|(logical_row, line)| {
            wrap_line(line, width as usize)
                .into_iter()
                .map(move |(start_byte, end_byte)| WrappedEditorLine {
                    text: line[start_byte..end_byte]
                        .trim_end_matches(char::is_whitespace)
                        .to_string(),
                    logical_row,
                    start_byte,
                    end_byte,
                })
        })
        .collect();

    let before_cursor = &editor.text[..editor.cursor];
    let cursor_logical_row = before_cursor.matches('\n').count();
    let logical_start = before_cursor.rfind('\n').map_or(0, |index| index + 1);
    let cursor_byte = editor.cursor - logical_start;
    let (mut cursor_visual_row, mut cursor_visual_col) = lines
        .iter()
        .enumerate()
        .find_map(|(index, line)| {
            if line.logical_row != cursor_logical_row {
                return None;
            }
            let is_last_for_logical_row = lines
                .get(index + 1)
                .is_none_or(|next| next.logical_row != cursor_logical_row);
            if cursor_byte < line.end_byte || is_last_for_logical_row {
                let relative = cursor_byte
                    .saturating_sub(line.start_byte)
                    .min(line.text.len());
                let col = UnicodeWidthStr::width(&line.text[..relative]);
                Some((index, col))
            } else {
                None
            }
        })
        .unwrap_or((0, 0));
    if cursor_visual_col >= width as usize {
        cursor_visual_row += 1;
        cursor_visual_col = 0;
        lines.insert(
            cursor_visual_row,
            WrappedEditorLine {
                text: String::new(),
                logical_row: cursor_logical_row,
                start_byte: cursor_byte,
                end_byte: cursor_byte,
            },
        );
    }

    let visible_height = height as usize;
    let max_scroll = lines.len().saturating_sub(visible_height);
    let scroll = cursor_visual_row
        .saturating_sub(visible_height.saturating_sub(1))
        .min(max_scroll);
    let visible_cursor_row = cursor_visual_row.saturating_sub(scroll);
    let cursor = (visible_cursor_row < visible_height).then_some((
        cursor_visual_col.min(u16::MAX as usize) as u16,
        visible_cursor_row as u16,
    ));

    EditorLayout {
        lines,
        scroll,
        cursor,
    }
}

/// Greedy word wrapping with hard breaks for a single overlong word. Byte
/// ranges make cursor mapping exact without modifying the underlying draft.
fn wrap_line(line: &str, width: usize) -> Vec<(usize, usize)> {
    if line.is_empty() || width == 0 {
        return vec![(0, 0)];
    }

    let mut ranges = Vec::new();
    let mut start = 0;
    while start < line.len() {
        let mut used = 0;
        let mut hard_end = start;
        let mut whitespace_end = None;
        let mut wrapped = false;

        for (relative, character) in line[start..].char_indices() {
            let char_start = start + relative;
            let char_end = char_start + character.len_utf8();
            let char_width = UnicodeWidthChar::width(character).unwrap_or(0);
            if used + char_width > width && hard_end > start {
                let end = if character.is_whitespace() && used >= width {
                    char_end
                } else {
                    whitespace_end
                        .filter(|end| *end > start)
                        .unwrap_or(hard_end)
                };
                ranges.push((start, end));
                start = end;
                wrapped = true;
                break;
            }
            used += char_width;
            hard_end = char_end;
            if character.is_whitespace() {
                whitespace_end = Some(char_end);
            }
        }

        if !wrapped {
            ranges.push((start, line.len()));
            break;
        }
    }
    ranges
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

        if self.purpose.is_new_issue() {
            let available = self.attachment.is_available();
            let checkbox = checkbox_label(
                self.attachment.image.is_some(),
                self.attachment.uploader_available,
                self.attachment.selected,
            );
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
        let has_title = self.purpose.has_title();
        let layout = editor_layout(self.editor, body.width, body.height);
        for (row, line) in layout
            .lines
            .iter()
            .skip(layout.scroll)
            .take(body.height as usize)
            .enumerate()
        {
            // Highlight the title line for new/edit issue forms.
            let style = if has_title && line.logical_row == 0 {
                Style::default()
                    .fg(self.theme.text_primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.theme.text_primary)
            };
            buf.set_stringn(
                body.x,
                body.y + row as u16,
                &line.text,
                body.width as usize,
                style,
            );
        }

        // Hint (bottom row).
        let hint = compose_hint(self.purpose);
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
        assert_eq!(
            checkbox_label(true, true, false),
            "[ ] Include clipboard image"
        );
        assert_eq!(
            checkbox_label(true, true, true),
            "[x] Include clipboard image"
        );
        assert_eq!(
            checkbox_label(false, true, false),
            "[-] Include clipboard image (no supported image in clipboard)"
        );
        assert_eq!(
            checkbox_label(true, false, false),
            "[-] Include clipboard image (install gh-image to enable)"
        );
    }

    #[test]
    fn new_issue_reserves_a_checkbox_row_above_the_editor() {
        let popup = Rect::new(10, 5, 60, 20);
        let new_issue = text_area(
            popup,
            IssueComposePurpose::NewIssue {
                target: IssueCreateTarget::CurrentRepository,
            },
        );
        let comment = text_area(popup, IssueComposePurpose::Comment { number: 1 });
        let edit = text_area(popup, IssueComposePurpose::EditIssue { number: 1 });
        assert_eq!(new_issue.y, comment.y + 1);
        assert_eq!(new_issue.height + 1, comment.height);
        assert_eq!(edit, comment);
    }

    #[test]
    fn compose_hints_expose_submit_and_cancel_controls() {
        assert_eq!(
            compose_hint(IssueComposePurpose::NewIssue {
                target: IssueCreateTarget::CurrentRepository,
            }),
            " Tab toggle image   Ctrl+S submit   Ctrl+E editor   Esc cancel "
        );
        assert_eq!(
            compose_hint(IssueComposePurpose::EditIssue { number: 1 }),
            " Ctrl+S save   Ctrl+E editor   Esc cancel "
        );
        assert_eq!(
            compose_hint(IssueComposePurpose::Comment { number: 1 }),
            " Ctrl+S submit   Ctrl+E editor   Esc cancel "
        );
    }

    #[test]
    fn editor_layout_wraps_at_words_and_hard_wraps_long_words() {
        let words = TextEditor::from_text("alpha beta gamma");
        let word_layout = editor_layout(&words, 10, 5);
        assert_eq!(
            word_layout
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha beta", "gamma"]
        );

        let long_word = TextEditor::from_text("abcdefghijkl");
        let hard_layout = editor_layout(&long_word, 5, 5);
        assert_eq!(
            hard_layout
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["abcde", "fghij", "kl"]
        );

        let exact_width = editor_layout(&TextEditor::from_text("abcde"), 5, 2);
        assert_eq!(exact_width.cursor, Some((0, 1)));
    }

    #[test]
    fn wrapped_cursor_tracks_visual_row_and_scrolls_into_view() {
        let mut editor = TextEditor::from_text("alpha beta gamma delta epsilon");
        editor.cursor = "alpha beta g".len();
        let second_line = editor_layout(&editor, 10, 5);
        assert_eq!(second_line.cursor, Some((1, 1)));

        editor.cursor = editor.text.len();
        let scrolled = editor_layout(&editor, 6, 2);
        assert_eq!(scrolled.cursor, Some((1, 1)));
        assert!(scrolled.scroll > 0);
    }

    #[test]
    fn wrapped_layout_handles_wide_unicode_and_large_pastes() {
        let wide = editor_layout(&TextEditor::from_text("界界界"), 4, 3);
        assert_eq!(
            wide.lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["界界", "界"]
        );
        assert_eq!(wide.cursor, Some((2, 1)));

        let large = TextEditor::from_text(&"word ".repeat(20_000));
        let layout = editor_layout(&large, 40, 5);
        assert!(layout.lines.len() > 1_000);
        assert!(layout.scroll > 0);
        assert!(layout.cursor.is_some());
    }
}
