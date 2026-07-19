//! GitHub Issues popups: the list, a single issue's detail, composing new
//! issues/comments, the label picker, assignee edits, and close/reopen. Mirrors
//! the PR feature's handlers (`ci_checks_actions`, `pr_thread_actions`,
//! `pr_action_actions`): fetches run in the background and their results fill
//! the view state; errors render inside the popup (never `AppMode::Error`);
//! mutating actions run through the shared async runner.

use super::*;
use crate::issue::{IssueDetail, IssueFilter, IssueInfo, IssueLabel};
use crate::issue_action::{success_message, IssueAction};
use crate::toast::ToastKind;

impl App {
    // ── list ──────────────────────────────────────────────────────────

    /// Open the issue list (defaulting to open issues) and kick off the fetch.
    /// Also prefetches the repo label set so the label picker opens instantly.
    pub(crate) fn open_issue_list(&mut self) {
        let filter = IssueFilter::Open;
        self.issue_list = Some(IssueListView {
            state: IssueListState::Loading,
            selected: 0,
            filter,
            scroll: 0,
        });
        self.issue_fetch.start_list(&self.repo_path, filter);
        self.issue_fetch.start_labels(&self.repo_path);
        self.mode = AppMode::IssueList;
    }

    pub(crate) fn handle_issue_list_action(&mut self, action: Action) {
        match action {
            Action::MoveUp => self.issue_list_move(-1, true),
            Action::MoveDown => self.issue_list_move(1, true),
            Action::PageUp => self.issue_list_move(-10, false),
            Action::PageDown => self.issue_list_move(10, false),
            Action::GoToTop => self.issue_list_move(i32::MIN, false),
            Action::GoToBottom => self.issue_list_move(i32::MAX, false),
            Action::OpenIssueDetail => self.open_issue_detail(),
            Action::CycleIssueFilter => self.cycle_issue_filter(),
            Action::RefreshIssues => self.refresh_issue_list(),
            Action::NewIssue => self.open_issue_compose(IssueComposePurpose::NewIssue),
            Action::OpenIssueInBrowser => self.open_selected_issue_url(),
            Action::Cancel => {
                self.issue_list = None;
                self.mode = AppMode::Normal;
            }
            _ => {}
        }
    }

    /// Move the list selection by `delta`. `wrap` wraps at both ends (j/k);
    /// otherwise it clamps (page/top/bottom, with `i32::MIN`/`MAX` as jumps).
    fn issue_list_move(&mut self, delta: i32, wrap: bool) {
        if let Some(v) = &mut self.issue_list {
            if let IssueListState::Ready(issues) = &v.state {
                let len = issues.len();
                if len == 0 {
                    return;
                }
                v.selected = if wrap {
                    wrapped_index(v.selected, len, delta)
                } else {
                    clamped_index(v.selected, len, delta)
                };
            }
        }
    }

    fn cycle_issue_filter(&mut self) {
        if let Some(v) = &mut self.issue_list {
            let filter = v.filter.next();
            v.filter = filter;
            v.state = IssueListState::Loading;
            v.selected = 0;
            v.scroll = 0;
            self.issue_fetch.start_list(&self.repo_path, filter);
        }
    }

    fn refresh_issue_list(&mut self) {
        if let Some(v) = &mut self.issue_list {
            let filter = v.filter;
            v.state = IssueListState::Loading;
            self.issue_fetch.start_list(&self.repo_path, filter);
        }
    }

    /// The issue currently selected in the list, if the list is `Ready`.
    fn selected_issue(&self) -> Option<&IssueInfo> {
        let v = self.issue_list.as_ref()?;
        match &v.state {
            IssueListState::Ready(issues) => issues.get(v.selected),
            _ => None,
        }
    }

    fn open_selected_issue_url(&mut self) {
        let url = self.selected_issue().map(|i| i.url.clone());
        match url {
            Some(url) if !url.is_empty() => self.open_issue_url(&url),
            _ => self.set_message("No URL for this issue"),
        }
    }

    // ── detail ────────────────────────────────────────────────────────

