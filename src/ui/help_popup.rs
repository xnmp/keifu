//! Help popup widget

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget,
        Wrap,
    },
};
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;

/// One row of the help sheet.
enum HelpEntry {
    /// A section heading (e.g. "Navigation").
    Header(&'static str),
    /// A `(key, description)` binding row.
    Row(&'static str, &'static str),
    /// The status-bar control, whose label reports the current setting value.
    StatusBar,
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
            HelpEntry::StatusBar => Some(UnicodeWidthStr::width("s")),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    widest + KEY_GAP
}

/// The help entries for the current context (uncommitted adds the staging and
/// merge-conflict rows). Order matches the on-screen sections.
fn entries(is_uncommitted: bool) -> Vec<HelpEntry> {
    use HelpEntry::{Blank, Header, Row, StatusBar};
    let mut e = vec![
        Header("Navigation"),
        Row("↑ / ↓", "Move up/down"),
        Row("← / →", "Switch panels"),
        Row("Tab / Shift+Tab", "Switch panels (forward/back)"),
        Row("Shift+F / Shift+C", "Show/hide the files / commit pane"),
        Row("Ctrl+d/u", "Page down/up"),
        Row("g / Home", "Go to top"),
        Row("Shift+G / End", "Go to bottom"),
        Row("@", "Jump to HEAD"),
        Row("Esc", "Return to graph / stop editing / quit (from graph)"),
        StatusBar,
        Blank,
        Header("Graph Panel"),
        Row("Enter", "Open actions menu"),
        Row("Space", "Open file select"),
        Row("] / [", "Next / previous branch label"),
        Row("b", "Create new branch"),
        Row("d", "Delete branch"),
        Row("f", "Fetch from remote"),
        Row("l", "Pull (fetch + integrate)"),
        Row("Shift+P", "Push current branch (publishes if no upstream)"),
        Row(
            "Shift+B",
            "Branch filter (type to filter by name, @ by author)",
        ),
        Row("Shift+O", "Show/hide remote-only branches"),
        Row(
            "Shift+H",
            "Hide/dim branches merged into the trunk (incl. squash)",
        ),
        Row("Ctrl+Shift+F", "Filter commits (message/author/hash)"),
        Row("m", "Mark / compare two commits (Esc clears)"),
        Row(
            "o",
            "Open PR in browser (badge color = CI: green/yellow/red)",
        ),
        Row("c", "CI check details (see failure logs without a browser)"),
        Row("v", "View PR conversation (comments, reviews, threads)"),
        Row("Shift+M", "Toggle author/hash/date, muted merges & avatars"),
        Row("< / >", "Shrink / widen the graph column (… = truncated)"),
        Row("t", "Toggle branch tracing (dim off-lineage lanes)"),
        Row("^", "Jump to fork point (merge base with main / HEAD)"),
        Row(
            "Ctrl+↑ / Ctrl+↓",
            "Jump to previous/next commit on the same graph line",
        ),
        Row(
            "Ctrl+Z",
            "Undo last op — branch/tag delete, merge, pull, rename",
        ),
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
        Row("/", "Filter files"),
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
        Row("Ctrl+Alt+W", "Toggle soft line wrap"),
        Row("Enter", "Start editing commit message"),
        Row("Enter", "Commit changes (or save amend)"),
        Row("Ctrl+Enter", "Amend last commit"),
        Row("Ctrl+S", "Stash changes (staged / all / +untracked)"),
        Blank,
        Header("GitHub Issues"),
        Row("Shift+I", "Open the issue list (from any panel)"),
        Row("Alt+I", "New repo issue (from anywhere)"),
        Row("Alt+K", "Report a Keifu issue (from anywhere)"),
        Row("Enter", "Open the selected issue's detail"),
        Row("Tab / f", "Cycle status filter (open / closed / all)"),
        Row("t", "Filter by label (checkbox picker)"),
        Row(
            "u",
            "Toggle unblocked-only (hide issues with open blockers)",
        ),
        Row("l", "Toggle tags on the selected issue"),
        Row("n", "New issue"),
        Row("e", "Edit title/body (in detail)"),
        Row("c", "Comment (in detail)"),
        Row("x", "Close / reopen (in detail)"),
        Row("a", "Edit assignees (in detail)"),
        Row("r", "Refresh   o  Open in browser"),
        Blank,
        Header("Search"),
        Row("Ctrl+F / /", "Search branches"),
        Row("Ctrl+Shift+F", "Search commits"),
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
        Row(
            "Ctrl+P / Ctrl+Alt+P / :",
            "Command palette (commands, branches, commits)",
        ),
        Row(
            "Ctrl+, / ,",
            "Settings menu (toggle/edit persisted settings)",
        ),
        Row("Shift+R", "Refresh"),
        Row("F5", "Full update (fetch all remotes + PRs + refresh)"),
        Row("?", "Toggle this help"),
        Row("Ctrl+Q", "Quit (from anywhere)"),
    ]);

    e
}

pub struct HelpPopup<'a> {
    pub is_uncommitted: bool,
    pub status_bar_visible: bool,
    pub theme: &'a Theme,
    pub scroll: usize,
}

