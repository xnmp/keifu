//! Status bar widget

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};

use super::theme::Theme;
use crate::app::{App, AppMode, FocusedPanel, InputAction};
use crate::git::OperationState;

/// Contextual hint text for the open-PR shortcut, shown when the selected
/// commit has an open PR. Pure so it can be unit-tested without a terminal.
fn pr_hint_label(pr_number: Option<u64>) -> Option<String> {
    pr_number.map(|n| format!("open PR #{n}"))
}

pub struct StatusBar<'a> {
    mode: &'a AppMode,
    focused_panel: FocusedPanel,
    repo_path: &'a str,
    head_name: Option<&'a str>,
    head_detached: bool,
    /// (ahead, behind) of the HEAD branch vs its upstream, when tracking one.
    head_ahead_behind: Option<(usize, usize)>,
    error_message: Option<&'a str>,
    message: Option<&'a str>,
    is_busy: bool,
    is_uncommitted: bool,
    is_filtering: bool,
    editing_commit: bool,
    amending_commit: bool,
    search_info: Option<String>,
    op_state: OperationState,
    conflict_count: usize,
    /// Open-PR hint for the selected commit (`o: open PR #N`), when it has one.
    pr_hint: Option<String>,
    /// Whether the selected commit's PR has CI checks (`c checks` hint).
    pr_has_ci: bool,
    /// Whether the graph column is capped or cappable (needs > 4 cells), so the
    /// `< >` resize hint is worth showing.
    graph_cappable: bool,
    /// Whether the graph is branchy enough (> 2 lanes) for branch tracing, so
    /// the `t trace` hint is worth showing; carries the current on/off state.
    trace_traceable: bool,
    trace_enabled: bool,
    theme: &'a Theme,
}