    /// Open the detail popup for the selected issue, using the session cache
    /// when available and otherwise fetching in the background.
    fn open_issue_detail(&mut self) {
        let Some(number) = self.selected_issue().map(|i| i.number) else {
            return;
        };
        let state = if let Some(detail) = self.issue_fetch.cached_detail(number) {
            IssueDetailState::Ready(Box::new(detail.clone()))
        } else {
            self.issue_fetch.start_detail(&self.repo_path, number);
            IssueDetailState::Loading
        };
        self.issue_detail = Some(IssueDetailView {
            number,
            state,
            scroll: 0,
            max_scroll: 0,
        });
        self.mode = AppMode::IssueDetail;
    }

    pub(crate) fn handle_issue_detail_action(&mut self, action: Action) {
        match action {
            Action::MoveUp => self.issue_detail_scroll(-1),
            Action::MoveDown => self.issue_detail_scroll(1),
            Action::PageUp => self.issue_detail_scroll(-15),
            Action::PageDown => self.issue_detail_scroll(15),
            Action::GoToTop => self.issue_detail_scroll(i32::MIN),
            Action::GoToBottom => self.issue_detail_scroll(i32::MAX),
            Action::CommentOnIssue => {
                if let Some(number) = self.detail_number() {
                    self.open_issue_compose(IssueComposePurpose::Comment { number });
                }
            }
            Action::ToggleIssueState => self.confirm_toggle_issue_state(),
            Action::EditIssueLabels => self.open_issue_label_picker(),
            Action::EditIssueAssignees => self.open_issue_assignees_input(),
            Action::OpenIssueInBrowser => self.open_detail_url(),
            Action::RefreshIssues => self.refresh_issue_detail(),
            Action::Cancel => {
                self.issue_detail = None;
                self.mode = AppMode::IssueList;
            }
            _ => {}
        }
    }

    /// The number of the issue whose detail popup is open.
    fn detail_number(&self) -> Option<u64> {
        self.issue_detail.as_ref().map(|v| v.number)
    }

    /// The loaded detail for the open popup, if `Ready`.
    fn loaded_detail(&self) -> Option<&IssueDetail> {
        match self.issue_detail.as_ref()?.state {
            IssueDetailState::Ready(ref d) => Some(d.as_ref()),
            _ => None,
        }
    }

    fn issue_detail_scroll(&mut self, delta: i32) {
        if let Some(v) = &mut self.issue_detail {
            v.scroll = match delta {
                i32::MIN => 0,
                i32::MAX => v.max_scroll,
                d => (v.scroll as i64 + d as i64).clamp(0, v.max_scroll as i64) as usize,
            };
        }
    }

    fn refresh_issue_detail(&mut self) {
        if let Some(v) = &mut self.issue_detail {
            let number = v.number;
            v.state = IssueDetailState::Loading;
            v.scroll = 0;
            self.issue_fetch.invalidate_detail(number);
            self.issue_fetch.start_detail(&self.repo_path, number);
        }
    }

    fn open_detail_url(&mut self) {
        let url = self.loaded_detail().map(|d| d.url.clone());
        match url {
            Some(url) if !url.is_empty() => self.open_issue_url(&url),
            _ => self.set_message("No URL for this issue"),
        }
    }

    /// Close an open issue / reopen a closed one, via the Confirm dialog.
    fn confirm_toggle_issue_state(&mut self) {
        let Some(detail) = self.loaded_detail() else {
            return;
        };
        let number = detail.number;
        let action = match detail.state {
            crate::issue::IssueState::Open => IssueAction::Close { number },
            crate::issue::IssueState::Closed => IssueAction::Reopen { number },
        };
        self.mode = AppMode::Confirm {
            message: format!("{}?", action.describe()),
            action: ConfirmAction::IssueAction(action),
        };
    }

    // ── compose (new issue / comment) ──────────────────────────────────

    fn open_issue_compose(&mut self, purpose: IssueComposePurpose) {
        self.issue_editor = crate::text_editor::TextEditor::new();
        self.mode = AppMode::IssueCompose { purpose };
    }

