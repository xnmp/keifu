//! Full-screen file diff viewer.

use super::*;

/// Snapshot of the FileDiff viewer state needed to run a hunk operation.
struct HunkOpTarget {
    path: std::path::PathBuf,
    is_binary: bool,
    has_hunks: bool,
    /// Index of the hunk under the cursor, into both `content.hunks` and the
    /// git2 patch built with the same diff options.
    hunk_index: usize,
    scroll_offset: usize,
    is_untracked: bool,
}

impl App {
    pub(crate) fn handle_file_diff_action(&mut self, action: Action) -> Result<()> {
        let AppMode::FileDiff {
            total_lines,
            max_line_width,
            file_index,
            ..
        } = &self.mode
        else {
            return Ok(());
        };
        let total_lines = *total_lines;
        let max_line_width = *max_line_width;
        let file_index = *file_index;
        let viewport = self.diff_viewport_height as usize;
        let half_page = (viewport / 2).max(1);
        let max_scroll = total_lines.saturating_sub(viewport);
        let h_viewport = self.diff_viewport_width as usize;
        let max_horizontal = max_line_width.saturating_sub(h_viewport);
        const H_SCROLL_STEP: usize = 4;

        match action {
            Action::ScrollDown => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = (*scroll_offset + 1).min(max_scroll);
                }
            }
            Action::ScrollUp => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = scroll_offset.saturating_sub(1);
                }
            }
            Action::ScrollPageDown => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = (*scroll_offset + half_page).min(max_scroll);
                }
            }
            Action::ScrollPageUp => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = scroll_offset.saturating_sub(half_page);
                }
            }
            Action::PageDown => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = (*scroll_offset + viewport).min(max_scroll);
                }
            }
            Action::PageUp => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = scroll_offset.saturating_sub(viewport);
                }
            }
            Action::ScrollToTop => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = 0;
                }
            }
            Action::ScrollToBottom => {
                if let AppMode::FileDiff { scroll_offset, .. } = &mut self.mode {
                    *scroll_offset = max_scroll;
                }
            }
            Action::ScrollRight => {
                if let AppMode::FileDiff {
                    horizontal_offset, ..
                } = &mut self.mode
                {
                    *horizontal_offset = (*horizontal_offset + H_SCROLL_STEP).min(max_horizontal);
                }
            }
            Action::ScrollLeft => {
                if let AppMode::FileDiff {
                    horizontal_offset, ..
                } = &mut self.mode
                {
                    *horizontal_offset = horizontal_offset.saturating_sub(H_SCROLL_STEP);
                }
            }
            Action::ScrollToLineStart => {
                if let AppMode::FileDiff {
                    horizontal_offset, ..
                } = &mut self.mode
                {
                    *horizontal_offset = 0;
                }
            }
            Action::NextHunk => {
                if let AppMode::FileDiff {
                    scroll_offset,
                    hunk_positions,
                    ..
                } = &mut self.mode
                {
                    // Find next hunk after current scroll position
                    if let Some(&pos) = hunk_positions.iter().find(|&&p| p > *scroll_offset) {
                        *scroll_offset = pos.min(max_scroll);
                    }
                }
            }
            Action::PrevHunk => {
                if let AppMode::FileDiff {
                    scroll_offset,
                    hunk_positions,
                    ..
                } = &mut self.mode
                {
                    // Find previous hunk before current scroll position
                    if let Some(&pos) = hunk_positions.iter().rev().find(|&&p| p < *scroll_offset) {
                        *scroll_offset = pos.min(max_scroll);
                    }
                }
            }
            Action::NextFile => {
                let snapshot = if let AppMode::FileDiff {
                    file_list,
                    diff_target,
                    ..
                } = &self.mode
                {
                    Some((file_list.clone(), *diff_target))
                } else {
                    None
                };
                let Some((file_list_snapshot, target)) = snapshot else {
                    return Ok(());
                };
                if !file_list_snapshot.is_empty() {
                    let new_index = (file_index + 1) % file_list_snapshot.len();
                    let path = file_list_snapshot[new_index].path.clone();
                    if let Err(e) = self.enter_file_diff(target, new_index, file_list_snapshot, &path) {
                        self.set_message(format!("Cannot open diff: {e}"));
                    }
                }
            }
            Action::PrevFile => {
                let snapshot = if let AppMode::FileDiff {
                    file_list,
                    diff_target,
                    ..
                } = &self.mode
                {
                    Some((file_list.clone(), *diff_target))
                } else {
                    None
                };
                let Some((file_list_snapshot, target)) = snapshot else {
                    return Ok(());
                };
                if !file_list_snapshot.is_empty() {
                    let new_index = if file_index == 0 {
                        file_list_snapshot.len() - 1
                    } else {
                        file_index - 1
                    };
                    let path = file_list_snapshot[new_index].path.clone();
                    if let Err(e) = self.enter_file_diff(target, new_index, file_list_snapshot, &path) {
                        self.set_message(format!("Cannot open diff: {e}"));
                    }
                }
            }
            Action::StageHunk => self.stage_hunk_under_cursor()?,
            Action::UnstageHunk => self.unstage_hunk_under_cursor()?,
            Action::DiscardHunk => self.prompt_discard_hunk_under_cursor()?,
            Action::Cancel | Action::Quit => {
                // Return to normal with files panel focused, preserving file index
                let flat_index = if let AppMode::FileDiff { file_index, .. } = &self.mode {
                    Some(*file_index)
                } else {
                    None
                };
                if let Some(fi) = flat_index {
                    self.sync_file_list_cache();
                    self.select_file_at(self.flat_index_to_display_index(fi));
                }
                self.focused_panel = FocusedPanel::Files;
                self.return_to_normal();
            }
            _ => {}
        }
        Ok(())
    }

    /// Snapshot the info needed to run a hunk operation against the FileDiff
    /// viewer. The hunk under the cursor is the last hunk whose header sits at
    /// or above the current scroll position (VSCode stages the hunk the cursor
    /// is in). Returns None when not in FileDiff mode.
    fn hunk_op_target(&self) -> Option<HunkOpTarget> {
        let AppMode::FileDiff {
            content,
            hunk_positions,
            scroll_offset,
            file_index,
            file_list,
            ..
        } = &self.mode
        else {
            return None;
        };
        let hunk_index = hunk_positions
            .iter()
            .rposition(|&pos| pos <= *scroll_offset)
            .unwrap_or(0);
        let is_untracked = file_list.get(*file_index).and_then(|f| f.stage_status)
            == Some(StageStatus::Untracked);
        Some(HunkOpTarget {
            path: content.path.clone(),
            is_binary: content.is_binary,
            has_hunks: !content.hunks.is_empty(),
            hunk_index,
            scroll_offset: *scroll_offset,
            is_untracked,
        })
    }

    /// Validate the common preconditions for a hunk operation. Emits a status
    /// message and returns None when the operation is unavailable.
    fn resolve_hunk_op(&mut self) -> Option<HunkOpTarget> {
        let target = self.hunk_op_target()?;
        if self.current_diff_target() != Some(DiffTarget::Uncommitted) {
            self.set_message("Hunk staging is only available for uncommitted changes");
            return None;
        }
        if target.is_binary {
            self.set_message("Cannot stage a hunk of a binary file");
            return None;
        }
        if !target.has_hunks {
            self.set_message("No hunk under the cursor");
            return None;
        }
        Some(target)
    }

    fn stage_hunk_under_cursor(&mut self) -> Result<()> {
        let Some(target) = self.resolve_hunk_op() else {
            return Ok(());
        };
        // Untracked files have no HEAD/index entry: the combined diff shows one
        // all-additions hunk that IS the whole file, so "stage the hunk" is
        // exactly `git add <file>` — no /dev/null new-file patch needed.
        if target.is_untracked {
            let path_str = target.path.to_string_lossy().to_string();
            stage_file(&self.repo_path, &path_str)?;
            self.set_message(format!("Staged {}", target.path.display()));
            return self.reload_file_diff_for_path(&target.path, target.scroll_offset);
        }
        let Some(hunk) =
            extract_hunk_from_working_tree(self.repo.repo(), &target.path, target.hunk_index)?
        else {
            self.set_message("Hunk is no longer present");
            return Ok(());
        };
        let path_str = target.path.to_string_lossy();
        let patch = render_hunk_patch(&path_str, &hunk);
        match apply_patch_cached(&self.repo_path, &patch) {
            Ok(()) => {
                self.set_message("Staged hunk");
                self.reload_file_diff_for_path(&target.path, target.scroll_offset)?;
            }
            Err(e) => self.set_message(format!("Stage hunk failed: {e}")),
        }
        Ok(())
    }

    fn unstage_hunk_under_cursor(&mut self) -> Result<()> {
        let Some(target) = self.resolve_hunk_op() else {
            return Ok(());
        };
        if target.is_untracked {
            self.set_message("Untracked file has nothing staged to unstage");
            return Ok(());
        }
        let Some(hunk) =
            extract_hunk_from_working_tree(self.repo.repo(), &target.path, target.hunk_index)?
        else {
            self.set_message("Hunk is no longer present");
            return Ok(());
        };
        let path_str = target.path.to_string_lossy();
        let patch = render_hunk_patch(&path_str, &hunk);
        match apply_patch_cached_reverse(&self.repo_path, &patch) {
            Ok(()) => {
                self.set_message("Unstaged hunk");
                self.reload_file_diff_for_path(&target.path, target.scroll_offset)?;
            }
            Err(e) => self.set_message(format!("Unstage hunk failed: {e}")),
        }
        Ok(())
    }

    fn prompt_discard_hunk_under_cursor(&mut self) -> Result<()> {
        let Some(target) = self.resolve_hunk_op() else {
            return Ok(());
        };
        if target.is_untracked {
            self.set_message("Untracked file — use the files pane (Delete) to remove it");
            return Ok(());
        }
        let Some(hunk) =
            extract_hunk_from_working_tree(self.repo.repo(), &target.path, target.hunk_index)?
        else {
            self.set_message("Hunk is no longer present");
            return Ok(());
        };
        let path_str = target.path.to_string_lossy();
        let patch = render_hunk_patch(&path_str, &hunk);
        // Destructive: route through the shared Confirm mode. The patch and the
        // scroll position are captured now so the viewer can be reopened at the
        // same spot after confirmation.
        self.mode = AppMode::Confirm {
            message: "Discard this hunk? Working-tree changes will be lost.".to_string(),
            action: ConfirmAction::DiscardHunk {
                patch,
                file_path: target.path.clone(),
                scroll_offset: target.scroll_offset,
            },
        };
        Ok(())
    }

    /// Reopen the FileDiff viewer for `path` after a hunk operation, refreshing
    /// app diff state and restoring the scroll position where possible. Falls
    /// back to the first changed file when `path` no longer differs, or exits to
    /// Normal (files panel focused) when nothing remains.
    pub(crate) fn reload_file_diff_for_path(
        &mut self,
        path: &std::path::Path,
        scroll_offset: usize,
    ) -> Result<()> {
        self.refresh_after_file_op()?;
        self.reopen_file_diff_for_path(path, scroll_offset)
    }

    /// Reopen the FileDiff viewer for `path` from the *current* diff cache
    /// (no git refresh), restoring the scroll position. Used when returning to
    /// the viewer after dismissing a modal launched from it.
    pub(crate) fn reopen_file_diff_for_path(
        &mut self,
        path: &std::path::Path,
        scroll_offset: usize,
    ) -> Result<()> {
        let target = self
            .active_file_diff_target()
            .or_else(|| self.current_diff_target())
            .unwrap_or(DiffTarget::Uncommitted);
        let new_file_list = self.files_pane.display_file_list();
        if new_file_list.is_empty() {
            self.mode = AppMode::Normal;
            self.focused_panel = FocusedPanel::Files;
            self.set_message("No changes remaining");
            return Ok(());
        }
        let new_index = new_file_list
            .iter()
            .position(|f| f.path == path)
            .unwrap_or(0);
        let new_path = new_file_list[new_index].path.clone();
        self.enter_file_diff(target, new_index, new_file_list, &new_path)?;
        let viewport = self.diff_viewport_height as usize;
        if let AppMode::FileDiff {
            scroll_offset: so,
            total_lines,
            ..
        } = &mut self.mode
        {
            *so = scroll_offset.min(total_lines.saturating_sub(viewport));
        }
        Ok(())
    }

    pub(crate) fn enter_file_diff(
        &mut self,
        target: DiffTarget,
        file_index: usize,
        file_list: Vec<FileDiffInfo>,
        file_path: &std::path::Path,
    ) -> Result<()> {
        use crate::ui::file_diff_view::build_highlighted_lines;

        // NOTE: Runs synchronously on the UI thread. For very large diffs (e.g. generated
        // files, large refactors) this may briefly block input. If this becomes a problem,
        // consider moving to a background task with a loading state, similar to commit diff summaries.
        let content = self.load_file_diff_content(file_path, target)?;
        let ui_theme = self.theme();
        let (rendered_lines, hunk_positions) = build_highlighted_lines(&content, &ui_theme);
        let total_lines = rendered_lines.len();
        let max_line_width = rendered_lines.iter().map(|l| l.width()).max().unwrap_or(0);

        self.mode = AppMode::FileDiff {
            diff_target: target,
            file_index,
            file_list,
            content,
            rendered_lines,
            hunk_positions,
            scroll_offset: 0,
            horizontal_offset: 0,
            max_line_width,
            total_lines,
        };
        Ok(())
    }

    /// The diff target the FileDiff viewer is currently pinned to, if any.
    fn active_file_diff_target(&self) -> Option<DiffTarget> {
        if let AppMode::FileDiff { diff_target, .. } = &self.mode {
            Some(*diff_target)
        } else {
            None
        }
    }

    fn load_file_diff_content(
        &self,
        file_path: &std::path::Path,
        target: DiffTarget,
    ) -> Result<FileDiffContent> {
        let result = match target {
            DiffTarget::Commit(oid) => {
                FileDiffContent::from_commit(self.repo.repo(), oid, file_path)
            }
            DiffTarget::Range(old, new) => {
                FileDiffContent::from_range(self.repo.repo(), old, new, file_path)
            }
            DiffTarget::Uncommitted => {
                FileDiffContent::from_working_tree(self.repo.repo(), file_path)
            }
        };
        // If diff fails (e.g. added file with no parent entry), return empty content
        result.or_else(|_| {
            Ok(FileDiffContent {
                path: file_path.to_path_buf(),
                kind: FileChangeKind::Added,
                is_binary: false,
                hunks: Vec::new(),
                total_additions: 0,
                total_deletions: 0,
            })
        })
    }

    /// Extract a file blob from a commit to a temp file, preserving the extension.
    pub(crate) fn extract_blob_to_temp(
        &self,
        commit_oid: git2::Oid,
        file_path: &std::path::Path,
    ) -> Result<std::path::PathBuf> {
        let commit = self.repo.repo().find_commit(commit_oid)?;
        let tree = commit.tree()?;
        let entry = tree.get_path(file_path)?;
        let blob = self.repo.repo().find_blob(entry.id())?;

        let ext = file_path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let stem = file_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());

        let tmp_dir = std::env::temp_dir().join("keifu");
        std::fs::create_dir_all(&tmp_dir)?;
        let tmp_path = tmp_dir.join(format!("{}{}", stem, ext));
        std::fs::write(&tmp_path, blob.content())?;
        Ok(tmp_path)
    }

    /// Open a file with the default system application.
    pub(crate) fn open_with_default(&mut self, full_path: &std::path::Path, display_path: &std::path::Path) {
        use std::process::{Command, Stdio};
        let result = if cfg!(target_os = "macos") {
            Command::new("open")
                .arg(full_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .stdin(Stdio::null())
                .spawn()
        } else if cfg!(target_os = "windows") {
            Command::new("cmd")
                .args(["/C", "start", ""])
                .arg(full_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .stdin(Stdio::null())
                .spawn()
        } else {
            Command::new("xdg-open")
                .arg(full_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .stdin(Stdio::null())
                .spawn()
        };
        match result {
            Ok(_) => self.set_message(format!("Opening {}", display_path.display())),
            Err(e) => self.set_message(format!("Cannot open file: {e}")),
        }
    }

    /// Sync the file_list held by FileDiff with the latest
    /// uncommitted diff cache.  Called right after `uncommitted_diff_cache` is
    /// updated so that navigation and display stay consistent.
    pub(crate) fn sync_file_list_with_uncommitted_diff(&mut self) {
        // Only touch the viewer when it is actually showing the working tree —
        // a diff opened from file history or a comparison must not be rewritten
        // when a background uncommitted diff lands.
        if self.active_file_diff_target() != Some(DiffTarget::Uncommitted) {
            return;
        }

        let new_files = match self.diff_cache.cached_diff(Some(DiffTarget::Uncommitted)) {
            Some(diff) => diff.files.clone(),
            None => return,
        };

        if new_files.is_empty() {
            if matches!(self.mode, AppMode::FileDiff { .. }) {
                self.mode = AppMode::Normal;
                self.set_message("No changed files in this diff");
            }
            return;
        }

        if let AppMode::FileDiff {
            file_index,
            file_list,
            ..
        } = &mut self.mode
        {
            let current_path = file_list.get(*file_index).map(|f| f.path.clone());
            *file_list = new_files;
            if let Some(path) = current_path {
                if let Some(new_idx) = file_list.iter().position(|f| f.path == path) {
                    *file_index = new_idx;
                } else if *file_index >= file_list.len() {
                    *file_index = file_list.len() - 1;
                }
            }
        }
    }

    fn return_to_normal(&mut self) {
        self.mode = AppMode::Normal;
        if self.pending_refresh {
            self.pending_refresh = false;
            if let Err(e) = self.refresh(true) {
                self.set_message(format!("Refresh failed: {e}"));
            }
        }
    }
}
