//! PR conversation popup: open, scroll, close.

use super::*;

impl App {
    /// Open the conversation popup for the selected commit's open PR (any PR,
    /// not just ones with CI). Uses the session cache or fetches in background.
    pub(crate) fn open_pr_thread(&mut self) {
        let pr = self.selected_commit_node().and_then(|node| {
            crate::ui::graph_view::pr_for_branch_labels(
                &node.branch_names,
                &self.remotes,
                &self.open_prs,
            )
            .cloned()
        });
        let Some(pr) = pr else {
            self.set_message("No open PR for this commit");
            return;
        };
        let state = if let Some(thread) = self.thread_fetch.cached(pr.number) {
            ThreadViewState::Loaded(thread.clone())
        } else {
            self.thread_fetch.start(&self.repo_path, pr.number);
            ThreadViewState::Loading
        };
        self.pr_thread = Some(PrThreadView {
            pr_number: pr.number,
            pr_url: pr.url,
            state,
            scroll: 0,
            max_scroll: 0,
        });
        self.mode = AppMode::PrThread;
    }

    pub(crate) fn handle_pr_thread_action(&mut self, action: Action) {
        match action {
            Action::Cancel => {
                self.pr_thread = None;
                self.mode = AppMode::Normal;
            }
            Action::MoveUp => self.pr_thread_scroll(-1),
            Action::MoveDown => self.pr_thread_scroll(1),
            Action::PageUp => self.pr_thread_scroll(-15),
            Action::PageDown => self.pr_thread_scroll(15),
            Action::GoToTop => self.pr_thread_scroll(i32::MIN),
            Action::GoToBottom => self.pr_thread_scroll(i32::MAX),
            Action::OpenPr => self.pr_thread_open_url(),
            Action::OpenReviewPicker => {
                if let Some(number) = self.pr_thread.as_ref().map(|v| v.pr_number) {
                    self.open_review_picker(number);
                }
            }
            _ => {}
        }
    }

    /// Scroll the conversation by `delta` wrapped rows, clamped to the max the
    /// last render computed. `i32::MIN`/`MAX` jump to top/bottom.
    fn pr_thread_scroll(&mut self, delta: i32) {
        if let Some(v) = &mut self.pr_thread {
            v.scroll = match delta {
                i32::MIN => 0,
                i32::MAX => v.max_scroll,
                d => (v.scroll as i64 + d as i64).clamp(0, v.max_scroll as i64) as usize,
            };
        }
    }

    fn pr_thread_open_url(&mut self) {
        let Some(v) = &self.pr_thread else {
            return;
        };
        let url = v.pr_url.clone();
        if let Err(e) = open_url(&url) {
            self.show_error(format!("Could not open: {e}"));
        } else {
            self.set_message("Opening PR in browser");
        }
    }

    /// Poll the background conversation fetch, filling the open popup. Returns
    /// true when something changed (triggering a re-render).
    pub fn update_thread_status(&mut self) -> bool {
        let Some((pr_number, result)) = self.thread_fetch.poll() else {
            return false;
        };
        if let Some(v) = &mut self.pr_thread {
            if v.pr_number == pr_number {
                v.state = match result {
                    Ok(thread) => ThreadViewState::Loaded(thread),
                    Err(e) => ThreadViewState::Error(e),
                };
                v.scroll = 0;
            }
        }
        true
    }
}