impl<'a> HelpPopup<'a> {
    pub fn new(is_uncommitted: bool, theme: &'a Theme, scroll: usize) -> Self {
        Self::with_status_bar_visibility(is_uncommitted, true, theme, scroll)
    }

    pub fn with_status_bar_visibility(
        is_uncommitted: bool,
        status_bar_visible: bool,
        theme: &'a Theme,
        scroll: usize,
    ) -> Self {
        Self {
            is_uncommitted,
            status_bar_visible,
            theme,
            scroll,
        }
    }

    /// Number of rendered rows after the sheet has wrapped to `inner_width`.
    /// The draw pass uses this same measurement to clamp scrolling state.
    pub fn content_height(
        is_uncommitted: bool,
        status_bar_visible: bool,
        theme: &Theme,
        inner_width: u16,
    ) -> usize {
        Paragraph::new(lines(is_uncommitted, status_bar_visible, theme))
            .wrap(Wrap { trim: false })
            .line_count(inner_width)
    }
}

fn lines(is_uncommitted: bool, status_bar_visible: bool, theme: &Theme) -> Vec<Line<'static>> {
    let key_style = Style::default()
        .fg(theme.help_key)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme.text_primary);
    let header_style = Style::default()
        .fg(theme.help_header)
        .add_modifier(Modifier::BOLD);
    let entries = entries(is_uncommitted);
    let kw = key_column_width(&entries);
    entries
        .iter()
        .map(|entry| match entry {
            HelpEntry::Header(text) => Line::from(Span::styled(*text, header_style)),
            HelpEntry::Row(key, desc) => Line::from(vec![
                Span::styled(format!(" {key:<kw$}"), key_style),
                Span::styled(*desc, desc_style),
            ]),
            HelpEntry::StatusBar => Line::from(vec![
                Span::styled(format!(" {:<kw$}", "s"), key_style),
                Span::styled(
                    format!(
                        "Toggle status bar ({})",
                        if status_bar_visible { "On" } else { "Off" }
                    ),
                    desc_style,
                ),
            ]),
            HelpEntry::Blank => Line::from(""),
        })
        .collect()
}

impl<'a> Widget for HelpPopup<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let block = self.theme.popup_block(" Help ");
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let content_height = Self::content_height(
            self.is_uncommitted,
            self.status_bar_visible,
            self.theme,
            inner.width,
        );
        let scroll = self
            .scroll
            .min(content_height.saturating_sub(inner.height as usize));
        Paragraph::new(lines(
            self.is_uncommitted,
            self.status_bar_visible,
            self.theme,
        ))
        .wrap(Wrap { trim: false })
        .scroll((scroll.min(u16::MAX as usize) as u16, 0))
        .render(inner, buf);
        if content_height > inner.height as usize && area.height > 2 {
            let mut state = ScrollbarState::new(content_height)
                .viewport_content_length(inner.height as usize)
                .position(scroll);
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_style(self.theme.scrollbar_track_style())
                .thumb_style(self.theme.scrollbar_thumb_style())
                .render(
                    Rect::new(
                        area.x.saturating_add(area.width.saturating_sub(1)),
                        area.y.saturating_add(1),
                        1,
                        area.height.saturating_sub(2),
                    ),
                    buf,
                    &mut state,
                );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    fn rendered_text(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

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
            "Shift+G",
            "Shift+P",
            "Shift+B",
            "Shift+O",
            "Shift+H",
            "Shift+M",
            "Shift+A",
            "Shift+N",
            "Shift+I",
            "Shift+R",
            "Shift+S",
            "Shift+U",
            "Shift+Tab",
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

    #[test]
    fn short_viewport_reaches_final_help_entries_and_draws_scrollbar() {
        let theme = Theme::dark();
        let area = Rect::new(0, 0, 64, 8);
        let inner_width = area.width.saturating_sub(4);
        let inner_height = area.height.saturating_sub(2) as usize;
        let content = HelpPopup::content_height(false, true, &theme, inner_width);
        assert!(content > inner_height, "fixture must overflow the popup");

        let mut top = Buffer::empty(area);
        HelpPopup::with_status_bar_visibility(false, true, &theme, 0).render(area, &mut top);
        assert!(!rendered_text(&top).contains("Quit (from anywhere)"));

        let mut bottom = Buffer::empty(area);
        HelpPopup::with_status_bar_visibility(false, true, &theme, content - inner_height)
            .render(area, &mut bottom);
        let text = rendered_text(&bottom);
        assert!(text.contains("Quit (from anywhere)"));
        assert_eq!(bottom[(area.width - 1, 0)].symbol(), "╮");
        assert_eq!(bottom[(area.width - 1, area.height - 1)].symbol(), "╯");
        let top_thumb = top[(area.width - 1, 1)].symbol().to_owned();
        let bottom_thumb = bottom[(area.width - 1, area.height - 2)]
            .symbol()
            .to_owned();
        assert_eq!(top_thumb, "█", "overflow draws a scrollbar thumb");
        assert_eq!(bottom_thumb, "█", "scrollbar remains visible at the end");
    }
}