    /// Open the new-issue compose editor (used by the command palette).
    pub(crate) fn open_new_issue_compose(&mut self) {
        self.open_issue_compose(IssueComposePurpose::NewIssue);
    }

    /// The mode to return to when a compose is cancelled/finished.
    fn compose_return_mode(purpose: IssueComposePurpose) -> AppMode {
        match purpose {
            IssueComposePurpose::NewIssue => AppMode::IssueList,
            IssueComposePurpose::Comment { .. } => AppMode::IssueDetail,
        }
    }

    pub(crate) fn handle_issue_compose_action(&mut self, action: Action) {
        match action {
            Action::Cancel => {
                let purpose = match self.mode {
                    AppMode::IssueCompose { purpose } => purpose,
                    _ => return,
                };
                self.issue_editor = crate::text_editor::TextEditor::new();
                self.mode = Self::compose_return_mode(purpose);
            }
            Action::SubmitCompose => self.submit_issue_compose(),
            Action::ExternalEdit => {
                self.pending_external_edit =
                    Some(crate::external_edit::ExternalEditTarget::Issue);
            }
            other => {
                super::commit_editor_actions::apply_editor_edit(&mut self.issue_editor, &other);
            }
        }
    }

    fn submit_issue_compose(&mut self) {
        let AppMode::IssueCompose { purpose } = self.mode else {
            return;
        };
        let text = self.issue_editor.text.clone();
        match purpose {
            IssueComposePurpose::NewIssue => {
                let (title, body) = compose_title_body(&text);
                if title.is_empty() {
                    self.toast(ToastKind::Error, "Issue title can't be empty");
                    return;
                }
                self.issue_editor = crate::text_editor::TextEditor::new();
                self.start_issue_action(IssueAction::Create { title, body }, "Creating issue…");
                self.mode = AppMode::IssueList;
            }
            IssueComposePurpose::Comment { number } => {
                let body = text.trim().to_string();
                if body.is_empty() {
                    self.toast(ToastKind::Error, "Comment can't be empty");
                    return;
                }
                self.issue_editor = crate::text_editor::TextEditor::new();
                self.start_issue_action(
                    IssueAction::Comment { number, body },
                    "Adding comment…",
                );
                self.mode = AppMode::IssueDetail;
            }
        }
    }

    // ── label picker ───────────────────────────────────────────────────

    fn open_issue_label_picker(&mut self) {
        let Some(detail) = self.loaded_detail() else {
            return;
        };
        let number = detail.number;
        let current: std::collections::HashSet<String> =
            detail.labels.iter().map(|l| l.name.clone()).collect();
        let Some(labels) = self.issue_fetch.cached_labels().cloned() else {
            // Not fetched yet — kick it off and let the user retry.
            self.issue_fetch.start_labels(&self.repo_path);
            self.toast(ToastKind::Info, "Loading labels… press l again");
            return;
        };
        if labels.is_empty() {
            self.toast(ToastKind::Info, "No labels defined in this repo");
            return;
        }
        let original: Vec<bool> = labels.iter().map(|l| current.contains(&l.name)).collect();
        let chosen = original.clone();
        self.issue_label_picker = Some(IssueLabelPicker {
            number,
            labels,
            original,
            chosen,
        });
        self.mode = AppMode::IssueLabelPicker {
            number,
            selected: 0,
        };
    }

    pub(crate) fn handle_issue_label_picker_action(&mut self, action: Action) {
        let len = self
            .issue_label_picker
            .as_ref()
            .map(|p| p.labels.len())
            .unwrap_or(0);
        match action {
            Action::MoveUp => {
                if let AppMode::IssueLabelPicker { selected, .. } = &mut self.mode {
                    *selected = wrapped_index(*selected, len, -1);
                }
            }
            Action::MoveDown => {
                if let AppMode::IssueLabelPicker { selected, .. } = &mut self.mode {
                    *selected = wrapped_index(*selected, len, 1);
                }
            }
            Action::ToggleIssueLabel => {
                let cursor = match self.mode {
                    AppMode::IssueLabelPicker { selected, .. } => selected,
                    _ => return,
                };
                if let Some(p) = &mut self.issue_label_picker {
                    if let Some(slot) = p.chosen.get_mut(cursor) {
                        *slot = !*slot;
                    }
                }
            }
            Action::MenuSelect => self.apply_issue_labels(),
            Action::Cancel => {
                self.issue_label_picker = None;
                self.mode = AppMode::IssueDetail;
            }
            _ => {}
        }
    }

