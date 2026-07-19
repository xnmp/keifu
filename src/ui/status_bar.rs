//! Status bar widget
//!
//! Key hints are built through a [`HintBar`] that records each clickable hint's
//! column range as it lays spans out, so the same pass drives both rendering and
//! mouse hit-testing — there is one source of truth for where a hint sits and
//! what pressing its key would do. `hint_regions` maps those ranges to absolute
//! cell rects for the mouse layer.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;
use crate::action::Action;
use crate::app::{App, AppMode, FocusedPanel, InputAction};

/// Contextual hint text for the open-PR shortcut, shown when the selected
/// commit has an open PR. Pure so it can be unit-tested without a terminal.
fn pr_hint_label(pr_number: Option<u64>) -> Option<String> {
    pr_number.map(|n| format!("open PR #{n}"))
}

/// A clickable key hint's column range (offset from the line start) and the
/// action a click dispatches.
struct HintSpan {
    start: u16,
    end: u16,
    action: Action,
}

/// Accumulates the status-bar line while tracking each span's starting column,
/// so a `key`+`desc` hint can be recorded as a `[start, end)` range in the same
/// pass that builds it. Non-clickable content (chips, separators, directional
/// hints) advances the cursor without recording a range.
struct HintBar {
    spans: Vec<Span<'static>>,
    hints: Vec<HintSpan>,
    x: u16,
}

impl HintBar {
    fn new() -> Self {
        Self {
            spans: Vec::new(),
            hints: Vec::new(),
            x: 0,
        }
    }

    /// Push a span and advance the column cursor by its display width.
    fn span(&mut self, span: Span<'static>) {
        self.x = self
            .x
            .saturating_add(UnicodeWidthStr::width(span.content.as_ref()) as u16);
        self.spans.push(span);
    }

    fn raw(&mut self, s: &'static str) {
        self.span(Span::raw(s));
    }

    /// Push a `key`+`desc` hint pair and record the combined span as a clickable
    /// region bound to `action`.
    fn hint(
        &mut self,
        key: &str,
        key_style: Style,
        desc: &str,
        desc_style: Style,
        action: Action,
    ) {
        let start = self.x;
        self.span(Span::styled(key.to_string(), key_style));
        self.span(Span::styled(desc.to_string(), desc_style));
        self.hints.push(HintSpan {
            start,
            end: self.x,
            action,
        });
    }

    /// Push a `key`+`desc` hint pair that isn't clickable — used for hints with
    /// no single unambiguous action (directional `↑↓` / `←→`, the `<>` width
    /// pair) and for the transient commit-editor hints.
    fn hint_static(&mut self, key: &str, key_style: Style, desc: &str, desc_style: Style) {
        self.span(Span::styled(key.to_string(), key_style));
        self.span(Span::styled(desc.to_string(), desc_style));
    }
}

pub struct StatusBar {
    spans: Vec<Span<'static>>,
    hints: Vec<HintSpan>,
    mode_label: Option<&'static str>,
    mode_style: Style,
}

