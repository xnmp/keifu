//! Mutating PR actions (create / merge / review): eligibility, the compose
//! editor, the pickers, confirmation, and async execution.

use super::*;
use crate::pr_action::{
    can_create_pr, success_message, MergeMethod, PrAction, ReviewDecision,
};
use crate::toast::ToastKind;

impl App {
    // ── eligibility ──────────────────────────────────────────────────

    /// Whether the repo can publish a branch (has at least one remote).
    fn publishable(&self) -> bool {
        !self.repo.remotes().is_empty()
    }

    /// The current (HEAD) branch name, if any.
    fn head_branch_name(&self) -> Option<String> {
        self.branches
            .iter()
            .find(|b| b.is_head && !b.is_remote)
            .map(|b| b.name.clone())
    }

    /// Whether the "Create pull request" menu item should be offered for the
    /// selected commit: it carries the current branch, the repo is publishable,
    /// and no open PR exists for that branch.
    pub(crate) fn can_offer_create_pr(&self) -> bool {
        let Some(head) = self.head_branch_name() else {
            return false;
        };
        // Only from the current branch's tip.
        if !self.selected_node_local_branches().contains(&head) {
            return false;
        }
        can_create_pr(&self.open_prs, &head, self.publishable())
    }

    /// Whether the selected commit carries an open PR (mergeable target).
    pub(crate) fn selected_commit_has_open_pr(&self) -> bool {
        self.selected_open_pr().is_some()
    }

    /// The open PR on the selected commit (by branch label), cloned.
    fn selected_open_pr(&self) -> Option<crate::pr::PrInfo> {
        self.selected_commit_node().and_then(|node| {
            crate::ui::graph_view::pr_for_branch_labels(
                &node.branch_names,
                &self.remotes,
                &self.open_prs,
            )
            .cloned()
        })
    }

    /// PR title by number, from the open-PR map.
    fn pr_title(&self, number: u64) -> Option<String> {
        self.open_prs
            .values()
            .find(|p| p.number == number)
            .map(|p| p.title.clone())
    }

    fn pr_actions_busy(&self) -> bool {
        self.pr_action_runner.is_busy() || self.network.is_busy()
    }

    // ── create ───────────────────────────────────────────────────────

    /// Open the compose editor to create a PR from the current branch. Prefills
    /// the title with the branch tip's commit subject.
    pub(crate) fn open_create_pr(&mut self) {
        if !self.can_offer_create_pr() {
            return;
        }
        let default_title = self
            .selected_commit_node()
            .and_then(|n| n.commit.as_ref())
            .map(|c| c.message.clone())
            .unwrap_or_default();
        self.pr_editor = crate::text_editor::TextEditor::from_text(&default_title);
        self.pr_editor.move_text_end(false);
        self.mode = AppMode::PrCompose {
            purpose: ComposePurpose::CreatePr,
        };
    }

    // ── merge ─────────────────────────────────────────────────────────

    /// Open the merge-method picker for the selected commit's open PR.
    pub(crate) fn open_merge_pr(&mut self) {
        if let Some(pr) = self.selected_open_pr() {
            self.mode = AppMode::PrMergePicker {
                number: pr.number,
                selected: 0,
            };
        }
    }

    pub(crate) fn handle_pr_merge_picker_action(&mut self, action: Action) {
        const N: usize = MergeMethod::ALL.len();
        match action {
            Action::MoveUp => {
                if let AppMode::PrMergePicker { selected, .. } = &mut self.mode {
                    *selected = (*selected + N - 1) % N;
                }
            }
            Action::MoveDown => {
                if let AppMode::PrMergePicker { selected, .. } = &mut self.mode {
                    *selected = (*selected + 1) % N;
                }
            }
            Action::MenuSelect => {
                let (number, idx) = match &self.mode {
                    AppMode::PrMergePicker { number, selected } => (*number, *selected),
                    _ => return,
                };
                let method = MergeMethod::ALL[idx];
                let title = self.pr_title(number).unwrap_or_default();
                self.confirm_pr_action(
                    format!("{} PR #{number} '{title}'?", method.label()),
                    PrAction::Merge { number, method },
                );
            }
            Action::Cancel => self.mode = AppMode::Normal,
            _ => {}
        }
    }