    fn apply_issue_labels(&mut self) {
        let Some(picker) = self.issue_label_picker.take() else {
            self.mode = AppMode::IssueDetail;
            return;
        };
        let (add, remove) = label_diff(&picker.labels, &picker.original, &picker.chosen);
        self.mode = AppMode::IssueDetail;
        if add.is_empty() && remove.is_empty() {
            self.toast(ToastKind::Info, "No label changes");
            return;
        }
        self.start_issue_action(
            IssueAction::EditLabels {
                number: picker.number,
                add,
                remove,
            },
            "Updating labels…",
        );
    }

    // ── assignees ──────────────────────────────────────────────────────

    fn open_issue_assignees_input(&mut self) {
        let Some(detail) = self.loaded_detail() else {
            return;
        };
        let number = detail.number;
        let current = detail.assignees.join(", ");
        self.mode = AppMode::Input {
            title: format!("Assignees for #{number} (comma-separated logins)"),
            input: current,
            action: InputAction::EditIssueAssignees { number },
        };
    }

    /// Diff the typed logins against the issue's current assignees and, if
    /// anything changed, run the edit. Called from the Input confirm path.
    pub(crate) fn submit_issue_assignees(&mut self, number: u64, input: &str) {
        let current = self.current_issue_assignees(number);
        let desired = parse_logins(input);
        let (add, remove) = assignee_diff(&current, &desired);
        if add.is_empty() && remove.is_empty() {
            self.toast(ToastKind::Info, "No assignee changes");
            return;
        }
        self.start_issue_action(
            IssueAction::EditAssignees {
                number,
                add,
                remove,
            },
            "Updating assignees…",
        );
    }

    /// The current assignees of issue `number`, read from the open detail view.
    fn current_issue_assignees(&self, number: u64) -> Vec<String> {
        self.loaded_detail()
            .filter(|d| d.number == number)
            .map(|d| d.assignees.clone())
            .unwrap_or_default()
    }

    // ── shared execution ───────────────────────────────────────────────

    /// Run a close/reopen confirmed through the Confirm dialog. Returns to the
    /// detail popup (the runner is async).
    pub(crate) fn run_issue_action(&mut self, action: IssueAction) {
        self.start_issue_action(action, "Working…");
        self.mode = if self.issue_detail.is_some() {
            AppMode::IssueDetail
        } else {
            AppMode::IssueList
        };
    }

    /// Start an issue action in the background, guarding against a busy runner.
    fn start_issue_action(&mut self, action: IssueAction, busy_msg: &str) {
        if self.issue_action_runner.is_busy() {
            self.toast(ToastKind::Info, "busy: another issue operation in progress");
            return;
        }
        self.issue_action_runner.start(&self.repo_path, action);
        self.toast(ToastKind::Info, busy_msg.to_string());
    }

    /// Open a URL in the browser, with status-line feedback.
    fn open_issue_url(&mut self, url: &str) {
        if let Err(e) = open_url(url) {
            self.show_error(format!("Could not open: {e}"));
        } else {
            self.set_message("Opening in browser");
        }
    }

    // ── async polling ──────────────────────────────────────────────────

