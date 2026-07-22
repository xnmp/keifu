//! Per-file commit history: list commits that touched a path, open each in the
//! file-diff viewer.

use super::*;
use crate::git::file_history;

/// Cap on how many history entries to load for a single file.
const FILE_HISTORY_LIMIT: usize = 200;

impl App {
    /// Open the per-file history picker for the selected file (files pane).
    pub(crate) fn open_file_history(&mut self) {
        self.sync_file_list_cache();
        let Some(path) = self.selected_file().map(|f| f.path.clone()) else {
            self.toast(crate::toast::ToastKind::Info, "No file selected");
            return;
        };
        let path_str = path.to_string_lossy().to_string();

        let oids = match file_history(&self.repo_path, &path_str, FILE_HISTORY_LIMIT) {
            Ok(oids) => oids,
            Err(e) => {
                self.toast(crate::toast::ToastKind::Error, format!("File history failed: {e}"));
                return;
            }
        };

        let entries: Vec<FileHistoryEntry> = oids
            .into_iter()
            .filter_map(|oid| self.file_history_entry(oid))
            .collect();

        if entries.is_empty() {
            self.toast(crate::toast::ToastKind::Info, format!("No history for {}", path.display()));
            return;
        }

        self.mode = AppMode::FileHistory {
            path,
            entries,
            selected: 0,
        };
    }

    /// Build a display entry for `oid`, reusing an already-loaded `CommitInfo`
    /// when available and falling back to a fresh lookup otherwise.
    fn file_history_entry(&self, oid: Oid) -> Option<FileHistoryEntry> {
        if let Some(commit) = self.commits.iter().find(|c| c.oid == oid) {
            return Some(FileHistoryEntry {
                oid,
                short_id: commit.short_id.clone(),
                date: commit.timestamp.format("%Y-%m-%d").to_string(),
                subject: commit.message.clone(),
            });
        }

        let commit = self.repo.repo().find_commit(oid).ok()?;
        let info = crate::git::CommitInfo::from_git2_commit(&commit);
        Some(FileHistoryEntry {
            oid,
            short_id: info.short_id,
            date: info.timestamp.format("%Y-%m-%d").to_string(),
            subject: info.message,
        })
    }

    pub(crate) fn handle_file_history_action(&mut self, action: Action) -> Result<()> {
        let AppMode::FileHistory {
            path,
            entries,
            selected,
        } = &self.mode
        else {
            return Ok(());
        };

        match action {
            Action::MoveUp => {
                let new = cyclic_prev(*selected, entries.len());
                if let AppMode::FileHistory { selected, .. } = &mut self.mode {
                    *selected = new;
                }
            }
            Action::MoveDown => {
                let new = cyclic_next(*selected, entries.len());
                if let AppMode::FileHistory { selected, .. } = &mut self.mode {
                    *selected = new;
                }
            }
            Action::MenuSelect | Action::Confirm => {
                let Some(entry) = entries.get(*selected) else {
                    return Ok(());
                };
                let oid = entry.oid;
                let path = path.clone();
                self.open_file_diff_at_commit(oid, &path)?;
            }
            Action::Cancel | Action::Quit => {
                self.mode = AppMode::Normal;
                self.focus_files_pane();
            }
            _ => {}
        }
        Ok(())
    }

    /// Open the file-diff viewer for `path` as it changed in commit `oid`.
    fn open_file_diff_at_commit(
        &mut self,
        oid: Oid,
        path: &std::path::Path,
    ) -> Result<()> {
        let file = FileDiffInfo {
            path: path.to_path_buf(),
            kind: FileChangeKind::Modified,
            is_binary: false,
            insertions: 0,
            deletions: 0,
            stage_status: None,
        };
        if let Err(e) = self.enter_file_diff(DiffTarget::Commit(oid), 0, vec![file], path) {
            self.toast(crate::toast::ToastKind::Error, format!("Cannot open diff: {e}"));
            self.mode = AppMode::Normal;
        }
        Ok(())
    }
}
