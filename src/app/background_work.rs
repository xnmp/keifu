//! UI-owned delivery of bounded background computations. Jobs own immutable
//! inputs; only this module applies results to the current application state.
use super::*;
use crate::background::LatestTask;
use std::collections::HashMap;

pub(crate) struct PreparedFileDiff {
    pub target: DiffTarget,
    pub content: FileDiffContent,
    pub rows: Vec<crate::ui::file_diff_view::DiffRow>,
    pub hunks: Vec<usize>,
    pub lines: Vec<ratatui::text::Line<'static>>,
    pub display_hunks: Vec<usize>,
    pub max_width: usize,
    pub width: usize,
    pub wrap: bool,
}

impl PreparedFileDiff {
    pub(crate) fn layout(
        mut self,
        wrap: bool,
        width: usize,
        cancel: &crate::background::Cancellation,
    ) -> Result<Self, String> {
        (self.lines, self.display_hunks) = crate::ui::file_diff_view::layout_diff_rows_while(
            &self.rows,
            &self.hunks,
            wrap,
            width,
            || !cancel.is_cancelled(),
        )
        .ok_or_else(|| "Diff request superseded".to_string())?;
        self.max_width = self.lines.iter().map(|l| l.width()).max().unwrap_or(0);
        self.width = width;
        self.wrap = wrap;
        Ok(self)
    }
}

#[derive(Default)]
pub struct BackgroundWork {
    /// False for embedded callers without an event loop, true in the TUI.
    pub threaded: bool,
    pub(crate) file_diff: LatestTask<PreparedFileDiff>,
    pub(crate) paths: LatestTask<HashMap<Oid, Vec<String>>>,
    pub(crate) path_key: Vec<Oid>,
    pub(crate) authors: LatestTask<HashMap<String, String>>,
}

impl App {
    /// Poll from the event loop, never from a renderer. Stale file results may
    /// populate no UI: the mode, target and path must still match the request.
    pub fn poll_background_work(&mut self) -> bool {
        let mut changed = false;
        if let Some(result) = self.background.file_diff.take() {
            changed = true;
            match result {
                Ok(prepared) => self.install_file_diff(prepared),
                Err(message) => {
                    if let AppMode::FileDiff { rendered_lines, .. } = &mut self.mode {
                        *rendered_lines = vec![ratatui::text::Line::from(message.clone())];
                        self.toast(crate::toast::ToastKind::Error, message);
                    }
                }
            }
        }
        if let Some(result) = self.background.authors.take() {
            changed = true;
            match result {
                Ok(authors) => {
                    if let AppMode::BranchFilter {
                        filter,
                        selected,
                        all_branches,
                    } = &mut self.mode
                    {
                        if filter.starts_with('@') {
                            let old_branch = all_branches
                                .iter()
                                .filter(|b| {
                                    crate::ui::branch_filter::matches_branch_filter(
                                        b,
                                        self.branch_authors.get(*b).map_or("", String::as_str),
                                        filter,
                                    )
                                })
                                .nth(*selected);
                            let matching: Vec<_> = all_branches
                                .iter()
                                .filter(|b| {
                                    crate::ui::branch_filter::matches_branch_filter(
                                        b,
                                        authors.get(*b).map_or("", String::as_str),
                                        filter,
                                    )
                                })
                                .collect();
                            *selected = old_branch
                                .and_then(|old| matching.iter().position(|b| *b == old))
                                .unwrap_or_else(|| {
                                    (*selected).min(matching.len().saturating_sub(1))
                                });
                        }
                    }
                    self.branch_authors = authors;
                }
                Err(message) => {
                    self.branch_authors_key.clear();
                    self.toast(crate::toast::ToastKind::Error, message);
                }
            }
        }
        if let Some(result) = self.background.paths.take() {
            changed = true;
            match result {
                Ok(paths) => {
                    self.commit_changed_paths.extend(paths);
                    self.graph_cache_key = None;
                    if let Err(error) = self.rebuild_and_restore(false) {
                        self.report_refresh_error(error);
                    }
                }
                Err(message) => {
                    self.background.path_key.clear();
                    self.toast(crate::toast::ToastKind::Error, message);
                }
            }
        }
        changed
    }