    // ── review ────────────────────────────────────────────────────────

    /// Open the review-disposition picker for `number` (from the thread popup).
    pub(crate) fn open_review_picker(&mut self, number: u64) {
        self.mode = AppMode::PrReviewPicker {
            number,
            selected: 0,
        };
    }

    pub(crate) fn handle_pr_review_picker_action(&mut self, action: Action) {
        const N: usize = ReviewDecision::ALL.len();
        match action {
            Action::MoveUp => {
                if let AppMode::PrReviewPicker { selected, .. } = &mut self.mode {
                    *selected = (*selected + N - 1) % N;
                }
            }
            Action::MoveDown => {
                if let AppMode::PrReviewPicker { selected, .. } = &mut self.mode {
                    *selected = (*selected + 1) % N;
                }
            }
            Action::MenuSelect => {
                let (number, idx) = match &self.mode {
                    AppMode::PrReviewPicker { number, selected } => (*number, *selected),
                    _ => return,
                };
                match ReviewDecision::ALL[idx] {
                    // Approve needs no body → straight to confirm.
                    ReviewDecision::Approve => self.confirm_pr_action(
                        format!("Approve PR #{number}?"),
                        PrAction::Review {
                            number,
                            decision: ReviewDecision::Approve,
                            body: String::new(),
                        },
                    ),
                    // The other two need a body → compose first.
                    ReviewDecision::RequestChanges => self.open_review_compose(
                        ComposePurpose::ReviewRequestChanges { pr: number },
                    ),
                    ReviewDecision::Comment => {
                        self.open_review_compose(ComposePurpose::ReviewComment { pr: number })
                    }
                }
            }
            Action::Cancel => self.mode = AppMode::Normal,
            _ => {}
        }
    }

    fn open_review_compose(&mut self, purpose: ComposePurpose) {
        self.pr_editor = crate::text_editor::TextEditor::new();
        self.mode = AppMode::PrCompose { purpose };
    }

    // ── compose editor ────────────────────────────────────────────────

    pub(crate) fn handle_pr_compose_action(&mut self, action: Action) {
        match action {
            Action::Cancel => {
                self.pr_editor = crate::text_editor::TextEditor::new();
                self.mode = AppMode::Normal;
            }
            Action::SubmitCompose => self.submit_pr_compose(),
            other => {
                super::commit_editor_actions::apply_editor_edit(&mut self.pr_editor, &other);
            }
        }
    }

    fn submit_pr_compose(&mut self) {
        let AppMode::PrCompose { purpose } = self.mode else {
            return;
        };
        let text = self.pr_editor.text.clone();
        match purpose {
            ComposePurpose::CreatePr => {
                let (title, body) = compose_title_body(&text);
                if title.is_empty() {
                    self.toast(ToastKind::Error, "PR title can't be empty");
                    return;
                }
                self.confirm_pr_action(
                    format!("Create pull request '{title}'?"),
                    PrAction::Create { title, body },
                );
            }
            ComposePurpose::ReviewRequestChanges { pr } => {
                let body = text.trim().to_string();
                if body.is_empty() {
                    self.toast(ToastKind::Error, "A body is required to request changes");
                    return;
                }
                self.confirm_pr_action(
                    format!("Request changes on PR #{pr}?"),
                    PrAction::Review {
                        number: pr,
                        decision: ReviewDecision::RequestChanges,
                        body,
                    },
                );
            }
            ComposePurpose::ReviewComment { pr } => {
                let body = text.trim().to_string();
                if body.is_empty() {
                    self.toast(ToastKind::Error, "Comment can't be empty");
                    return;
                }
                // Plain comment is non-destructive → no confirm.
                self.pr_editor = crate::text_editor::TextEditor::new();
                self.run_pr_action(PrAction::Review {
                    number: pr,
                    decision: ReviewDecision::Comment,
                    body,
                });
            }
        }
    }