impl StatusBar {
    pub fn new(app: &App, theme: &Theme) -> Self {
        // Three-tier hierarchy: accent (keys + the repo identity chip), normal
        // text (the branch), muted text (hint labels). Colored backgrounds are
        // reserved for semantic states (detached/error/mode).
        let accent = theme.accent();
        let key_style = Style::default().fg(accent).add_modifier(Modifier::BOLD);
        let desc_style = Style::default().fg(theme.text_muted);
        let mode_style = Style::default()
            .fg(theme.status_mode_fg)
            .bg(theme.status_mode_bg)
            .add_modifier(Modifier::BOLD);
        let repo_style = Style::default()
            .fg(theme.status_repo_fg)
            .bg(accent)
            .add_modifier(Modifier::BOLD);

        let mode = &app.mode;
        let focused_panel = app.focused_panel;
        let is_busy = app.is_network_busy();
        let is_uncommitted = app.is_uncommitted_selected();
        let is_filtering = app.files_pane.files_filter_active;
        let editing_commit = app.editing_commit_message;
        let amending_commit = app.amending_commit;
        let op_state = app.op_state;
        let conflict_count = app.conflict_count;

        let error_message = match mode {
            AppMode::Error { message } => Some(message.as_str()),
            _ => None,
        };

        // The selected commit's open PR, if any — drives the o/c/v hints.
        let selected_pr = app.selected_commit_node().and_then(|node| {
            crate::ui::graph_view::pr_for_branch_labels(
                &node.branch_names,
                &app.remotes,
                &app.open_prs,
            )
        });
        let pr_hint = pr_hint_label(selected_pr.map(|pr| pr.number));
        let pr_has_ci = selected_pr.is_some_and(|pr| pr.ci != crate::pr::CiStatus::None);
        let graph_cappable = (app.graph_layout.max_lane + 1) * 2 > 4;
        let trace_traceable = crate::git::graph::graph_has_enough_lanes(&app.graph_layout);
        let trace_enabled = app.trace_enabled;
        let diff_word_wrap = app.diff_word_wrap;

        // Search status message (only while the search input is open).
        let search_info = match mode {
            AppMode::Input {
                action: InputAction::Search,
                ..
            } => {
                let count = app.search_match_count();
                Some(if count > 0 {
                    format!("{} matches", count)
                } else {
                    "No matches".to_string()
                })
            }
            _ => None,
        };

        let mut hb = HintBar::new();

        // Repository name (folder name) on the left.
        let repo_name = std::path::Path::new(&app.repo_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(app.repo_path.as_str());
        hb.span(Span::styled(format!(" {} ", repo_name), repo_style));
        hb.raw(" ");

        // HEAD branch.
        if let Some(head) = app.head_name.as_deref() {
            if app.head_detached {
                hb.span(Span::styled(
                    format!(" DETACHED: {} ", head),
                    Style::default()
                        .fg(theme.status_detached_fg)
                        .bg(theme.status_detached_bg)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                hb.span(Span::styled(
                    format!(" {} ", head),
                    Style::default()
                        .fg(theme.text_primary)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            hb.raw(" ");

            // Ahead/behind vs upstream (e.g. "↑2 ↓1"), when tracking and diverged.
            let head_ahead_behind = app
                .branches
                .iter()
                .find(|b| b.is_head && b.upstream.is_some())
                .map(|b| (b.ahead, b.behind));
            if let Some((ahead, behind)) = head_ahead_behind {
                if ahead > 0 || behind > 0 {
                    let mut label = String::new();
                    if ahead > 0 {
                        label.push_str(&format!("↑{ahead}"));
                    }
                    if behind > 0 {
                        if !label.is_empty() {
                            label.push(' ');
                        }
                        label.push_str(&format!("↓{behind}"));
                    }
                    hb.span(Span::styled(
                        format!(" {label} "),
                        Style::default()
                            .fg(theme.text_primary)
                            .add_modifier(Modifier::BOLD),
                    ));
                    hb.raw(" ");
                }
            }
        }

        // Remote-only branches hidden by the show/hide-remotes toggle (Shift+O).
        if app.hide_remote_branches {
            hb.span(Span::styled(" remotes hidden ", mode_style));
            hb.raw(" ");
        }

        // In-progress operation indicator (merge/rebase/…): prominent, shown in
        // Normal mode regardless of message/hints so conflicts stay visible.
        if matches!(mode, AppMode::Normal) && op_state.is_in_progress() {
            let op_style = Style::default()
                .fg(theme.status_error_fg)
                .bg(theme.status_error_bg)
                .add_modifier(Modifier::BOLD);
            let label = if conflict_count > 0 {
                format!(
                    " {} ({} conflict{}) ",
                    op_state.label(),
                    conflict_count,
                    if conflict_count == 1 { "" } else { "s" }
                )
            } else {
                format!(" {} ", op_state.label())
            };
            hb.span(Span::styled(label, op_style));
            // c/A are only wired up in the files pane, so only advertise them
            // once that's actually where the hint applies.
            if focused_panel == FocusedPanel::Files {
                hb.hint(" c ", key_style, "continue ", desc_style, Action::ContinueOperation);
                hb.hint(" A ", key_style, "abort ", desc_style, Action::AbortOperation);
            }
            hb.raw("  ");
        }

        // Key hints (vary by mode).
        match mode {
            AppMode::Normal => match app.get_message() {
                Some(msg) => {
                    let bg = if is_busy {
                        theme.status_busy_bg
                    } else {
                        theme.status_success_bg
                    };
                    let msg_style = Style::default()
                        .fg(theme.status_key_fg)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD);
                    hb.span(Span::styled(format!(" {} ", msg), msg_style));
                    hb.raw("  ");
                }
                None => {
                    if let Some(info) = &search_info {
                        let search_style = Style::default()
                            .fg(theme.status_branch_fg)
                            .bg(theme.status_branch_bg)
                            .add_modifier(Modifier::BOLD);
                        hb.span(Span::styled(format!(" {} ", info), search_style));
                        hb.raw("  ");
                    }

                    if editing_commit {
                        // Transient editor hints: not wired as clickable (a click
                        // mid-edit shouldn't commit/amend).
                        if amending_commit {
                            hb.hint_static(" Enter ", key_style, "save amend ", desc_style);
                        } else {
                            hb.hint_static(" Enter ", key_style, "commit ", desc_style);
                            hb.hint_static(" Ctrl+Enter ", key_style, "amend ", desc_style);
                            hb.hint_static(" Ctrl+S ", key_style, "stash ", desc_style);
                        }
                        hb.hint_static(" Esc ", key_style, "cancel", desc_style);
                    } else {
                        match focused_panel {
                            FocusedPanel::Graph => {
                                hb.hint_static(" ↑↓ ", key_style, "move ", desc_style);
                                hb.hint(" Enter ", key_style, "actions ", desc_style, Action::OpenCommitMenu);
                                // Only when the selected commit has an open PR.
                                if let Some(hint) = &pr_hint {
                                    hb.hint(" o ", key_style, &format!("{hint} "), desc_style, Action::OpenPr);
                                    // ...and a CI checks hint when the PR reports checks.
                                    if pr_has_ci {
                                        hb.hint(" c ", key_style, "checks ", desc_style, Action::OpenCiChecks);
                                    }
                                    hb.hint(" v ", key_style, "thread ", desc_style, Action::OpenPrThread);
                                }
                                // Only when the graph is wide enough to be capped.
                                if graph_cappable {
                                    hb.hint_static(" <> ", key_style, "width ", desc_style);
                                }
                                // Only on branchy graphs, where tracing helps.
                                if trace_traceable {
                                    let label = if trace_enabled {
                                        "trace on "
                                    } else {
                                        "trace off "
                                    };
                                    hb.hint(" t ", key_style, label, desc_style, Action::ToggleTrace);
                                }
                                hb.hint_static(" ←→ ", key_style, "panels ", desc_style);
                                hb.hint(" B ", key_style, "branches ", desc_style, Action::OpenBranchFilter);
                                hb.hint(" ? ", key_style, "help", desc_style, Action::ToggleHelp);
                            }
                            FocusedPanel::Files if is_filtering => {
                                let filter_style = Style::default()
                                    .fg(theme.status_mode_fg)
                                    .bg(theme.status_mode_bg)
                                    .add_modifier(Modifier::BOLD);
                                hb.span(Span::styled(" FILTER ", filter_style));
                                hb.raw("  ");
                                hb.hint(" Enter ", key_style, "confirm ", desc_style, Action::Confirm);
                                hb.hint(" Esc ", key_style, "cancel ", desc_style, Action::Cancel);
                            }
                            FocusedPanel::Files => {
                                hb.hint_static(" ↑↓ ", key_style, "select ", desc_style);
                                hb.hint(" Enter ", key_style, "diff ", desc_style, Action::OpenFileDiff);
                                hb.hint(" Space ", key_style, "open ", desc_style, Action::OpenWithDefault);
                                if is_uncommitted {
                                    hb.hint(" s ", key_style, "stage ", desc_style, Action::ToggleStage);
                                    hb.hint(" r ", key_style, "restore ", desc_style, Action::RestoreFile);
                                    hb.hint(" i ", key_style, "ignore ", desc_style, Action::AddToGitignore);
                                    hb.hint(" v ", key_style, "archive ", desc_style, Action::ArchiveFile);
                                    hb.hint(" Del ", key_style, "trash ", desc_style, Action::TrashFile);
                                    hb.hint(" ^z ", key_style, "undo ", desc_style, Action::UndoLastFileOp);
                                }
                                hb.hint(" f ", key_style, "folders ", desc_style, Action::ToggleFolderView);
                                hb.hint(" ^f ", key_style, "filter ", desc_style, Action::StartFilesFilter);
                                hb.hint_static(" ←→ ", key_style, "panels ", desc_style);
                                hb.hint(" ? ", key_style, "help", desc_style, Action::ToggleHelp);
                            }
                            FocusedPanel::CommitDetail => {
                                hb.hint_static(" ↑↓ ", key_style, "scroll ", desc_style);
                                if is_uncommitted {
                                    hb.hint(" Enter ", key_style, "edit msg ", desc_style, Action::StartEditing);
                                    hb.hint(" Ctrl+Enter ", key_style, "amend ", desc_style, Action::AmendCommit);
                                    hb.hint(" Ctrl+S ", key_style, "stash ", desc_style, Action::StashStaged);
                                }
                                hb.hint_static(" ←→ ", key_style, "panels ", desc_style);
                                hb.hint(" Esc ", key_style, "graph ", desc_style, Action::FocusGraph);
                                hb.hint(" ? ", key_style, "help", desc_style, Action::ToggleHelp);
                            }
                        }
                    }
                }
            },
            AppMode::Help => {
                hb.hint(" Esc/q ", key_style, "close help", desc_style, Action::ToggleHelp);
            }
            AppMode::Input { .. } => {
                hb.hint(" Enter ", key_style, "confirm ", desc_style, Action::Confirm);
                hb.hint(" Esc ", key_style, "cancel", desc_style, Action::Cancel);
            }
            AppMode::Confirm { .. } => {
                hb.hint(" y ", key_style, "yes ", desc_style, Action::Confirm);
                hb.hint(" n ", key_style, "no", desc_style, Action::Cancel);
            }
            AppMode::Error { .. } => {
                // In error mode, show the message then a single close hint.
                let error_style = Style::default()
                    .fg(theme.status_error_fg)
                    .bg(theme.status_error_bg)
                    .add_modifier(Modifier::BOLD);
                if let Some(msg) = error_message {
                    hb.span(Span::styled(format!(" {} ", msg), error_style));
                    hb.raw("  ");
                    hb.hint(" Esc/Enter ", key_style, "close", desc_style, Action::Cancel);
                }
            }
            AppMode::FileDiff { .. } => {
                hb.hint_static(" n/N ", key_style, "file ", desc_style);
                hb.hint_static(" ]/[ ", key_style, "hunk ", desc_style);
                hb.hint_static(" ↑↓ ", key_style, "scroll ", desc_style);
                // Panning is only meaningful without wrap; when wrapped, lines
                // already fit the pane.
                if !diff_word_wrap {
                    hb.hint_static(" ←→ ", key_style, "pan ", desc_style);
                }
                let wrap_label = if diff_word_wrap { "wrap on " } else { "wrap off " };
                hb.hint(" ^⌥w ", key_style, wrap_label, desc_style, Action::ToggleDiffWrap);
                hb.hint(" Esc ", key_style, "back", desc_style, Action::Cancel);
            }
            AppMode::CommitMenu { .. }
            | AppMode::BranchPicker { .. }
            | AppMode::BranchDeletePicker { .. }
            | AppMode::TagPicker { .. }
            | AppMode::RemotePicker { .. } => {
                hb.hint_static(" ↑/↓ ", key_style, "select ", desc_style);
                hb.hint(" Enter ", key_style, "confirm ", desc_style, Action::MenuSelect);
                hb.hint(" Esc ", key_style, "cancel", desc_style, Action::Cancel);
            }
            AppMode::MetadataMenu { .. } => {
                hb.hint_static(" ↑/↓ ", key_style, "move ", desc_style);
                hb.hint(" Space ", key_style, "toggle ", desc_style, Action::MenuSelect);
                hb.hint(" Esc ", key_style, "close", desc_style, Action::Cancel);
            }
            AppMode::PullDivergence { .. } => {
                hb.hint_static(" ↑/↓ ", key_style, "move ", desc_style);
                hb.hint(" Enter ", key_style, "choose ", desc_style, Action::MenuSelect);
                hb.hint(" Esc ", key_style, "cancel", desc_style, Action::Cancel);
            }
            AppMode::CiChecks => {
                hb.hint_static(" ↑↓ ", key_style, "nav ", desc_style);
                hb.hint(" Enter ", key_style, "details ", desc_style, Action::MenuSelect);
                hb.hint(" o ", key_style, "open ", desc_style, Action::OpenPr);
                hb.hint(" Esc ", key_style, "back", desc_style, Action::Cancel);
            }
            AppMode::PrThread => {
                hb.hint_static(" ↑↓ ", key_style, "scroll ", desc_style);
                hb.hint(" o ", key_style, "open PR ", desc_style, Action::OpenPr);
                hb.hint(" r ", key_style, "review ", desc_style, Action::OpenReviewPicker);
                hb.hint(" Esc ", key_style, "close", desc_style, Action::Cancel);
            }
            AppMode::PrCompose { .. } => {
                hb.hint(" Ctrl+S ", key_style, "submit ", desc_style, Action::SubmitCompose);
                hb.hint(" Esc ", key_style, "cancel", desc_style, Action::Cancel);
            }
            AppMode::PrMergePicker { .. } | AppMode::PrReviewPicker { .. } => {
                hb.hint_static(" ↑/↓ ", key_style, "move ", desc_style);
                hb.hint(" Enter ", key_style, "choose ", desc_style, Action::MenuSelect);
                hb.hint(" Esc ", key_style, "cancel", desc_style, Action::Cancel);
            }
            AppMode::BranchFilter { .. } => {
                hb.hint(" Space ", key_style, "toggle ", desc_style, Action::MenuSelect);
                hb.hint(" C-a ", key_style, "all ", desc_style, Action::SelectAll);
                hb.hint(" C-o ", key_style, "none ", desc_style, Action::SelectNone);
                hb.hint(" Esc ", key_style, "close", desc_style, Action::Cancel);
            }
            AppMode::FileHistory { .. } => {
                hb.hint_static(" ↑/↓ ", key_style, "select ", desc_style);
                hb.hint(" Enter ", key_style, "open diff ", desc_style, Action::MenuSelect);
                hb.hint(" Esc ", key_style, "back", desc_style, Action::Cancel);
            }
            AppMode::CommandPalette { .. } => {
                hb.hint_static(" type ", key_style, "filter ", desc_style);
                hb.hint_static(" ↑/↓ ", key_style, "move ", desc_style);
                hb.hint(" Enter ", key_style, "run ", desc_style, Action::MenuSelect);
                hb.hint(" Esc ", key_style, "close", desc_style, Action::Cancel);
            }
        }

        // Mode label shown on the right (only for non-Normal modes).
        let mode_label = match mode {
            AppMode::Normal => None,
            AppMode::Help => Some(" HELP "),
            AppMode::Input { .. } => Some(" INPUT "),
            AppMode::Confirm { .. } => Some(" CONFIRM "),
            AppMode::Error { .. } => Some(" ERROR "),
            AppMode::CommitMenu { .. } => Some(" MENU "),
            AppMode::MetadataMenu { .. } => Some(" COLUMNS "),
            AppMode::PullDivergence { .. } => Some(" PULL "),
            AppMode::CiChecks => Some(" CHECKS "),
            AppMode::PrThread => Some(" PR THREAD "),
            AppMode::PrCompose { .. } => Some(" COMPOSE "),
            AppMode::PrMergePicker { .. } => Some(" MERGE PR "),
            AppMode::PrReviewPicker { .. } => Some(" REVIEW "),
            AppMode::BranchPicker { .. } => Some(" CHECKOUT "),
            AppMode::BranchDeletePicker { .. } => Some(" DELETE BRANCH "),
            AppMode::TagPicker { .. } => Some(" TAG "),
            AppMode::RemotePicker { .. } => Some(" REMOTE "),
            AppMode::BranchFilter { .. } => Some(" BRANCH FILTER "),
            AppMode::FileDiff { .. } => Some(" DIFF "),
            AppMode::FileHistory { .. } => Some(" FILE HISTORY "),
            AppMode::CommandPalette { .. } => Some(" PALETTE "),
        };

        Self {
            spans: hb.spans,
            hints: hb.hints,
            mode_label,
            mode_style,
        }
    }

    /// Absolute cell rects for the clickable hints on this frame, paired with the
    /// action each dispatches. `area` is the status-bar row. Hints scrolled past
    /// the right edge are dropped (they aren't visible to click).
    pub fn hint_regions(&self, area: Rect) -> Vec<(Rect, Action)> {
        self.hints
            .iter()
            .filter(|h| h.start < area.width)
            .map(|h| {
                let end = h.end.min(area.width);
                (
                    Rect::new(area.x + h.start, area.y, end - h.start, 1),
                    h.action.clone(),
                )
            })
            .collect()
    }
}

impl Widget for StatusBar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let line = Line::from(self.spans);
        buf.set_line(area.x, area.y, &line, area.width);

        if let Some(text) = self.mode_label {
            let mode_len = text.len() as u16;
            if area.width > mode_len {
                let x = area.x + area.width - mode_len;
                buf.set_string(x, area.y, text, self.mode_style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    #[test]
    fn pr_hint_shown_only_when_a_pr_exists() {
        assert_eq!(pr_hint_label(Some(12)).as_deref(), Some("open PR #12"));
        assert_eq!(pr_hint_label(None), None);
    }

    #[test]
    fn hint_bar_records_ranges_over_key_plus_desc() {
        let s = Style::default();
        let mut hb = HintBar::new();
        // 2-column prefix chip, then two hints; a static hint records nothing.
        hb.raw("AB");
        hb.hint(" x ", s, "go ", s, Action::ToggleHelp); // 3 + 3 = 6 cols
        hb.hint_static(" y ", s, "no ", s); // advances but no range
        hb.hint(" z ", s, "end", s, Action::Cancel); // 3 + 3 = 6 cols

        assert_eq!(hb.hints.len(), 2, "static hint is not clickable");
        // First hint starts after the 2-col prefix, spans key+desc = 6 cols.
        assert_eq!((hb.hints[0].start, hb.hints[0].end), (2, 8));
        assert_eq!(hb.hints[0].action, Action::ToggleHelp);
        // Second hint follows the static hint (2 + 6 + 6 = 14) and spans 6 cols.
        assert_eq!((hb.hints[1].start, hb.hints[1].end), (14, 20));
        assert_eq!(hb.hints[1].action, Action::Cancel);
    }

    #[test]
    fn hint_bar_counts_wide_glyph_widths() {
        let s = Style::default();
        let mut hb = HintBar::new();
        // "↑↓" are two display columns despite being multi-byte.
        hb.raw("↑↓");
        hb.hint(" k ", s, "d", s, Action::Cancel);
        assert_eq!(hb.hints[0].start, 2, "wide glyphs count as their column width");
    }

    #[test]
    fn hint_regions_offsets_by_area_and_drops_offscreen() {
        let s = Style::default();
        let mut hb = HintBar::new();
        hb.hint(" a ", s, "one ", s, Action::ToggleHelp); // cols 0..7
        hb.hint(" b ", s, "two ", s, Action::Cancel); // cols 7..14
        let bar = StatusBar {
            spans: hb.spans,
            hints: hb.hints,
            mode_label: None,
            mode_style: Style::default(),
        };
        // Wide enough for both, offset by the bar's origin.
        let regions = bar.hint_regions(Rect::new(5, 20, 40, 1));
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].0, Rect::new(5, 20, 7, 1));
        assert_eq!(regions[0].1, Action::ToggleHelp);
        assert_eq!(regions[1].0, Rect::new(12, 20, 7, 1));

        // Narrow bar: the second hint starts past the edge and is dropped; the
        // first is clipped to the visible width.
        let regions = bar.hint_regions(Rect::new(0, 20, 5, 1));
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].0, Rect::new(0, 20, 5, 1));
    }
}
