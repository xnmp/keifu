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
    /// A binding row tied directly to one or more stable action identifiers.
    Binding(&'static str, &'static [&'static str], &'static str),
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
            HelpEntry::Row(key, _) | HelpEntry::Binding(key, _, _) => {
                Some(UnicodeWidthStr::width(*key))
            }
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
    use HelpEntry::{Binding, Blank, Header, Row, StatusBar};
    macro_rules! bind {
        ($key:literal, $description:literal, $($id:literal),+ $(,)?) => {
            Binding($key, &[$($id),+], $description)
        };
    }
    let mut e = vec![
        Header("Navigation"),
        bind!("↑ / ↓", "Move up/down", "move-up", "move-down"),
        bind!("← / →", "Switch panels", "panel-left", "panel-right"),
        bind!(
            "Tab / Shift+Tab",
            "Switch panels (forward/back)",
            "panel-right",
            "panel-left"
        ),
        bind!(
            "Shift+F / Shift+C",
            "Show/hide the files / commit pane",
            "toggle-files-pane",
            "toggle-commit-pane"
        ),
        bind!("Ctrl+d/u", "Page down/up", "page-down", "page-up"),
        bind!("g / Home", "Go to top", "go-to-top"),
        bind!("Shift+G / End", "Go to bottom", "go-to-bottom"),
        bind!("@", "Jump to HEAD", "jump-to-head"),
        bind!(
            "Esc",
            "Return to graph / stop editing / quit (from graph)",
            "focus-graph",
            "stop-editing",
            "quit"
        ),
        StatusBar,
        Blank,
        Header("Graph Panel"),
        bind!("Enter", "Open actions menu", "open-commit-menu"),
        bind!("Space", "Open file select", "open-file-diff"),
        bind!(
            "] / [",
            "Next / previous branch label",
            "next-branch",
            "previous-branch"
        ),
        bind!("b", "Create new branch", "create-branch"),
        bind!("d", "Delete branch", "delete-branch"),
        bind!("f", "Fetch from remote", "fetch"),
        bind!("l", "Pull (fetch + integrate)", "pull"),
        bind!(
            "Shift+P",
            "Push current branch (publishes if no upstream)",
            "push"
        ),
        bind!(
            "Shift+B",
            "Branch filter (type to filter by name, @ by author)",
            "open-branch-filter"
        ),
        bind!(
            "Shift+O",
            "Show/hide remote-only branches",
            "toggle-remote-branches"
        ),
        bind!(
            "Shift+H",
            "Hide/dim branches merged into the trunk (incl. squash)",
            "toggle-merged-branches"
        ),
        bind!(
            "Ctrl+Shift+F",
            "Filter commits (message/author/file/hash)",
            "start-commit-filter"
        ),
        bind!(
            "m",
            "Mark / compare two commits (Esc clears)",
            "mark-for-compare"
        ),
        bind!(
            "o",
            "Open PR in browser (badge color = CI: green/yellow/red)",
            "open-pr"
        ),
        bind!(
            "c",
            "CI check details (see failure logs without a browser)",
            "open-ci-checks"
        ),
        bind!(
            "v",
            "View PR conversation (comments, reviews, threads)",
            "open-pr-thread"
        ),
        bind!(
            "Shift+M",
            "Toggle author/hash/date, muted merges & avatars",
            "open-metadata-menu"
        ),
        bind!(
            "< / >",
            "Shrink / widen the graph column (… = truncated)",
            "shrink-graph-width",
            "widen-graph-width"
        ),
        bind!(
            "t",
            "Toggle branch tracing (dim off-lineage lanes)",
            "toggle-trace"
        ),
        bind!(
            "^",
            "Jump to fork point (merge base with main / HEAD)",
            "jump-to-merge-base"
        ),
        bind!(
            "Ctrl+↑ / Ctrl+↓",
            "Jump to previous/next commit on the same graph line",
            "same-lane-up",
            "same-lane-down"
        ),
        bind!(
            "Ctrl+Z",
            "Undo last op — branch/tag delete, merge, pull, rename",
            "undo-last-operation"
        ),
        Blank,
        Header("Files Panel"),
    ];

    if is_uncommitted {
        e.extend([
            bind!("s", "Stage/unstage file", "toggle-stage"),
            bind!("Shift+S", "Stage all", "stage-all"),
            bind!("Shift+U", "Unstage all", "unstage-all"),
            bind!(
                "i",
                "Add to .gitignore (folder in folder mode)",
                "add-to-gitignore"
            ),
            bind!(
                "v",
                "Archive to .archive/ (folder in folder mode)",
                "archive-file"
            ),
            bind!("r", "Restore file (discard changes)", "restore-file"),
            bind!(
                "Delete",
                "Delete untracked file (recycle bin)",
                "trash-file"
            ),
            bind!(
                "Ctrl+z",
                "Undo last file operation",
                "undo-last-file-operation"
            ),
            Header("Merge conflicts"),
            bind!(
                "] / [",
                "Jump to next / previous conflicted file",
                "next-conflict",
                "previous-conflict"
            ),
            bind!("o", "Accept ours (on conflicted file)", "accept-ours"),
            bind!("t", "Accept theirs (on conflicted file)", "accept-theirs"),
            bind!(
                "c",
                "Continue merge/rebase/cherry-pick/revert",
                "continue-operation"
            ),
            bind!(
                "Shift+A",
                "Abort the in-progress operation",
                "abort-operation"
            ),
        ]);
    }

    e.extend([
        bind!("f", "Toggle folder grouping", "toggle-folder-view"),
        bind!("/", "Filter files", "start-files-filter"),
        bind!("Space", "Open file with default app", "open-with-default"),
        bind!("y", "Copy file path", "copy-path"),
        bind!("Enter", "Open file diff", "open-file-diff"),
        bind!(
            "h",
            "File history (commits touching this file)",
            "file-history"
        ),
        Blank,
        Header("File Diff Viewer"),
        bind!(
            "[ / ]",
            "Previous / next hunk",
            "previous-hunk",
            "next-hunk"
        ),
        bind!(
            "n / Shift+N",
            "Next / previous file",
            "next-file",
            "previous-file"
        ),
        bind!("s", "Stage hunk under cursor", "stage-hunk"),
        bind!("u", "Unstage hunk under cursor", "unstage-hunk"),
        bind!("x", "Discard hunk (working tree)", "discard-hunk"),
        bind!("Ctrl+Alt+W", "Toggle soft line wrap", "toggle-diff-wrap"),
        Blank,
        Header("Commit Panel"),
        bind!("↑ / ↓", "Scroll", "move-up", "move-down"),
        bind!(
            "Ctrl+Alt+W",
            "Toggle soft line wrap",
            "toggle-commit-detail-wrap"
        ),
        bind!("Enter", "Start editing commit message", "start-editing"),
        bind!("Enter", "Commit changes (or save amend)", "commit-changes"),
        bind!("Ctrl+Enter", "Amend last commit", "amend-commit"),
        bind!(
            "Ctrl+S",
            "Stash changes (staged / all / +untracked)",
            "stash-staged"
        ),
        Blank,
        Header("GitHub Issues"),
        bind!(
            "Shift+I",
            "Open the issue list (from any panel)",
            "open-issue-list"
        ),
        bind!("Alt+I", "New repo issue (from anywhere)", "new-issue"),
        bind!(
            "Alt+K",
            "Report a Keifu issue (from anywhere)",
            "report-keifu-issue"
        ),
        bind!(
            "Enter",
            "Open the selected issue's detail",
            "open-issue-detail"
        ),
        bind!(
            "Tab / f",
            "Cycle status filter (open / closed / all)",
            "cycle-issue-filter"
        ),
        bind!(
            "t",
            "Filter by label (checkbox picker)",
            "open-issue-label-filter"
        ),
        bind!(
            "u",
            "Toggle unblocked-only (hide issues with open blockers)",
            "toggle-unblocked-only"
        ),
        bind!(
            "l",
            "Toggle tags on the selected issue",
            "edit-issue-labels"
        ),
        bind!("n", "New issue", "new-issue"),
        bind!("e", "Edit title/body (in detail)", "edit-issue"),
        bind!("c", "Comment (in detail)", "comment-on-issue"),
        bind!("x", "Close / reopen (in detail)", "toggle-issue-state"),
        bind!("a", "Edit assignees (in detail)", "edit-issue-assignees"),
        bind!("r", "Refresh issues", "refresh-issues"),
        bind!("o", "Open issue in browser", "open-issue-in-browser"),
        Blank,
        Header("Search"),
        bind!("Ctrl+F / /", "Search branches", "search"),
        bind!("Ctrl+Shift+F", "Search commits", "start-commit-filter"),
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
        bind!(
            "Ctrl+P / Ctrl+Alt+P / :",
            "Command palette (commands, branches, commits)",
            "open-command-palette"
        ),
        bind!(
            "Ctrl+, / ,",
            "Settings menu (toggle/edit persisted settings)",
            "open-settings"
        ),
        bind!("Shift+R", "Refresh", "refresh"),
        bind!(
            "F5",
            "Full update (fetch all remotes + PRs + refresh)",
            "full-update"
        ),
        bind!("?", "Toggle this help", "toggle-help"),
        bind!("Ctrl+Q", "Quit (from anywhere)", "force-quit"),
    ]);

    e
}