    pub fn file_diff_is_loading(&self) -> bool {
        self.background.file_diff.is_pending()
    }

    pub fn commit_paths_are_loading(&self) -> bool {
        self.background.paths.is_pending() && self.commit_filter_needs_paths()
    }

    pub(crate) fn install_file_diff(&mut self, prepared: PreparedFileDiff) {
        let AppMode::FileDiff {
            diff_target,
            content,
            ..
        } = &self.mode
        else {
            return;
        };
        if *diff_target != prepared.target || content.path != prepared.content.path {
            return;
        }
        let width = self.diff_viewport_width as usize;
        let wrap = self.diff_word_wrap;
        if prepared.wrap != wrap || (wrap && prepared.width != width) {
            // Resize/toggle while preparing: reflow owned rows on the same
            // worker; never do a whole-file layout on the delivery frame.
            self.background
                .file_diff
                .submit_cancellable(self.background.threaded, move |cancel| {
                    prepared.layout(wrap, width, &cancel)
                });
            return;
        }
        self.diff_source = Some(crate::ui::file_diff_view::DiffSource {
            rows: prepared.rows,
            hunk_positions: prepared.hunks,
            layout_wrap: wrap,
            layout_width: width,
        });
        if let AppMode::FileDiff {
            content,
            rendered_lines,
            hunk_positions,
            max_line_width,
            total_lines,
            scroll_offset,
            horizontal_offset,
            ..
        } = &mut self.mode
        {
            *content = prepared.content;
            *total_lines = prepared.lines.len();
            *rendered_lines = prepared.lines;
            *hunk_positions = prepared.display_hunks;
            *max_line_width = prepared.max_width;
            if wrap {
                *horizontal_offset = 0;
            }
            *scroll_offset = (*scroll_offset)
                .min(total_lines.saturating_sub(self.diff_viewport_height as usize));
        }
    }

    pub(crate) fn request_diff_relayout(&mut self, wrap: bool, width: usize) {
        let Some(source) = self.diff_source.take() else {
            return;
        };
        let AppMode::FileDiff {
            diff_target,
            content,
            ..
        } = &mut self.mode
        else {
            return;
        };
        let empty = FileDiffContent {
            path: content.path.clone(),
            kind: content.kind,
            is_binary: content.is_binary,
            hunks: Vec::new(),
            total_additions: content.total_additions,
            total_deletions: content.total_deletions,
        };
        let prepared = PreparedFileDiff {
            target: *diff_target,
            content: std::mem::replace(content, empty),
            rows: source.rows,
            hunks: source.hunk_positions,
            lines: Vec::new(),
            display_hunks: Vec::new(),
            max_width: 0,
            width,
            wrap,
        };
        self.background
            .file_diff
            .submit_cancellable(true, move |cancel| prepared.layout(wrap, width, &cancel));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn author_delivery_reconciles_selection_and_bulk_actions_wait_for_current_data() {
        let mut app = App::test_fixture();
        app.mode = AppMode::BranchFilter {
            filter: "@Alice".into(),
            selected: 1,
            all_branches: vec!["one".into(), "two".into()],
        };
        app.branch_authors = HashMap::from([
            ("one".into(), "Alice".into()),
            ("two".into(), "Alice".into()),
        ]);
        let (release, wait) = mpsc::channel();
        app.background.authors.submit(true, move || {
            wait.recv().unwrap();
            Ok(HashMap::from([("two".into(), "Alice".into())]))
        });
        app.handle_action(Action::SelectNone).unwrap();
        assert!(app.hidden_branches.is_empty());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.background.authors.is_pending() {
            app.poll_background_work();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(
            app.mode,
            AppMode::BranchFilter { selected: 0, .. }
        ));
        app.handle_action(Action::SelectNone).unwrap();
        assert!(app.hidden_branches.contains("two"));
        assert!(!app.hidden_branches.contains("one"));
    }
}