    /// Poll the background issue fetches and the action runner, filling the open
    /// popups. Returns true when something changed (triggering a re-render).
    pub fn update_issue_status(&mut self) -> bool {
        let mut changed = false;

        if let Some(result) = self.issue_fetch.poll_list() {
            if let Some(v) = &mut self.issue_list {
                if matches!(v.state, IssueListState::Loading) {
                    v.state = list_state_from(result);
                    v.selected = 0;
                    v.scroll = 0;
                }
            }
            changed = true;
        }

        if let Some((number, result)) = self.issue_fetch.poll_detail() {
            if let Some(v) = &mut self.issue_detail {
                if v.number == number && matches!(v.state, IssueDetailState::Loading) {
                    v.state = detail_state_from(result);
                    v.scroll = 0;
                }
            }
            changed = true;
        }

        // Draining a completed label fetch caches it (inside `poll_labels`); the
        // picker reads the cache lazily when it opens.
        if self.issue_fetch.poll_labels().is_some() {
            changed = true;
        }

        if let Some((action, result)) = self.issue_action_runner.poll() {
            match result {
                Ok(stdout) => {
                    self.toast(ToastKind::Success, success_message(&action, &stdout));
                    self.after_issue_action(&action);
                }
                Err(e) => self.toast(ToastKind::Error, first_line(&e)),
            }
            changed = true;
        }

        changed
    }

    /// After a successful mutation, refetch the affected detail (comment / close
    /// / reopen / labels / assignees) and the list so both reflect the change.
    fn after_issue_action(&mut self, action: &IssueAction) {
        if let Some(number) = issue_action_number(action) {
            self.issue_fetch.invalidate_detail(number);
            if let Some(v) = &mut self.issue_detail {
                if v.number == number {
                    v.state = IssueDetailState::Loading;
                    v.scroll = 0;
                    self.issue_fetch.start_detail(&self.repo_path, number);
                }
            }
        }
        if let Some(v) = &mut self.issue_list {
            let filter = v.filter;
            v.state = IssueListState::Loading;
            self.issue_fetch.start_list(&self.repo_path, filter);
        }
    }
}

// ── pure helpers (unit-tested) ─────────────────────────────────────────

/// Move `current` by `delta` within `len`, wrapping at both ends. Empty ⇒ 0.
fn wrapped_index(current: usize, len: usize, delta: i32) -> usize {
    if len == 0 {
        return 0;
    }
    let n = len as i64;
    (((current as i64 + delta as i64) % n + n) % n) as usize
}

/// Move `current` by `delta` within `len`, clamped to `0..=len-1`.
/// `i32::MIN`/`MAX` jump to the ends. Empty ⇒ 0.
fn clamped_index(current: usize, len: usize, delta: i32) -> usize {
    if len == 0 {
        return 0;
    }
    match delta {
        i32::MIN => 0,
        i32::MAX => len - 1,
        d => (current as i64 + d as i64).clamp(0, len as i64 - 1) as usize,
    }
}

/// Split compose text into (title, body): the first line is the title, the rest
/// (trimmed) is the body. Mirrors the PR compose split.
fn compose_title_body(text: &str) -> (String, String) {
    let mut lines = text.lines();
    let title = lines.next().unwrap_or("").trim().to_string();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    (title, body)
}

/// Parse comma-separated logins into a de-duplicated, order-preserving list,
/// dropping blanks and a leading `@`.
fn parse_logins(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in input.split(',') {
        let login = raw.trim().trim_start_matches('@').trim();
        if login.is_empty() || out.iter().any(|x: &String| x == login) {
            continue;
        }
        out.push(login.to_string());
    }
    out
}

/// The add/remove label sets from the picker's original vs chosen bitsets.
/// `add` = newly checked; `remove` = newly unchecked. Order follows `labels`.
fn label_diff(labels: &[IssueLabel], original: &[bool], chosen: &[bool]) -> (Vec<String>, Vec<String>) {
    let mut add = Vec::new();
    let mut remove = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        let was = original.get(i).copied().unwrap_or(false);
        let now = chosen.get(i).copied().unwrap_or(false);
        match (was, now) {
            (false, true) => add.push(label.name.clone()),
            (true, false) => remove.push(label.name.clone()),
            _ => {}
        }
    }
    (add, remove)
}

/// The add/remove assignee sets to reach `desired` from `current`.
fn assignee_diff(current: &[String], desired: &[String]) -> (Vec<String>, Vec<String>) {
    let add = desired
        .iter()
        .filter(|d| !current.iter().any(|c| c == *d))
        .cloned()
        .collect();
    let remove = current
        .iter()
        .filter(|c| !desired.iter().any(|d| d == *c))
        .cloned()
        .collect();
    (add, remove)
}

