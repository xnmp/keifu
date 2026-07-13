//! Full-screen file diff viewer.

use super::*;

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
                let file_list_snapshot = if let AppMode::FileDiff { file_list, .. } = &self.mode {
                    file_list.clone()
                } else {
                    return Ok(());
                };
                if !file_list_snapshot.is_empty() {
                    let new_index = (file_index + 1) % file_list_snapshot.len();
                    let path = file_list_snapshot[new_index].path.clone();
                    if let Err(e) = self.enter_file_diff(new_index, file_list_snapshot, &path) {
                        self.set_message(format!("Cannot open diff: {e}"));
                    }
                }
            }
            Action::PrevFile => {
                let file_list_snapshot = if let AppMode::FileDiff { file_list, .. } = &self.mode {
                    file_list.clone()
                } else {
                    return Ok(());
                };
                if !file_list_snapshot.is_empty() {
                    let new_index = if file_index == 0 {
                        file_list_snapshot.len() - 1
                    } else {
                        file_index - 1
                    };
                    let path = file_list_snapshot[new_index].path.clone();
                    if let Err(e) = self.enter_file_diff(new_index, file_list_snapshot, &path) {
                        self.set_message(format!("Cannot open diff: {e}"));
                    }
                }
            }
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

    pub(crate) fn enter_file_diff(
        &mut self,
        file_index: usize,
        file_list: Vec<FileDiffInfo>,
        file_path: &std::path::Path,
    ) -> Result<()> {
        use crate::ui::file_diff_view::build_highlighted_lines;

        // NOTE: Runs synchronously on the UI thread. For very large diffs (e.g. generated
        // files, large refactors) this may briefly block input. If this becomes a problem,
        // consider moving to a background task with a loading state, similar to commit diff summaries.
        let content = self.load_file_diff_content(file_path)?;
        let ui_theme = self.theme();
        let (rendered_lines, hunk_positions) = build_highlighted_lines(&content, &ui_theme);
        let total_lines = rendered_lines.len();
        let max_line_width = rendered_lines.iter().map(|l| l.width()).max().unwrap_or(0);

        self.mode = AppMode::FileDiff {
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

    fn load_file_diff_content(&self, file_path: &std::path::Path) -> Result<FileDiffContent> {
        let result = match self.current_diff_target() {
            Some(DiffTarget::Commit(oid)) => {
                FileDiffContent::from_commit(self.repo.repo(), oid, file_path)
            }
            Some(DiffTarget::Uncommitted) | None => {
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
        if self.current_diff_target() != Some(DiffTarget::Uncommitted) {
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
