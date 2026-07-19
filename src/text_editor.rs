//! Simple multi-line text editor with cursor, selection, and word navigation.

/// Selection anchor + cursor range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Byte offset where selection started.
    pub anchor: usize,
    /// Byte offset where cursor currently sits.
    pub cursor: usize,
}

impl Selection {
    /// Returns (start, end) with start <= end.
    pub fn ordered(&self) -> (usize, usize) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }
}

/// A simple multi-line text editor for editing commit messages in a TUI.
#[derive(Debug, Clone, Default)]
pub struct TextEditor {
    /// The text buffer (UTF-8).
    pub text: String,
    /// Byte offset of the cursor within `text`.
    pub cursor: usize,
    /// Optional selection (anchor + cursor).
    pub selection: Option<Selection>,
}

impl TextEditor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an editor pre-filled with text, cursor at end.
    pub fn from_text(text: &str) -> Self {
        let text = text.to_string();
        let cursor = text.len();
        Self {
            text,
            cursor,
            selection: None,
        }
    }

    /// Returns (row, col) where row is 0-indexed line number and col is character offset.
    pub fn cursor_position(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let row = before.matches('\n').count();
        let last_newline = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let col = before[last_newline..].chars().count();
        (row, col)
    }

    /// Returns the lines of text.
    pub fn lines(&self) -> Vec<&str> {
        if self.text.is_empty() {
            return vec![""];
        }
        self.text.split('\n').collect()
    }

    pub fn line_count(&self) -> usize {
        if self.text.is_empty() {
            1
        } else {
            self.text.matches('\n').count() + 1
        }
    }

    // ── Editing ──────────────────────────────────────────────────────

    pub fn insert_char(&mut self, c: char) {
        self.delete_selection();
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// Insert a whole string at the cursor (replacing any selection). Used for
    /// bracketed paste; `s` may contain newlines.
    pub fn insert_str(&mut self, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor > 0 {
            let prev = prev_char_boundary(&self.text, self.cursor);
            self.text.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor < self.text.len() {
            let next = next_char_boundary(&self.text, self.cursor);
            self.text.drain(self.cursor..next);
        }
    }

    pub fn backspace_word(&mut self) {
        if self.delete_selection() {
            return;
        }
        let target = word_boundary_left(&self.text, self.cursor);
        if target == self.cursor {
            // At a line boundary — fall back to regular backspace (join lines)
            self.backspace();
        } else {
            self.text.drain(target..self.cursor);
            self.cursor = target;
        }
    }

    pub fn delete_word(&mut self) {
        if self.delete_selection() {
            return;
        }
        let target = word_boundary_right(&self.text, self.cursor);
        self.text.drain(self.cursor..target);
    }

    pub fn kill_line(&mut self) {
        self.selection = None;
        let line_start = self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        self.text.drain(line_start..self.cursor);
        self.cursor = line_start;
    }

    // ── Movement ─────────────────────────────────────────────────────

    pub fn move_left(&mut self, shift: bool) {
        if !shift {
            if let Some(sel) = self.selection.take() {
                let (start, _) = sel.ordered();
                self.cursor = start;
                return;
            }
        }
        let anchor = self.begin_move(shift);
        if self.cursor > 0 {
            self.cursor = prev_char_boundary(&self.text, self.cursor);
        }
        self.finish_move(anchor, shift);
    }

    pub fn move_right(&mut self, shift: bool) {
        if !shift {
            if let Some(sel) = self.selection.take() {
                let (_, end) = sel.ordered();
                self.cursor = end;
                return;
            }
        }
        let anchor = self.begin_move(shift);
        if self.cursor < self.text.len() {
            self.cursor = next_char_boundary(&self.text, self.cursor);
        }
        self.finish_move(anchor, shift);
    }

    pub fn move_word_left(&mut self, shift: bool) {
        let anchor = self.begin_move(shift);
        self.cursor = word_boundary_left(&self.text, self.cursor);
        self.finish_move(anchor, shift);
    }

    pub fn move_word_right(&mut self, shift: bool) {
        let anchor = self.begin_move(shift);
        self.cursor = word_boundary_right(&self.text, self.cursor);
        self.finish_move(anchor, shift);
    }

    pub fn move_up(&mut self, shift: bool) {
        let anchor = self.begin_move(shift);
        let (row, col) = self.cursor_position();
        if row > 0 {
            self.cursor = offset_at(&self.text, row - 1, col);
        } else {
            self.cursor = 0;
        }
        self.finish_move(anchor, shift);
    }

    pub fn move_down(&mut self, shift: bool) {
        let anchor = self.begin_move(shift);
        let (row, col) = self.cursor_position();
        if row + 1 < self.line_count() {
            self.cursor = offset_at(&self.text, row + 1, col);
        } else {
            self.cursor = self.text.len();
        }
        self.finish_move(anchor, shift);
    }

    pub fn move_home(&mut self, shift: bool) {
        let anchor = self.begin_move(shift);
        let before = &self.text[..self.cursor];
        self.cursor = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        self.finish_move(anchor, shift);
    }

    pub fn move_end(&mut self, shift: bool) {
        let anchor = self.begin_move(shift);
        let after = &self.text[self.cursor..];
        self.cursor += after.find('\n').unwrap_or(after.len());
        self.finish_move(anchor, shift);
    }

    pub fn move_text_start(&mut self, shift: bool) {
        let anchor = self.begin_move(shift);
        self.cursor = 0;
        self.finish_move(anchor, shift);
    }

    pub fn move_text_end(&mut self, shift: bool) {
        let anchor = self.begin_move(shift);
        self.cursor = self.text.len();
        self.finish_move(anchor, shift);
    }

    // ── Selection ────────────────────────────────────────────────────

    pub fn has_selection(&self) -> bool {
        self.selection
            .is_some_and(|s| s.anchor != s.cursor)
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection.and_then(|sel| {
            let (start, end) = sel.ordered();
            if start != end {
                Some(&self.text[start..end])
            } else {
                None
            }
        })
    }

    /// Deletes selected text if any. Returns true if something was deleted.
    pub fn delete_selection(&mut self) -> bool {
        let sel = match self.selection.take() {
            Some(s) if s.anchor != s.cursor => s,
            _ => return false,
        };
        let (start, end) = sel.ordered();
        self.text.drain(start..end);
        self.cursor = start;
        true
    }

    // ── Internal helpers ─────────────────────────────────────────────

    /// Prepares for a movement: if shift, returns the anchor to use;
    /// if not shift, clears selection. Returns `Some(anchor)` if shift is held.
    fn begin_move(&mut self, shift: bool) -> Option<usize> {
        if shift {
            let anchor = match self.selection {
                Some(sel) => sel.anchor,
                None => self.cursor,
            };
            Some(anchor)
        } else {
            self.selection = None;
            None
        }
    }

    /// Finalizes a movement: if shift, sets the selection from anchor to cursor.
    fn finish_move(&mut self, anchor: Option<usize>, shift: bool) {
        if shift {
            if let Some(a) = anchor {
                self.selection = Some(Selection {
                    anchor: a,
                    cursor: self.cursor,
                });
            }
        }
    }
}

// ── Free helper functions ────────────────────────────────────────────

fn prev_char_boundary(text: &str, offset: usize) -> usize {
    let mut pos = offset.saturating_sub(1);
    while pos > 0 && !text.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

fn next_char_boundary(text: &str, offset: usize) -> usize {
    let mut pos = offset + 1;
    while pos < text.len() && !text.is_char_boundary(pos) {
        pos += 1;
    }
    pos.min(text.len())
}

/// Word boundary left: spaces only. Skip spaces, then skip non-space/non-newline.
pub fn word_boundary_left(text: &str, from: usize) -> usize {
    if from == 0 {
        return 0;
    }
    let bytes = text.as_bytes();
    let mut pos = from;
    while pos > 0 && bytes[pos - 1] == b' ' {
        pos -= 1;
    }
    while pos > 0 && bytes[pos - 1] != b' ' && bytes[pos - 1] != b'\n' {
        pos -= 1;
    }
    pos
}

/// Word boundary right: spaces only. Skip non-space/non-newline, then skip spaces.
fn word_boundary_right(text: &str, from: usize) -> usize {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut pos = from;
    while pos < len && bytes[pos] != b' ' && bytes[pos] != b'\n' {
        pos += 1;
    }
    while pos < len && bytes[pos] == b' ' {
        pos += 1;
    }
    pos
}

/// Delete the last word from a string (cursor implicitly at end).
pub fn pop_word(s: &mut String) {
    let target = word_boundary_left(s, s.len());
    s.truncate(target);
}

/// Get byte offset for (row, col) where col is in characters. Clamps to line length.
fn offset_at(text: &str, target_row: usize, target_col: usize) -> usize {
    let mut row = 0;
    let mut line_start = 0;
    for (i, c) in text.char_indices() {
        if row == target_row {
            let col_in_line = text[line_start..i].chars().count();
            if col_in_line >= target_col {
                return i;
            }
        }
        if c == '\n' {
            if row == target_row {
                return i; // clamp to end of line
            }
            row += 1;
            line_start = i + 1;
        }
    }
    if row == target_row {
        let remaining = &text[line_start..];
        let chars_count = remaining.chars().count();
        if target_col <= chars_count {
            let byte_offset: usize = remaining
                .chars()
                .take(target_col)
                .map(|c| c.len_utf8())
                .sum();
            return line_start + byte_offset;
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Delegates to the real constructor so `from_text` gets exercised by
    // every test that uses this helper.
    fn editor_with(text: &str) -> TextEditor {
        TextEditor::from_text(text)
    }

    fn editor_at(text: &str, cursor: usize) -> TextEditor {
        TextEditor {
            text: text.to_string(),
            cursor,
            selection: None,
        }
    }

    // ── Construction ─────────────────────────────────────────────────

    #[test]
    fn new_editor_is_empty() {
        let ed = TextEditor::new();
        assert_eq!(ed.text, "");
        assert_eq!(ed.cursor, 0);
        assert!(ed.selection.is_none());
        assert_eq!(ed.cursor_position(), (0, 0));
        assert_eq!(ed.lines(), vec![""]);
        assert_eq!(ed.line_count(), 1);
    }

    // ── Lines ────────────────────────────────────────────────────────

    #[test]
    fn lines_single() {
        let ed = editor_with("hello");
        assert_eq!(ed.lines(), vec!["hello"]);
        assert_eq!(ed.line_count(), 1);
    }

    #[test]
    fn lines_multi() {
        let ed = editor_with("a\nb\nc");
        assert_eq!(ed.lines(), vec!["a", "b", "c"]);
        assert_eq!(ed.line_count(), 3);
    }

    #[test]
    fn lines_trailing_newline() {
        let ed = editor_with("a\n");
        assert_eq!(ed.lines(), vec!["a", ""]);
        assert_eq!(ed.line_count(), 2);
    }

    // ── Cursor position ──────────────────────────────────────────────

    #[test]
    fn cursor_position_start() {
        let ed = editor_at("hello\nworld", 0);
        assert_eq!(ed.cursor_position(), (0, 0));
    }

    #[test]
    fn cursor_position_mid_line() {
        let ed = editor_at("hello\nworld", 3);
        assert_eq!(ed.cursor_position(), (0, 3));
    }

    #[test]
    fn cursor_position_second_line() {
        let ed = editor_at("hello\nworld", 8);
        assert_eq!(ed.cursor_position(), (1, 2));
    }

    // ── Insert ───────────────────────────────────────────────────────

    #[test]
    fn insert_char_appends() {
        let mut ed = editor_with("ab");
        ed.insert_char('c');
        assert_eq!(ed.text, "abc");
        assert_eq!(ed.cursor, 3);
    }

    #[test]
    fn insert_char_at_start() {
        let mut ed = editor_at("bc", 0);
        ed.insert_char('a');
        assert_eq!(ed.text, "abc");
        assert_eq!(ed.cursor, 1);
    }

    #[test]
    fn insert_newline_splits_line() {
        let mut ed = editor_at("ab", 1);
        ed.insert_newline();
        assert_eq!(ed.text, "a\nb");
        assert_eq!(ed.cursor_position(), (1, 0));
    }

    #[test]
    fn insert_replaces_selection() {
        let mut ed = editor_at("hello", 1);
        ed.selection = Some(Selection { anchor: 1, cursor: 4 });
        ed.insert_char('X');
        assert_eq!(ed.text, "hXo");
        assert!(ed.selection.is_none());
    }

    // ── Backspace / Delete ───────────────────────────────────────────

    #[test]
    fn backspace_at_start_is_noop() {
        let mut ed = editor_at("abc", 0);
        ed.backspace();
        assert_eq!(ed.text, "abc");
    }

    #[test]
    fn backspace_removes_prev_char() {
        let mut ed = editor_at("abc", 2);
        ed.backspace();
        assert_eq!(ed.text, "ac");
        assert_eq!(ed.cursor, 1);
    }

    #[test]
    fn delete_at_end_is_noop() {
        let mut ed = editor_with("abc");
        ed.delete();
        assert_eq!(ed.text, "abc");
    }

    #[test]
    fn delete_removes_next_char() {
        let mut ed = editor_at("abc", 1);
        ed.delete();
        assert_eq!(ed.text, "ac");
        assert_eq!(ed.cursor, 1);
    }

    #[test]
    fn backspace_deletes_selection() {
        let mut ed = editor_at("abcde", 1);
        ed.selection = Some(Selection { anchor: 1, cursor: 3 });
        ed.backspace();
        assert_eq!(ed.text, "ade");
        assert_eq!(ed.cursor, 1);
    }

    // ── Word operations ──────────────────────────────────────────────

    #[test]
    fn word_boundaries_spaces_only() {
        let text = "abc####def ghi";
        assert_eq!(word_boundary_left(text, 14), 11);
        assert_eq!(word_boundary_left(text, 11), 0);
        assert_eq!(word_boundary_right(text, 0), 11);
        assert_eq!(word_boundary_right(text, 11), 14);
    }

    #[test]
    fn backspace_word_removes_last_word() {
        let mut ed = editor_with("hello world");
        ed.backspace_word();
        assert_eq!(ed.text, "hello ");
    }

    #[test]
    fn delete_word_removes_next_word() {
        let mut ed = editor_at("hello world", 0);
        ed.delete_word();
        assert_eq!(ed.text, "world");
    }

    // ── Movement ─────────────────────────────────────────────────────

    #[test]
    fn move_left_right() {
        let mut ed = editor_at("abc", 1);
        ed.move_left(false);
        assert_eq!(ed.cursor, 0);
        ed.move_right(false);
        assert_eq!(ed.cursor, 1);
    }

    #[test]
    fn move_left_at_start_is_noop() {
        let mut ed = editor_at("abc", 0);
        ed.move_left(false);
        assert_eq!(ed.cursor, 0);
    }

    #[test]
    fn move_right_at_end_is_noop() {
        let mut ed = editor_with("abc");
        ed.move_right(false);
        assert_eq!(ed.cursor, 3);
    }

    #[test]
    fn move_up_down() {
        let mut ed = editor_at("hello\nworld\nfoo", 8); // row 1, col 2
        ed.move_up(false);
        assert_eq!(ed.cursor_position(), (0, 2));
        ed.move_down(false);
        assert_eq!(ed.cursor_position(), (1, 2));
    }

    #[test]
    fn move_up_clamps_column() {
        let mut ed = editor_at("ab\nlong line", 7); // row 1, col 4
        ed.move_up(false);
        assert_eq!(ed.cursor_position(), (0, 2)); // clamped to line length
    }

    #[test]
    fn move_up_from_first_line_goes_to_start() {
        let mut ed = editor_at("abc", 2);
        ed.move_up(false);
        assert_eq!(ed.cursor, 0);
    }

    #[test]
    fn move_down_from_last_line_goes_to_end() {
        let mut ed = editor_at("abc", 1);
        ed.move_down(false);
        assert_eq!(ed.cursor, 3);
    }

    #[test]
    fn move_home_end() {
        let mut ed = editor_at("hello\nworld", 8);
        ed.move_home(false);
        assert_eq!(ed.cursor_position(), (1, 0));
        ed.move_end(false);
        assert_eq!(ed.cursor_position(), (1, 5));
    }

    #[test]
    fn move_text_start_end() {
        let mut ed = editor_at("hello\nworld", 4);
        ed.move_text_start(false);
        assert_eq!(ed.cursor, 0);
        ed.move_text_end(false);
        assert_eq!(ed.cursor, 11);
    }

    #[test]
    fn move_word_left_right() {
        let mut ed = editor_at("one two three", 7);
        ed.move_word_left(false);
        assert_eq!(ed.cursor, 4);
        ed.move_word_right(false);
        assert_eq!(ed.cursor, 8);
    }

    // ── Selection ────────────────────────────────────────────────────

    #[test]
    fn shift_creates_selection() {
        let mut ed = editor_at("hello", 0);
        ed.move_right(true);
        ed.move_right(true);
        ed.move_right(true);
        assert!(ed.has_selection());
        assert_eq!(ed.selected_text(), Some("hel"));
        assert_eq!(ed.selection, Some(Selection { anchor: 0, cursor: 3 }));
    }

    #[test]
    fn non_shift_clears_selection_and_collapses_right() {
        let mut ed = editor_at("hello", 0);
        ed.move_right(true);
        ed.move_right(true);
        assert!(ed.has_selection());
        ed.move_right(false); // collapses to end of selection
        assert!(!ed.has_selection());
        assert_eq!(ed.cursor, 2);
    }

    #[test]
    fn move_left_collapses_selection_to_start() {
        let mut ed = editor_at("hello", 1);
        ed.selection = Some(Selection { anchor: 1, cursor: 4 });
        ed.move_left(false);
        assert_eq!(ed.cursor, 1);
        assert!(!ed.has_selection());
    }

    #[test]
    fn selection_ordered_reversed() {
        let sel = Selection { anchor: 5, cursor: 2 };
        assert_eq!(sel.ordered(), (2, 5));
    }

    #[test]
    fn selected_text_reversed_direction() {
        let mut ed = editor_at("hello world", 5);
        ed.selection = Some(Selection { anchor: 5, cursor: 0 });
        assert_eq!(ed.selected_text(), Some("hello"));
    }

    #[test]
    fn collapsed_selection_is_not_active() {
        let mut ed = editor_at("abc", 1);
        ed.selection = Some(Selection { anchor: 1, cursor: 1 });
        assert!(!ed.has_selection());
        assert_eq!(ed.selected_text(), None);
    }

    #[test]
    fn delete_selection_returns_false_when_none() {
        let mut ed = editor_with("abc");
        assert!(!ed.delete_selection());
    }

    #[test]
    fn delete_selection_removes_range() {
        let mut ed = editor_at("abcdef", 0);
        ed.selection = Some(Selection { anchor: 1, cursor: 4 });
        assert!(ed.delete_selection());
        assert_eq!(ed.text, "aef");
        assert_eq!(ed.cursor, 1);
    }

    // ── UTF-8 ────────────────────────────────────────────────────────

    #[test]
    fn multibyte_insert_and_move() {
        let mut ed = TextEditor::new();
        ed.insert_char('a');
        ed.insert_char('\u{00e9}'); // é (2 bytes)
        ed.insert_char('b');
        assert_eq!(ed.text, "a\u{00e9}b");
        assert_eq!(ed.cursor, 4); // a(1) + é(2) + b(1)
        ed.move_left(false);
        assert_eq!(ed.cursor, 3); // before 'b'
        ed.move_left(false);
        assert_eq!(ed.cursor, 1); // before 'é'
    }

    // ── Emoji / multi-byte codepoints ─────────────────────────────────

    #[test]
    fn emoji_insert_and_move_keeps_valid_utf8_and_moves_whole_codepoint() {
        let mut ed = TextEditor::new();
        ed.insert_char('a');
        ed.insert_char('\u{1F600}'); // 😀, 4 bytes
        ed.insert_char('b');
        assert_eq!(ed.text, "a\u{1F600}b");
        assert_eq!(ed.cursor, 6); // a(1) + emoji(4) + b(1)
        assert!(ed.text.is_char_boundary(ed.cursor));

        ed.move_left(false);
        assert_eq!(ed.cursor, 5); // before 'b', right after the emoji
        assert!(ed.text.is_char_boundary(ed.cursor));

        ed.move_left(false);
        assert_eq!(ed.cursor, 1); // before the emoji — jumped all 4 bytes at once
        assert!(ed.text.is_char_boundary(ed.cursor));

        ed.move_right(false);
        assert_eq!(ed.cursor, 5); // after the emoji again
        assert!(ed.text.is_char_boundary(ed.cursor));
    }

    #[test]
    fn emoji_backspace_removes_whole_codepoint() {
        let mut ed = editor_at("a\u{1F600}b", 5); // cursor right after the emoji
        ed.backspace();
        assert_eq!(ed.text, "ab");
        assert_eq!(ed.cursor, 1);
        assert!(ed.text.is_char_boundary(ed.cursor));
    }

    #[test]
    fn emoji_delete_removes_whole_codepoint() {
        let mut ed = editor_at("a\u{1F600}b", 1); // cursor right before the emoji
        ed.delete();
        assert_eq!(ed.text, "ab");
        assert_eq!(ed.cursor, 1);
        assert!(ed.text.is_char_boundary(ed.cursor));
    }

    #[test]
    fn combining_sequence_moves_per_codepoint_not_grapheme() {
        // "e" + combining acute accent (U+0301) is a single grapheme cluster
        // visually, but two separate Unicode scalar values. This editor
        // moves per-codepoint, not per-grapheme, so clearing the whole
        // visual character from the end takes two `move_left` calls. This
        // pins the CURRENT behavior (arguably not ideal UX) rather than
        // asserting it's correct.
        let mut ed = editor_with("e\u{0301}"); // cursor at end (byte 3)
        assert_eq!(ed.cursor, 3);
        ed.move_left(false);
        assert_eq!(ed.cursor, 1); // stopped between 'e' and the combining mark
        ed.move_left(false);
        assert_eq!(ed.cursor, 0);
    }

    #[test]
    fn combining_sequence_backspace_removes_mark_before_base_char() {
        // Same per-codepoint behavior as above, via backspace: the
        // combining mark is deleted first, leaving the bare base character.
        let mut ed = editor_with("e\u{0301}");
        ed.backspace();
        assert_eq!(ed.text, "e");
        ed.backspace();
        assert_eq!(ed.text, "");
    }

    // ── Empty buffer / huge line ───────────────────────────────────────

    #[test]
    fn empty_buffer_vertical_and_home_end_movement_is_noop() {
        let mut ed = TextEditor::new();
        ed.move_up(false);
        assert_eq!(ed.cursor, 0);
        ed.move_down(false);
        assert_eq!(ed.cursor, 0);
        ed.move_home(false);
        assert_eq!(ed.cursor, 0);
        ed.move_end(false);
        assert_eq!(ed.cursor, 0);
    }

    #[test]
    fn huge_line_move_end_and_home() {
        let text = "a".repeat(10_000);
        let mut ed = editor_at(&text, 0);
        ed.move_end(false);
        assert_eq!(ed.cursor, 10_000);
        ed.move_home(false);
        assert_eq!(ed.cursor, 0);
    }

    // ── pop_word ────────────────────────────────────────────────────

    #[test]
    fn pop_word_removes_last_word() {
        let mut s = "hello world".to_string();
        pop_word(&mut s);
        assert_eq!(s, "hello ");
    }

    #[test]
    fn pop_word_removes_trailing_spaces_then_word() {
        let mut s = "hello   ".to_string();
        pop_word(&mut s);
        assert_eq!(s, "");
    }

    #[test]
    fn pop_word_on_empty_string() {
        let mut s = String::new();
        pop_word(&mut s);
        assert_eq!(s, "");
    }

    #[test]
    fn pop_word_single_word() {
        let mut s = "word".to_string();
        pop_word(&mut s);
        assert_eq!(s, "");
    }

    // ── kill_line ───────────────────────────────────────────────────

    #[test]
    fn kill_line_deletes_to_line_start() {
        let mut ed = editor_at("hello world", 5);
        ed.kill_line();
        assert_eq!(ed.text, " world");
        assert_eq!(ed.cursor, 0);
    }

    #[test]
    fn kill_line_on_second_line() {
        let mut ed = editor_at("first\nsecond", 9);
        ed.kill_line();
        assert_eq!(ed.text, "first\nond");
        assert_eq!(ed.cursor, 6);
    }

    #[test]
    fn kill_line_at_line_start_is_noop() {
        let mut ed = editor_at("hello", 0);
        ed.kill_line();
        assert_eq!(ed.text, "hello");
        assert_eq!(ed.cursor, 0);
    }
}