/// The issue number a mutation targets (`None` for `Create`, which has none yet).
fn issue_action_number(action: &IssueAction) -> Option<u64> {
    match action {
        IssueAction::Create { .. } => None,
        IssueAction::Comment { number, .. }
        | IssueAction::Close { number }
        | IssueAction::Reopen { number }
        | IssueAction::EditLabels { number, .. }
        | IssueAction::EditAssignees { number, .. } => Some(*number),
    }
}

/// Map a completed list fetch to the list view state.
fn list_state_from(result: Result<Vec<IssueInfo>, String>) -> IssueListState {
    match result {
        Ok(issues) => IssueListState::Ready(issues),
        Err(e) => IssueListState::Error(e),
    }
}

/// Map a completed detail fetch to the detail view state.
fn detail_state_from(result: Result<IssueDetail, String>) -> IssueDetailState {
    match result {
        Ok(detail) => IssueDetailState::Ready(Box::new(detail)),
        Err(e) => IssueDetailState::Error(e),
    }
}

/// First non-empty line of a gh error, stripped of a leading `error:`.
fn first_line(err: &str) -> String {
    let line = err
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(err.trim());
    let line = line.strip_prefix("error:").unwrap_or(line).trim();
    if line.is_empty() {
        "issue action failed".to_string()
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::{IssueDetail, IssueInfo, IssueState};

    // ── selection movement ────────────────────────────────────────────

    #[test]
    fn wrapped_index_wraps_both_ends() {
        assert_eq!(wrapped_index(0, 3, -1), 2);
        assert_eq!(wrapped_index(2, 3, 1), 0);
        assert_eq!(wrapped_index(1, 3, 1), 2);
        assert_eq!(wrapped_index(1, 3, -1), 0);
        // Large deltas wrap correctly.
        assert_eq!(wrapped_index(0, 3, -4), 2);
        assert_eq!(wrapped_index(0, 3, 7), 1);
        // Degenerate lengths never panic.
        assert_eq!(wrapped_index(0, 0, 1), 0);
        assert_eq!(wrapped_index(0, 1, -1), 0);
    }

    #[test]
    fn clamped_index_clamps_and_jumps() {
        assert_eq!(clamped_index(0, 5, -1), 0);
        assert_eq!(clamped_index(4, 5, 1), 4);
        assert_eq!(clamped_index(2, 5, 10), 4);
        assert_eq!(clamped_index(2, 5, i32::MIN), 0);
        assert_eq!(clamped_index(2, 5, i32::MAX), 4);
        assert_eq!(clamped_index(0, 0, i32::MAX), 0);
    }

    // ── compose title/body split ───────────────────────────────────────

    #[test]
    fn compose_splits_title_and_body() {
        assert_eq!(
            compose_title_body("Title here\n\nBody line 1\nBody line 2"),
            ("Title here".to_string(), "Body line 1\nBody line 2".to_string())
        );
        // Title only → empty body.
        assert_eq!(
            compose_title_body("Just a title"),
            ("Just a title".to_string(), String::new())
        );
        // Fully empty.
        assert_eq!(compose_title_body(""), (String::new(), String::new()));
        // Whitespace-only first line → empty title.
        assert_eq!(compose_title_body("   \nbody"), (String::new(), "body".to_string()));
        // Unicode survives the split.
        assert_eq!(
            compose_title_body("修复崩溃 🐛\n\n日本語 body"),
            ("修复崩溃 🐛".to_string(), "日本語 body".to_string())
        );
    }

    // ── login parsing ──────────────────────────────────────────────────

    #[test]
    fn parse_logins_trims_dedups_and_drops_blanks() {
        assert_eq!(
            parse_logins("alice, bob ,  , @carol, alice"),
            vec!["alice", "bob", "carol"]
        );
        assert!(parse_logins("").is_empty());
        assert!(parse_logins("  , , ").is_empty());
    }

    // ── label diff ─────────────────────────────────────────────────────

    fn lbl(name: &str) -> IssueLabel {
        IssueLabel {
            name: name.to_string(),
            color: String::new(),
        }
    }

    #[test]
    fn label_diff_reports_only_changes() {
        let labels = vec![lbl("bug"), lbl("p1"), lbl("wontfix"), lbl("docs")];
        // originally: bug + wontfix on the issue.
        let original = vec![true, false, true, false];
        // now: bug + p1 (added p1, removed wontfix, docs untouched).
        let chosen = vec![true, true, false, false];
        let (add, remove) = label_diff(&labels, &original, &chosen);
        assert_eq!(add, vec!["p1"]);
        assert_eq!(remove, vec!["wontfix"]);
    }

    #[test]
    fn label_diff_empty_when_unchanged() {
        let labels = vec![lbl("bug"), lbl("p1")];
        let state = vec![true, false];
        let (add, remove) = label_diff(&labels, &state, &state);
        assert!(add.is_empty() && remove.is_empty());
    }

    // ── assignee diff ──────────────────────────────────────────────────

    #[test]
    fn assignee_diff_computes_add_and_remove() {
        let current = vec!["alice".to_string(), "bob".to_string()];
        let desired = vec!["bob".to_string(), "carol".to_string()];
        let (add, remove) = assignee_diff(&current, &desired);
        assert_eq!(add, vec!["carol"]);
        assert_eq!(remove, vec!["alice"]);
    }

    #[test]
    fn assignee_diff_empty_when_same_set() {
        let current = vec!["alice".to_string(), "bob".to_string()];
        // Same members, different order → no changes.
        let desired = vec!["bob".to_string(), "alice".to_string()];
        let (add, remove) = assignee_diff(&current, &desired);
        assert!(add.is_empty() && remove.is_empty());
        // Clearing all assignees removes everyone.
        let (add, remove) = assignee_diff(&current, &[]);
        assert!(add.is_empty());
        assert_eq!(remove, vec!["alice", "bob"]);
    }

    // ── action-number extraction ───────────────────────────────────────

    #[test]
    fn issue_action_number_maps_variants() {
        assert_eq!(
            issue_action_number(&IssueAction::Create {
                title: "t".into(),
                body: String::new()
            }),
            None
        );
        assert_eq!(issue_action_number(&IssueAction::Close { number: 5 }), Some(5));
        assert_eq!(
            issue_action_number(&IssueAction::EditLabels {
                number: 9,
                add: vec![],
                remove: vec![]
            }),
            Some(9)
        );
    }

    // ── view-state transitions on poll results ─────────────────────────

    fn sample_issue(number: u64) -> IssueInfo {
        IssueInfo {
            number,
            title: "t".into(),
            state: IssueState::Open,
            labels: vec![],
            assignees: vec![],
            author: "ghost".into(),
            updated_at: String::new(),
            url: String::new(),
        }
    }

    #[test]
    fn list_state_transitions_loading_to_ready_and_error() {
        match list_state_from(Ok(vec![sample_issue(1), sample_issue(2)])) {
            IssueListState::Ready(v) => assert_eq!(v.len(), 2),
            _ => panic!("expected Ready"),
        }
        match list_state_from(Err("boom".to_string())) {
            IssueListState::Error(e) => assert_eq!(e, "boom"),
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn detail_state_transitions_loading_to_ready_and_error() {
        let detail = IssueDetail {
            number: 7,
            title: "t".into(),
            state: IssueState::Closed,
            state_reason: None,
            body: String::new(),
            author: "ghost".into(),
            created_at: String::new(),
            updated_at: String::new(),
            labels: vec![],
            assignees: vec![],
            comments: vec![],
            url: String::new(),
        };
        match detail_state_from(Ok(detail)) {
            IssueDetailState::Ready(d) => assert_eq!(d.number, 7),
            _ => panic!("expected Ready"),
        }
        match detail_state_from(Err("nope".to_string())) {
            IssueDetailState::Error(e) => assert_eq!(e, "nope"),
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn first_line_condenses_errors() {
        assert_eq!(first_line("error: could not resolve"), "could not resolve");
        assert_eq!(first_line("\n\nsecond line\nthird"), "second line");
        assert_eq!(first_line("   "), "issue action failed");
    }
}
