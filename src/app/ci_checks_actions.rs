//! CI check details popup: open, navigate, drill into a failed check's log.

use super::*;
use crate::checks::{CheckSource, CheckState};
use crate::pr::CiStatus;

impl App {
    /// Open the CI checks popup for the selected commit's PR, if it has one with
    /// a reported CI state. Fetches the check list in the background.
    pub(crate) fn open_ci_checks(&mut self) {
        let pr = self.selected_commit_node().and_then(|node| {
            crate::ui::graph_view::pr_for_branch_labels(
                &node.branch_names,
                &self.remotes,
                &self.open_prs,
            )
            .cloned()
        });
        let Some(pr) = pr else {
            return;
        };
        if pr.ci == CiStatus::None {
            self.toast(crate::toast::ToastKind::Info, "This PR has no CI checks");
            return;
        }
        self.ci_checks = Some(CiChecksView {
            pr_number: pr.number,
            pr_url: pr.url,
            checks: ChecksState::Loading,
            selected: 0,
            log: None,
        });
        self.check_fetch.start_list(&self.repo_path, pr.number);
        self.mode = AppMode::CiChecks;
    }

    pub(crate) fn handle_ci_checks_action(&mut self, action: Action) {
        let in_log = self.ci_checks.as_ref().is_some_and(|v| v.log.is_some());
        match action {
            // Esc/q closes the log detail first, then the whole popup.
            Action::Cancel => {
                if in_log {
                    if let Some(v) = &mut self.ci_checks {
                        v.log = None;
                    }
                } else {
                    self.close_ci_checks();
                }
            }
            // Up/Down move the list, or scroll the log by a line.
            Action::MoveUp => {
                if in_log {
                    self.ci_checks_scroll(-1);
                } else {
                    self.ci_checks_move(-1);
                }
            }
            Action::MoveDown => {
                if in_log {
                    self.ci_checks_scroll(1);
                } else {
                    self.ci_checks_move(1);
                }
            }
            Action::PageUp => self.ci_checks_scroll(-20),
            Action::PageDown => self.ci_checks_scroll(20),
            Action::GoToTop => self.ci_checks_scroll(i32::MIN),
            Action::GoToBottom => self.ci_checks_scroll(i32::MAX),
            Action::MenuSelect if !in_log => self.ci_checks_select(),
            Action::OpenPr => self.ci_checks_open_url(),
            _ => {}
        }
    }

    fn close_ci_checks(&mut self) {
        self.ci_checks = None;
        self.mode = AppMode::Normal;
    }

    fn ci_checks_move(&mut self, delta: i32) {
        let Some(v) = &mut self.ci_checks else {
            return;
        };
        let ChecksState::Loaded(checks) = &v.checks else {
            return;
        };
        if checks.is_empty() {
            return;
        }
        let last = checks.len() - 1;
        v.selected = (v.selected as i32 + delta).clamp(0, last as i32) as usize;
    }

    fn ci_checks_scroll(&mut self, delta: i32) {
        let Some(log) = self.ci_checks.as_mut().and_then(|v| v.log.as_mut()) else {
            return;
        };
        let max = match &log.content {
            LogContent::Lines(lines) => lines.len().saturating_sub(1),
            _ => 0,
        };
        log.scroll = match delta {
            i32::MIN => 0,
            i32::MAX => max,
            d => (log.scroll as i64 + d as i64).clamp(0, max as i64) as usize,
        };
    }

    /// Enter on a check: a failed Actions run opens its log (fetching lazily);
    /// an external check shows its URL; anything else is a no-op with a hint.
    fn ci_checks_select(&mut self) {
        let (check, _) = match &self.ci_checks {
            Some(v) => match &v.checks {
                ChecksState::Loaded(checks) => (checks.get(v.selected).cloned(), ()),
                _ => return,
            },
            None => return,
        };
        let Some(check) = check else {
            return;
        };
        match &check.source {
            CheckSource::Run { run_id } if check.state == CheckState::Fail => {
                let run_id = *run_id;
                let content = if let Some(lines) = self.check_fetch.cached_log(run_id) {
                    LogContent::Lines(lines.clone())
                } else {
                    self.check_fetch.start_log(&self.repo_path, run_id);
                    LogContent::Loading
                };
                if let Some(v) = &mut self.ci_checks {
                    v.log = Some(LogView {
                        title: check.name,
                        run_id: Some(run_id),
                        content,
                        scroll: 0,
                    });
                }
            }
            CheckSource::External { url } => {
                let url = url.clone();
                if let Some(v) = &mut self.ci_checks {
                    v.log = Some(LogView {
                        title: check.name,
                        run_id: None,
                        content: LogContent::External(url),
                        scroll: 0,
                    });
                }
            }
            _ => self.toast(crate::toast::ToastKind::Info, "No failure log for this check (press o to open)"),
        }
    }

    /// `o` opens a URL in the browser: the current external detail's URL, else
    /// the selected check's link, else the PR itself.
    fn ci_checks_open_url(&mut self) {
        let url = match &self.ci_checks {
            Some(v) => {
                if let Some(LogView {
                    content: LogContent::External(url),
                    ..
                }) = &v.log
                {
                    url.clone()
                } else if let ChecksState::Loaded(checks) = &v.checks {
                    checks
                        .get(v.selected)
                        .map(|c| c.url.clone())
                        .filter(|u| !u.is_empty())
                        .unwrap_or_else(|| v.pr_url.clone())
                } else {
                    v.pr_url.clone()
                }
            }
            None => return,
        };
        if let Err(e) = open_url(&url) {
            self.show_error(format!("Could not open: {e}"));
        } else {
            self.toast(crate::toast::ToastKind::Success, "Opening in browser");
        }
    }

    /// Poll the background check-list and log fetches, filling the open popup.
    /// Returns true when something changed (triggering a re-render).
    pub fn update_check_status(&mut self) -> bool {
        let mut changed = false;

        if let Some(result) = self.check_fetch.poll_list() {
            if let Some(v) = &mut self.ci_checks {
                if matches!(v.checks, ChecksState::Loading) {
                    v.checks = match result {
                        Ok(checks) => ChecksState::Loaded(checks),
                        Err(e) => ChecksState::Error(e),
                    };
                    v.selected = 0;
                }
            }
            changed = true;
        }

        if let Some((run_id, result)) = self.check_fetch.poll_log() {
            if let Some(log) = self.ci_checks.as_mut().and_then(|v| v.log.as_mut()) {
                if log.run_id == Some(run_id) && matches!(log.content, LogContent::Loading) {
                    log.content = match result {
                        Ok(lines) if lines.is_empty() => {
                            LogContent::Error("No failure output captured".to_string())
                        }
                        Ok(lines) => LogContent::Lines(lines),
                        Err(e) => LogContent::Error(e),
                    };
                }
            }
            changed = true;
        }

        changed
    }
}