pub struct HelpPopup<'a> {
    pub is_uncommitted: bool,
    pub status_bar_visible: bool,
    pub theme: &'a Theme,
    pub scroll: usize,
    pub keymap: Option<&'a crate::keymap::ResolvedKeymap>,
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
            keymap: None,
        }
    }

    pub fn with_keymap(
        is_uncommitted: bool,
        status_bar_visible: bool,
        theme: &'a Theme,
        scroll: usize,
        keymap: &'a crate::keymap::ResolvedKeymap,
    ) -> Self {
        Self {
            is_uncommitted,
            status_bar_visible,
            theme,
            scroll,
            keymap: Some(keymap),
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
        Paragraph::new(lines(is_uncommitted, status_bar_visible, theme, None))
            .wrap(Wrap { trim: false })
            .line_count(inner_width)
    }

    pub fn content_height_with_keymap(
        is_uncommitted: bool,
        status_bar_visible: bool,
        theme: &Theme,
        inner_width: u16,
        keymap: &crate::keymap::ResolvedKeymap,
    ) -> usize {
        Paragraph::new(lines(
            is_uncommitted,
            status_bar_visible,
            theme,
            Some(keymap),
        ))
        .wrap(Wrap { trim: false })
        .line_count(inner_width)
    }
}

fn lines(
    is_uncommitted: bool,
    status_bar_visible: bool,
    theme: &Theme,
    keymap: Option<&crate::keymap::ResolvedKeymap>,
) -> Vec<Line<'static>> {
    let key_style = Style::default()
        .fg(theme.help_key)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme.text_primary);
    let header_style = Style::default()
        .fg(theme.help_header)
        .add_modifier(Modifier::BOLD);
    let entries = entries(is_uncommitted);
    let kw = entries
        .iter()
        .filter_map(|entry| match entry {
            HelpEntry::Row(key, _) => Some((*key).to_string()),
            HelpEntry::Binding(key, ids, _) => Some(effective_bindings(keymap, key, ids)),
            HelpEntry::StatusBar => Some(
                keymap
                    .map(|map| map.display_bindings("toggle-status-bar"))
                    .unwrap_or_else(|| "s".to_string()),
            ),
            _ => None,
        })
        .map(|key| UnicodeWidthStr::width(key.as_str()))
        .max()
        .unwrap_or_else(|| key_column_width(&entries).saturating_sub(KEY_GAP))
        + KEY_GAP;
    entries
        .iter()
        .map(|entry| match entry {
            HelpEntry::Header(text) => Line::from(Span::styled(*text, header_style)),
            HelpEntry::Row(key, desc) => Line::from(vec![
                Span::styled(format!(" {key:<kw$}"), key_style),
                Span::styled(*desc, desc_style),
            ]),
            HelpEntry::Binding(key, ids, desc) => {
                let effective = effective_bindings(keymap, key, ids);
                Line::from(vec![
                    Span::styled(format!(" {effective:<kw$}"), key_style),
                    Span::styled(*desc, desc_style),
                ])
            }
            HelpEntry::StatusBar => Line::from(vec![
                Span::styled(
                    format!(
                        " {:<kw$}",
                        keymap
                            .map(|map| map.display_bindings("toggle-status-bar"))
                            .unwrap_or_else(|| "s".to_string())
                    ),
                    key_style,
                ),
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

fn effective_bindings(
    keymap: Option<&crate::keymap::ResolvedKeymap>,
    fallback: &str,
    ids: &[&str],
) -> String {
    keymap.map_or_else(
        || fallback.to_string(),
        |map| {
            if !ids.iter().any(|id| map.is_overridden(id)) {
                return fallback.to_string();
            }
            let mut labels = Vec::new();
            for id in ids {
                for label in map.display_bindings(id).split(" / ") {
                    if !labels.iter().any(|existing| existing == label) {
                        labels.push(label.to_string());
                    }
                }
            }
            labels.join(" / ")
        },
    )
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
        let content_height = if let Some(keymap) = self.keymap {
            Self::content_height_with_keymap(
                self.is_uncommitted,
                self.status_bar_visible,
                self.theme,
                inner.width,
                keymap,
            )
        } else {
            Self::content_height(
                self.is_uncommitted,
                self.status_bar_visible,
                self.theme,
                inner.width,
            )
        };
        let scroll = self
            .scroll
            .min(content_height.saturating_sub(inner.height as usize));
        Paragraph::new(lines(
            self.is_uncommitted,
            self.status_bar_visible,
            self.theme,
            self.keymap,
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
    fn every_shortcut_aware_help_row_references_registered_actions() {
        for entry in entries(true) {
            if let HelpEntry::Binding(_, ids, description) = entry {
                for id in ids {
                    assert!(
                        crate::keymap::binding_registry()
                            .iter()
                            .any(|descriptor| descriptor.id == *id),
                        "help row {description:?} references unknown action {id:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn key_column_leaves_a_gap_after_the_longest_key() {
        let e = entries(true);
        let kw = key_column_width(&e);
        // Widest key label present in the sheet.
        let widest = e
            .iter()
            .filter_map(|entry| match entry {
                HelpEntry::Row(k, _) | HelpEntry::Binding(k, _, _) => {
                    Some(UnicodeWidthStr::width(*k))
                }
                _ => None,
            })
            .max()
            .unwrap();
        assert_eq!(kw, widest + KEY_GAP);
        // Every key padded to `kw` keeps at least KEY_GAP trailing spaces before
        // the description — the fix for the "Tab / Shift+TabSwitch panels"
        // collision.
        for entry in &e {
            if let HelpEntry::Row(k, _) | HelpEntry::Binding(k, _, _) = entry {
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
                HelpEntry::Row(k, _) | HelpEntry::Binding(k, _, _) => Some(*k),
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
