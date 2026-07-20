//! Help popup widget

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;

/// One row of the help sheet.
enum HelpEntry {
    /// A section heading (e.g. "Navigation").
    Header(&'static str),
    /// A `(key, description)` binding row.
    Row(&'static str, &'static str),
    /// Vertical spacer between sections.
    Blank,
}

/// Minimum gap (in columns) between the key column and its description, so the
/// two never collide even for the longest key label.
const KEY_GAP: usize = 2;

/// The rendered width of the key column: the widest key label plus [`KEY_GAP`].
/// Pure so the layout can be unit-tested independently of rendering.
fn key_column_width(entries: &[HelpEntry]) -> usize {
    let widest = entries
        .iter()
        .filter_map(|e| match e {
            HelpEntry::Row(key, _) => Some(UnicodeWidthStr::width(*key)),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    widest + KEY_GAP
}

/// The help entries for the current context (uncommitted adds the staging and
/// merge-conflict rows). Order matches the on-screen sections.
fn entries(is_uncommitted: bool) -> Vec<HelpEntry> {
    use HelpEntry::{Blank, Header, Row};
    let mut e = vec![
        Header("Navigation"),
        Row("↑ / ↓", "Move up/down"),
        Row("← / →", "Switch panels"),
        Row("Tab / Shift+Tab", "Switch panels (forward/back)"),
        Row("Ctrl+d/u", "Page down/up"),
        Row("g / Home", "Go to top"),
        Row("Shift+G / End", "Go to bottom"),
        Row("@", "Jump to HEAD"),
        Row("Esc", "Return to graph / stop editing / quit (from graph)"),
        Blank,
        Header("Graph Panel"),
        Row("Enter", "Open actions menu"),
        Row("Space", "Open file select"),
        Row("] / [", "Next / previous branch label"),
        Row("b", "Create new branch"),
        Row("d", "Delete branch"),
        Row("f", "Fetch from remote"),
        Row("p", "Pull (fetch + integrate)"),
        Row("Shift+P", "Push current branch (publishes if no upstream)"),
        Row(
            "Shift+B",
            "Branch filter (type to filter by name, @ by author)",
        ),
        Row("Shift+O", "Show/hide remote-only branches"),
        Row("Ctrl+f", "Filter commits (message/author/hash)"),
        Row("m", "Mark / compare two commits (Esc clears)"),
        Row("o", "Open PR in browser (badge color = CI: green/yellow/red)"),
        Row("c", "CI check details (see failure logs without a browser)"),
        Row("v", "View PR conversation (comments, reviews, threads)"),
        Row(
            "Shift+M",
            "Toggle author/hash/date, muted merges & avatars",
        ),
        Row("< / >", "Shrink / widen the graph column (… = truncated)"),
        Row("t", "Toggle branch tracing (dim off-lineage lanes)"),
        Row("^", "Jump to fork point (merge base with main / HEAD)"),
        Row("Ctrl+Z", "Undo last op — branch/tag delete, merge, pull, rename"),
        Blank,
        Header("Files Panel"),
    ];

    if is_uncommitted {
        e.extend([
            Row("s", "Stage/unstage file"),
            Row("Shift+S", "Stage all"),
            Row("Shift+U", "Unstage all"),
            Row("i", "Add to .gitignore (folder in folder mode)"),
            Row("v", "Archive to .archive/ (folder in folder mode)"),
            Row("r", "Restore file (discard changes)"),
            Row("Delete", "Delete untracked file (recycle bin)"),
            Row("Ctrl+z", "Undo last file operation"),
            Header("Merge conflicts"),
            Row("] / [", "Jump to next / previous conflicted file"),
            Row("o", "Accept ours (on conflicted file)"),
            Row("t", "Accept theirs (on conflicted file)"),
            Row("c", "Continue merge/rebase/cherry-pick/revert"),
            Row("Shift+A", "Abort the in-progress operation"),
        ]);
    }

    e.extend([
        Row("f", "Toggle folder grouping"),
        Row("Ctrl+f", "Filter files"),
        Row("Space", "Open file with default app"),
        Row("y", "Copy file path"),
        Row("Enter", "Open file diff"),
        Row("h", "File history (commits touching this file)"),
        Blank,
        Header("File Diff Viewer"),
        Row("[ / ]", "Previous / next hunk"),
        Row("n / Shift+N", "Next / previous file"),
        Row("s", "Stage hunk under cursor"),
        Row("u", "Unstage hunk under cursor"),
        Row("x", "Discard hunk (working tree)"),
        Row("Ctrl+Alt+W", "Toggle soft line wrap"),
        Blank,
        Header("Commit Panel"),
        Row("↑ / ↓", "Scroll"),
        Row("Enter", "Start editing commit message"),
        Row("Enter", "Commit changes (or save amend)"),
        Row("Ctrl+Enter", "Amend last commit"),
        Row("Ctrl+S", "Stash changes (staged / all / +untracked)"),
        Blank,
        Header("GitHub Issues"),
        Row("Shift+I", "Open the issue list (from any panel)"),
        Row("Enter", "Open the selected issue's detail"),
        Row("Tab / f", "Cycle status filter (open / closed / all)"),
        Row("t", "Filter by label (checkbox picker)"),
        Row("u", "Toggle unblocked-only (hide issues with open blockers)"),
        Row("l", "Toggle tags on the selected issue"),
        Row("n", "New issue"),
        Row("c", "Comment (in detail)"),
        Row("x", "Close / reopen (in detail)"),
        Row("a", "Edit assignees (in detail)"),
        Row("r", "Refresh   o  Open in browser"),
        Blank,
        Header("Search"),
        Row("/", "Search branches"),
        Blank,
        Header("Mouse"),
        Row("Click", "Select commit/file, focus panel"),
        Row("Double-click", "Open commit menu / file diff"),
        Row("Right-click", "Commit context menu at cursor"),
        Row("Click chip", "PR badge opens PR; branch chip checks out"),
        Row("Wheel", "Scroll panel / popup under cursor"),
        Row("Drag divider", "Resize the graph/detail split"),
        Blank,
        Header("Other"),
        Row("Ctrl+P / :", "Command palette (commands, branches, commits)"),
        Row("Shift+R", "Refresh"),
        Row("F5", "Full update (fetch all remotes + PRs + refresh)"),
        Row("?", "Toggle this help"),
        Row("Ctrl+Q", "Quit (from anywhere)"),
    ]);

    e
}

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

        let entries = entries(self.is_uncommitted);
        // Fixed key column sized to the longest key, guaranteeing a ≥ KEY_GAP
        // gap so keys and descriptions never collide (e.g. "Tab / Shift+Tab").
        let kw = key_column_width(&entries);

        let lines: Vec<Line> = entries
            .iter()
            .map(|entry| match entry {
                HelpEntry::Header(text) => Line::from(Span::styled(*text, header_style)),
                HelpEntry::Row(key, desc) => Line::from(vec![
                    // Leading space indents rows under their section header;
                    // `{:<kw$}` pads the key so descriptions start at a fixed
                    // column with a guaranteed gap.
                    Span::styled(format!(" {key:<kw$}"), key_style),
                    Span::styled(*desc, desc_style),
                ]),
                HelpEntry::Blank => Line::from(""),
            })
            .collect();

        let block = self.theme.popup_block(" Help ");
        let paragraph = Paragraph::new(lines).block(block);

        Widget::render(paragraph, area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_column_leaves_a_gap_after_the_longest_key() {
        let e = entries(true);
        let kw = key_column_width(&e);
        // Widest key label present in the sheet.
        let widest = e
            .iter()
            .filter_map(|entry| match entry {
                HelpEntry::Row(k, _) => Some(UnicodeWidthStr::width(*k)),
                _ => None,
            })
            .max()
            .unwrap();
        assert_eq!(kw, widest + KEY_GAP);
        // Every key padded to `kw` keeps at least KEY_GAP trailing spaces before
        // the description — the fix for the "Tab / Shift+TabSwitch panels"
        // collision.
        for entry in &e {
            if let HelpEntry::Row(k, _) = entry {
                let padded = format!("{k:<kw$}");
                let trailing = kw - UnicodeWidthStr::width(*k);
                assert!(
                    trailing >= KEY_GAP,
                    "key {k:?} padded to {padded:?} leaves only {trailing} cols"
                );
            }
        }
    }

    #[test]
    fn width_is_stable_across_contexts() {
        // The widest key lives in an always-present section (Mouse), so the
        // column width does not jump between committed and uncommitted help.
        assert_eq!(
            key_column_width(&entries(false)),
            key_column_width(&entries(true))
        );
    }

    #[test]
    fn shift_bindings_are_labelled_with_shift_prefix() {
        // Every key that keybindings.rs binds via KeyModifiers::SHIFT should be
        // rendered as "Shift+<Key>" here, not a bare capital letter, and no
        // abbreviations like "S-Tab" / "C-k" should remain.
        let shift_bound_keys = [
            "Shift+G", "Shift+P", "Shift+B", "Shift+O", "Shift+M", "Shift+A", "Shift+N",
            "Shift+I", "Shift+R", "Shift+S", "Shift+U", "Shift+Tab",
        ];
        let text: String = entries(true)
            .iter()
            .filter_map(|entry| match entry {
                HelpEntry::Row(k, _) => Some(*k),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        for key in shift_bound_keys {
            assert!(
                text.contains(key),
                "expected help text to contain {key:?}, got: {text}"
            );
        }
        assert!(!text.contains("S-Tab"), "abbreviated modifier remained");
        assert!(!text.contains("C-k"), "abbreviated modifier remained");
        assert!(!text.contains("C-j"), "abbreviated modifier remained");
    }
}