impl<'a> StatusBar<'a> {
    pub fn new(app: &'a App, theme: &'a Theme) -> Self {
        let error_message = match &app.mode {
            AppMode::Error { message } => Some(message.as_str()),
            _ => None,
        };

        // The selected commit's open PR, if any — drives the o/c hints.
        let selected_pr = app.selected_commit_node().and_then(|node| {
            crate::ui::graph_view::pr_for_branch_labels(
                &node.branch_names,
                &app.remotes,
                &app.open_prs,
            )
        });

        // Generate search status message
        let search_info = match &app.mode {
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

        Self {
            mode: &app.mode,
            focused_panel: app.focused_panel,
            repo_path: &app.repo_path,
            head_name: app.head_name.as_deref(),
            head_detached: app.head_detached,
            head_ahead_behind: app
                .branches
                .iter()
                .find(|b| b.is_head && b.upstream.is_some())
                .map(|b| (b.ahead, b.behind)),
            error_message,
            message: app.get_message(),
            is_busy: app.is_network_busy(),
            is_uncommitted: app.is_uncommitted_selected(),
            is_filtering: app.files_pane.files_filter_active,
            editing_commit: app.editing_commit_message,
            amending_commit: app.amending_commit,
            search_info,
            op_state: app.op_state,
            conflict_count: app.conflict_count,
            pr_hint: pr_hint_label(selected_pr.map(|pr| pr.number)),
            pr_has_ci: selected_pr.is_some_and(|pr| pr.ci != crate::pr::CiStatus::None),
            graph_cappable: (app.graph_layout.max_lane + 1) * 2 > 4,
            trace_traceable: crate::git::graph::graph_has_enough_lanes(&app.graph_layout),
            trace_enabled: app.trace_enabled,
            theme,
        }
    }
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let key_style = Style::default()
            .fg(self.theme.status_key_fg)
            .bg(self.theme.status_key_bg)
            .add_modifier(Modifier::BOLD);
        let desc_style = Style::default().fg(self.theme.text_primary);
        let mode_style = Style::default()
            .fg(self.theme.status_mode_fg)
            .bg(self.theme.status_mode_bg)
            .add_modifier(Modifier::BOLD);
        let repo_style = Style::default()
            .fg(self.theme.status_repo_fg)
            .bg(self.theme.status_repo_bg)
            .add_modifier(Modifier::BOLD);

        let mut spans: Vec<Span> = Vec::new();

        // Show the repository name (folder name) on the left
        let repo_name = std::path::Path::new(self.repo_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(self.repo_path);
        spans.push(Span::styled(format!(" {} ", repo_name), repo_style));
        spans.push(Span::raw(" "));

        // HEAD branch
        if let Some(head) = self.head_name {
            if self.head_detached {
                spans.push(Span::styled(
                    format!(" DETACHED: {} ", head),
                    Style::default()
                        .fg(self.theme.status_detached_fg)
                        .bg(self.theme.status_detached_bg)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(
                    format!(" {} ", head),
                    Style::default().fg(self.theme.status_branch_fg).bg(self.theme.status_branch_bg),
                ));
            }
            spans.push(Span::raw(" "));

            // Ahead/behind vs upstream (e.g. "↑2 ↓1"), when tracking one and
            // diverged.
            if let Some((ahead, behind)) = self.head_ahead_behind {
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
                    spans.push(Span::styled(
                        format!(" {label} "),
                        Style::default()
                            .fg(self.theme.text_primary)
                            .add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::raw(" "));
                }
            }
        }

        // In-progress operation indicator (merge/rebase/…): prominent, shown in
        // Normal mode regardless of message/hints so conflicts stay visible.
        if matches!(self.mode, AppMode::Normal) && self.op_state.is_in_progress() {
            let op_style = Style::default()
                .fg(self.theme.status_error_fg)
                .bg(self.theme.status_error_bg)
                .add_modifier(Modifier::BOLD);
            let label = if self.conflict_count > 0 {
                format!(
                    " {} ({} conflict{}) ",
                    self.op_state.label(),
                    self.conflict_count,
                    if self.conflict_count == 1 { "" } else { "s" }
                )
            } else {
                format!(" {} ", self.op_state.label())
            };
            spans.push(Span::styled(label, op_style));
            // c/A (and o/t for conflicts) are only wired up in the files pane,
            // so only advertise them once that's actually where the hint applies.
            if self.focused_panel == FocusedPanel::Files {
                spans.push(Span::styled(" c ", key_style));
                spans.push(Span::styled("continue ", desc_style));
                spans.push(Span::styled(" A ", key_style));
                spans.push(Span::styled("abort ", desc_style));
            }
            spans.push(Span::raw("  "));
        }

        // Key hints (vary by mode)
        match self.mode {
            AppMode::Normal => match self.message {
                Some(msg) => {
                    let bg = if self.is_busy {
                        self.theme.status_busy_bg
                    } else {
                        self.theme.status_success_bg
                    };
                    let msg_style = Style::default()
                        .fg(self.theme.status_key_fg)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD);
                    spans.push(Span::styled(format!(" {} ", msg), msg_style));
                    spans.push(Span::raw("  "));
                }
                None => {
                    // Show search info if available
                    if let Some(info) = &self.search_info {
                        let search_style = Style::default()
                            .fg(self.theme.status_branch_fg)
                            .bg(self.theme.status_branch_bg)
                            .add_modifier(Modifier::BOLD);
                        spans.push(Span::styled(format!(" {} ", info), search_style));
                        spans.push(Span::raw("  "));
                    }

                    if self.editing_commit {
                        if self.amending_commit {
                            spans.push(Span::styled(" Enter ", key_style));
                            spans.push(Span::styled("save amend ", desc_style));
                        } else {
                            spans.push(Span::styled(" Enter ", key_style));
                            spans.push(Span::styled("commit ", desc_style));
                            spans.push(Span::styled(" Ctrl+Enter ", key_style));
                            spans.push(Span::styled("amend ", desc_style));
                            spans.push(Span::styled(" Ctrl+S ", key_style));
                            spans.push(Span::styled("stash ", desc_style));
                        }
                        spans.push(Span::styled(" Esc ", key_style));
                        spans.push(Span::styled("cancel", desc_style));
                    } else {
                    match self.focused_panel {
                        FocusedPanel::Graph => {
                            spans.push(Span::styled(" ↑↓ ", key_style));
                            spans.push(Span::styled("move ", desc_style));
                            spans.push(Span::styled(" Enter ", key_style));
                            spans.push(Span::styled("actions ", desc_style));
                            // Only when the selected commit has an open PR.
                            if let Some(hint) = &self.pr_hint {
                                spans.push(Span::styled(" o ", key_style));
                                spans.push(Span::styled(format!("{hint} "), desc_style));
                                // ...and a CI checks hint when the PR reports checks.
                                if self.pr_has_ci {
                                    spans.push(Span::styled(" c ", key_style));
                                    spans.push(Span::styled("checks ", desc_style));
                                }
                                spans.push(Span::styled(" v ", key_style));
                                spans.push(Span::styled("thread ", desc_style));
                            }
                            // Only when the graph is wide enough to be capped.
                            if self.graph_cappable {
                                spans.push(Span::styled(" <> ", key_style));
                                spans.push(Span::styled("width ", desc_style));
                            }
                            // Only on branchy graphs, where tracing helps.
                            if self.trace_traceable {
                                spans.push(Span::styled(" t ", key_style));
                                let label = if self.trace_enabled {
                                    "trace on "
                                } else {
                                    "trace off "
                                };
                                spans.push(Span::styled(label, desc_style));
                            }
                            spans.push(Span::styled(" ←→ ", key_style));
                            spans.push(Span::styled("panels ", desc_style));
                            spans.push(Span::styled(" B ", key_style));
                            spans.push(Span::styled("branches ", desc_style));
                            spans.push(Span::styled(" ? ", key_style));
                            spans.push(Span::styled("help", desc_style));
                        }
                        FocusedPanel::Files if self.is_filtering => {
                            let filter_style = Style::default()
                                .fg(self.theme.status_mode_fg)
                                .bg(self.theme.status_mode_bg)
                                .add_modifier(Modifier::BOLD);
                            spans.push(Span::styled(" FILTER ", filter_style));
                            spans.push(Span::raw("  "));
                            spans.push(Span::styled(" Enter ", key_style));
                            spans.push(Span::styled("confirm ", desc_style));
                            spans.push(Span::styled(" Esc ", key_style));
                            spans.push(Span::styled("cancel ", desc_style));
                        }
                        FocusedPanel::Files => {
                            spans.push(Span::styled(" ↑↓ ", key_style));
                            spans.push(Span::styled("select ", desc_style));
                            spans.push(Span::styled(" Enter ", key_style));
                            spans.push(Span::styled("diff ", desc_style));
                            spans.push(Span::styled(" Space ", key_style));
                            spans.push(Span::styled("open ", desc_style));
                            if self.is_uncommitted {
                                spans.push(Span::styled(" s ", key_style));
                                spans.push(Span::styled("stage ", desc_style));
                                spans.push(Span::styled(" r ", key_style));
                                spans.push(Span::styled("restore ", desc_style));
                                spans.push(Span::styled(" i ", key_style));
                                spans.push(Span::styled("ignore ", desc_style));
                                spans.push(Span::styled(" v ", key_style));
                                spans.push(Span::styled("archive ", desc_style));
                                spans.push(Span::styled(" Del ", key_style));
                                spans.push(Span::styled("trash ", desc_style));
                                spans.push(Span::styled(" ^z ", key_style));
                                spans.push(Span::styled("undo ", desc_style));
                            }
                            spans.push(Span::styled(" f ", key_style));
                            spans.push(Span::styled("folders ", desc_style));
                            spans.push(Span::styled(" ^f ", key_style));
                            spans.push(Span::styled("filter ", desc_style));
                            spans.push(Span::styled(" ←→ ", key_style));
                            spans.push(Span::styled("panels ", desc_style));
                            spans.push(Span::styled(" ? ", key_style));
                            spans.push(Span::styled("help", desc_style));
                        }
                        FocusedPanel::CommitDetail => {
                            spans.push(Span::styled(" ↑↓ ", key_style));
                            spans.push(Span::styled("scroll ", desc_style));
                            if self.is_uncommitted {
                                spans.push(Span::styled(" Enter ", key_style));
                                spans.push(Span::styled("edit msg ", desc_style));
                                spans.push(Span::styled(" Ctrl+Enter ", key_style));
                                spans.push(Span::styled("amend ", desc_style));
                                spans.push(Span::styled(" Ctrl+S ", key_style));
                                spans.push(Span::styled("stash ", desc_style));
                            }
                            spans.push(Span::styled(" ←→ ", key_style));
                            spans.push(Span::styled("panels ", desc_style));
                            spans.push(Span::styled(" Esc ", key_style));
                            spans.push(Span::styled("graph ", desc_style));
                            spans.push(Span::styled(" ? ", key_style));
                            spans.push(Span::styled("help", desc_style));
                        }
                    }
                    }
                }
            },
            AppMode::Help => {
                spans.push(Span::styled(" Esc/q ", key_style));
                spans.push(Span::styled("close help", desc_style));
            }
            AppMode::Input { .. } => {
                spans.push(Span::styled(" Enter ", key_style));
                spans.push(Span::styled("confirm ", desc_style));
                spans.push(Span::styled(" Esc ", key_style));
                spans.push(Span::styled("cancel", desc_style));
            }
            AppMode::Confirm { .. } => {
                spans.push(Span::styled(" y ", key_style));
                spans.push(Span::styled("yes ", desc_style));
                spans.push(Span::styled(" n ", key_style));
                spans.push(Span::styled("no", desc_style));
            }
            AppMode::Error { .. } => {
                // In error mode, show the message and hide key hints
                let error_style = Style::default()
                    .fg(self.theme.status_error_fg)
                    .bg(self.theme.status_error_bg)
                    .add_modifier(Modifier::BOLD);
                if let Some(msg) = self.error_message {
                    spans.push(Span::styled(format!(" {} ", msg), error_style));
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(" Esc/Enter ", key_style));
                    spans.push(Span::styled("close", desc_style));
                }
            }
            AppMode::FileDiff { .. } => {
                spans.push(Span::styled(" n/N ", key_style));
                spans.push(Span::styled("file ", desc_style));
                spans.push(Span::styled(" ]/[ ", key_style));
                spans.push(Span::styled("hunk ", desc_style));
                spans.push(Span::styled(" ↑↓ ", key_style));
                spans.push(Span::styled("scroll ", desc_style));
                spans.push(Span::styled(" ←→ ", key_style));
                spans.push(Span::styled("pan ", desc_style));
                spans.push(Span::styled(" Esc ", key_style));
                spans.push(Span::styled("back", desc_style));
            }
            AppMode::CommitMenu { .. }
            | AppMode::BranchPicker { .. }
            | AppMode::BranchDeletePicker { .. }
            | AppMode::TagPicker { .. }
            | AppMode::RemotePicker { .. } => {
                spans.push(Span::styled(" ↑/↓ ", key_style));
                spans.push(Span::styled("select ", desc_style));
                spans.push(Span::styled(" Enter ", key_style));
                spans.push(Span::styled("confirm ", desc_style));
                spans.push(Span::styled(" Esc ", key_style));
                spans.push(Span::styled("cancel", desc_style));
            }
            AppMode::MetadataMenu { .. } => {
                spans.push(Span::styled(" ↑/↓ ", key_style));
                spans.push(Span::styled("move ", desc_style));
                spans.push(Span::styled(" Space ", key_style));
                spans.push(Span::styled("toggle ", desc_style));
                spans.push(Span::styled(" Esc ", key_style));
                spans.push(Span::styled("close", desc_style));
            }
            AppMode::PullDivergence { .. } => {
                spans.push(Span::styled(" ↑/↓ ", key_style));
                spans.push(Span::styled("move ", desc_style));
                spans.push(Span::styled(" Enter ", key_style));
                spans.push(Span::styled("choose ", desc_style));
                spans.push(Span::styled(" Esc ", key_style));
                spans.push(Span::styled("cancel", desc_style));
            }
            AppMode::CiChecks => {
                spans.push(Span::styled(" ↑↓ ", key_style));
                spans.push(Span::styled("nav ", desc_style));
                spans.push(Span::styled(" Enter ", key_style));
                spans.push(Span::styled("details ", desc_style));
                spans.push(Span::styled(" o ", key_style));
                spans.push(Span::styled("open ", desc_style));
                spans.push(Span::styled(" Esc ", key_style));
                spans.push(Span::styled("back", desc_style));
            }
            AppMode::PrThread => {
                spans.push(Span::styled(" ↑↓ ", key_style));
                spans.push(Span::styled("scroll ", desc_style));
                spans.push(Span::styled(" o ", key_style));
                spans.push(Span::styled("open PR ", desc_style));
                spans.push(Span::styled(" r ", key_style));
                spans.push(Span::styled("review ", desc_style));
                spans.push(Span::styled(" Esc ", key_style));
                spans.push(Span::styled("close", desc_style));
            }
            AppMode::PrCompose { .. } => {
                spans.push(Span::styled(" Ctrl+S ", key_style));
                spans.push(Span::styled("submit ", desc_style));
                spans.push(Span::styled(" Esc ", key_style));
                spans.push(Span::styled("cancel", desc_style));
            }
            AppMode::PrMergePicker { .. } | AppMode::PrReviewPicker { .. } => {
                spans.push(Span::styled(" ↑/↓ ", key_style));
                spans.push(Span::styled("move ", desc_style));
                spans.push(Span::styled(" Enter ", key_style));
                spans.push(Span::styled("choose ", desc_style));
                spans.push(Span::styled(" Esc ", key_style));
                spans.push(Span::styled("cancel", desc_style));
            }
            AppMode::BranchFilter { .. } => {
                spans.push(Span::styled(" Space ", key_style));
                spans.push(Span::styled("toggle ", desc_style));
                spans.push(Span::styled(" C-a ", key_style));
                spans.push(Span::styled("all ", desc_style));
                spans.push(Span::styled(" C-o ", key_style));
                spans.push(Span::styled("none ", desc_style));
                spans.push(Span::styled(" Esc ", key_style));
                spans.push(Span::styled("close", desc_style));
            }
            AppMode::FileHistory { .. } => {
                spans.push(Span::styled(" ↑/↓ ", key_style));
                spans.push(Span::styled("select ", desc_style));
                spans.push(Span::styled(" Enter ", key_style));
                spans.push(Span::styled("open diff ", desc_style));
                spans.push(Span::styled(" Esc ", key_style));
                spans.push(Span::styled("back", desc_style));
            }
            AppMode::CommandPalette { .. } => {
                spans.push(Span::styled(" type ", key_style));
                spans.push(Span::styled("filter ", desc_style));
                spans.push(Span::styled(" ↑/↓ ", key_style));
                spans.push(Span::styled("move ", desc_style));
                spans.push(Span::styled(" Enter ", key_style));
                spans.push(Span::styled("run ", desc_style));
                spans.push(Span::styled(" Esc ", key_style));
                spans.push(Span::styled("close", desc_style));
            }
        }

        let line = Line::from(spans);
        buf.set_line(area.x, area.y, &line, area.width);

        // Show the mode on the right (only for non-Normal modes)
        let mode_text = match self.mode {
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
        if let Some(text) = mode_text {
            let mode_len = text.len() as u16;
            if area.width > mode_len {
                let x = area.x + area.width - mode_len;
                buf.set_string(x, area.y, text, mode_style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_hint_shown_only_when_a_pr_exists() {
        assert_eq!(pr_hint_label(Some(12)).as_deref(), Some("open PR #12"));
        assert_eq!(pr_hint_label(None), None);
    }
}