    // ── confirm + execute ─────────────────────────────────────────────

    /// Route a mutating action through the Confirm dialog.
    fn confirm_pr_action(&mut self, message: String, action: PrAction) {
        self.pr_editor = crate::text_editor::TextEditor::new();
        self.mode = AppMode::Confirm {
            message,
            action: ConfirmAction::PrAction(action),
        };
    }

    /// Start a PR action in the background (guards against a busy network/runner).
    pub(crate) fn run_pr_action(&mut self, action: PrAction) {
        if self.pr_actions_busy() {
            self.toast(ToastKind::Info, "busy: another operation in progress");
            self.mode = AppMode::Normal;
            return;
        }
        let verb = match &action {
            PrAction::Create { .. } => "Creating PR…",
            PrAction::Merge { .. } => "Merging PR…",
            PrAction::Review { .. } => "Submitting review…",
        };
        self.pr_action_runner.start(&self.repo_path, action);
        self.toast(ToastKind::Info, verb);
        self.mode = AppMode::Normal;
    }

    /// Poll the async PR action; toast the result and refresh PR data (and the
    /// graph, after a merge). Returns true when an action completed.
    pub fn update_pr_action_status(&mut self) -> bool {
        let Some((action, result)) = self.pr_action_runner.poll() else {
            return false;
        };
        match result {
            Ok(stdout) => {
                self.toast(ToastKind::Success, success_message(&action, &stdout));
                // Refresh open-PR data so the badge updates promptly.
                self.pr_fetch.force();
                if action.refreshes_graph() {
                    if let Err(e) = self.refresh(true) {
                        self.show_error(format!("Refresh failed: {e}"));
                    }
                }
            }
            Err(e) => {
                // Surface gh's reason (e.g. "not mergeable", "checks required").
                self.toast(ToastKind::Error, humanize_gh_error(&e));
            }
        }
        true
    }
}

/// Split compose-editor text into (title, body): first line is the title, the
/// rest (trimmed, a leading blank line dropped by convention) is the body.
fn compose_title_body(text: &str) -> (String, String) {
    let mut lines = text.lines();
    let title = lines.next().unwrap_or("").trim().to_string();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    (title, body)
}

/// Condense a gh error into a single actionable line (first non-empty line,
/// stripped of any leading "error:"/"failed to run"), keeping gh's own reason.
fn humanize_gh_error(err: &str) -> String {
    let line = err
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(err.trim());
    let line = line
        .strip_prefix("error:")
        .or_else(|| line.strip_prefix("GraphQL:"))
        .unwrap_or(line)
        .trim();
    if line.is_empty() {
        "PR action failed".to_string()
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{compose_title_body, humanize_gh_error};

    #[test]
    fn title_body_split() {
        assert_eq!(
            compose_title_body("My title\n\nBody line 1\nBody line 2"),
            ("My title".to_string(), "Body line 1\nBody line 2".to_string())
        );
        // Title only.
        assert_eq!(
            compose_title_body("Just a title"),
            ("Just a title".to_string(), String::new())
        );
        // Empty.
        assert_eq!(compose_title_body(""), (String::new(), String::new()));
    }

    #[test]
    fn gh_error_condenses_to_a_reason() {
        assert_eq!(
            humanize_gh_error("error: Pull request is not mergeable"),
            "Pull request is not mergeable"
        );
        assert_eq!(
            humanize_gh_error("\n\nGraphQL: something failed\nmore detail"),
            "something failed"
        );
        assert_eq!(humanize_gh_error("   "), "PR action failed");
    }
}
