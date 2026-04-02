//! Files pane state: selection tracking, display item building, filtering, archive listing.

use crate::git::{
    CommitDiffInfo, FileChangeKind, FileDiffInfo, StageStatus,
};

/// Item in the files pane (header or file entry)
#[derive(Debug, Clone)]
pub enum FilesPaneItem {
    Header(String),
    File(FileDiffInfo),
}

/// Tracks which file is selected in the files pane by (section, path).
/// Resolved to an index from the current display items at point of use,
/// so it never goes stale when items reshuffle.
#[derive(Debug, Clone, Default)]
pub(crate) struct FileSelection {
    pub section: Option<String>,
    pub path: Option<std::path::PathBuf>,
}

impl FileSelection {
    /// Resolve this selection to an index within the given items.
    /// Returns the index of the matching file, or the first file if not found.
    pub(crate) fn resolve(&self, items: &[FilesPaneItem]) -> usize {
        // Try to find exact match (section + path)
        if let Some(ref path) = self.path {
            if let Some(ref section) = self.section {
                let mut current_section: Option<&str> = None;
                for (i, item) in items.iter().enumerate() {
                    match item {
                        FilesPaneItem::Header(t) => current_section = Some(t),
                        FilesPaneItem::File(f) if f.path == *path && current_section == Some(section) => {
                            return i;
                        }
                        _ => {}
                    }
                }
            }
            // Fall back to path match in any section
            for (i, item) in items.iter().enumerate() {
                if matches!(item, FilesPaneItem::File(f) if f.path == *path) {
                    return i;
                }
            }
        }
        // Fall back to first file
        items
            .iter()
            .position(|item| matches!(item, FilesPaneItem::File(_)))
            .unwrap_or(0)
    }

    /// Set selection from an index in the given items.
    pub(crate) fn set_from_index(&mut self, idx: usize, items: &[FilesPaneItem]) {
        if let Some(FilesPaneItem::File(f)) = items.get(idx) {
            self.path = Some(f.path.clone());
            self.section = items[..=idx]
                .iter()
                .rev()
                .find_map(|item| match item {
                    FilesPaneItem::Header(t) => Some(t.clone()),
                    _ => None,
                });
        }
    }
}

/// Find which section header an index falls under.
pub fn section_of(items: &[FilesPaneItem], idx: usize) -> Option<&str> {
    if items.is_empty() {
        return None;
    }
    items[..=idx.min(items.len() - 1)]
        .iter()
        .rev()
        .find_map(|item| match item {
            FilesPaneItem::Header(text) => Some(text.as_str()),
            _ => None,
        })
}

/// State for the files pane subsystem.
#[derive(Default)]
pub struct FilesPaneState {
    pub(crate) file_selection: FileSelection,
    pub file_list_cache: Vec<FileDiffInfo>,
    display_items_cache: Vec<FilesPaneItem>,
    pub files_group_by_folder: bool,
    pub files_filter: String,
    pub files_filter_active: bool,
}

impl FilesPaneState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sync the file list cache and display items from the current diff.
    pub fn sync_file_list_cache(
        &mut self,
        diff: Option<&CommitDiffInfo>,
        is_uncommitted: bool,
        repo_path: &str,
    ) {
        if let Some(diff) = diff {
            if self.file_list_cache.len() != diff.files.len()
                || self.file_list_cache.first().map(|f| &f.path)
                    != diff.files.first().map(|f| &f.path)
            {
                self.file_list_cache = diff.files.clone();
            }
        } else {
            self.file_list_cache.clear();
        }
        self.display_items_cache = self.build_files_pane_items(diff, is_uncommitted, repo_path);
    }

    /// Resolve the current selection to an index in display_items_cache.
    /// Always points at a File item (never a header).
    pub fn file_selected_index(&self) -> usize {
        self.file_selection.resolve(&self.display_items_cache)
    }

    /// Resolve the current selection against an arbitrary items list.
    pub fn file_selected_index_in(&self, items: &[FilesPaneItem]) -> usize {
        self.file_selection.resolve(items)
    }

    /// Get the cached display items.
    pub fn display_items(&self) -> &[FilesPaneItem] {
        &self.display_items_cache
    }

    /// Update the selection to point at the given index in display_items_cache.
    pub fn select_file_at(&mut self, idx: usize) {
        self.file_selection.set_from_index(idx, &self.display_items_cache);
    }

    /// Get the selected display item.
    pub fn selected_display_item(&self) -> Option<&FilesPaneItem> {
        self.display_items_cache.get(self.file_selected_index())
    }

    /// Resolve the selected display item to a single file.
    pub fn selected_file(&self) -> Option<&FileDiffInfo> {
        match self.selected_display_item()? {
            FilesPaneItem::File(f) => Some(f),
            _ => None,
        }
    }

    /// Resolve the selected display item to all affected files.
    pub fn selected_files(&self) -> Vec<&FileDiffInfo> {
        match self.selected_display_item() {
            Some(FilesPaneItem::File(f)) => vec![f],
            _ => vec![],
        }
    }

    /// Convert a display index to a flat file index (for enter_file_diff).
    pub fn display_index_to_flat_index(&self, display_index: usize) -> usize {
        let mut flat_idx = 0;
        for (i, item) in self.display_items_cache.iter().enumerate() {
            if i == display_index {
                return flat_idx;
            }
            if matches!(item, FilesPaneItem::File(_)) {
                flat_idx += 1;
            }
        }
        flat_idx.saturating_sub(1)
    }

    /// Convert a flat file index back to a display index.
    pub fn flat_index_to_display_index(&self, flat_index: usize) -> usize {
        let mut file_count = 0;
        for (i, item) in self.display_items_cache.iter().enumerate() {
            if matches!(item, FilesPaneItem::File(_)) {
                if file_count == flat_index {
                    return i;
                }
                file_count += 1;
            }
        }
        self.display_items_cache.len().saturating_sub(1)
    }

    /// Move file selection by delta, skipping headers. Positive = down, negative = up.
    pub fn move_file_selection(&mut self, delta: i32) {
        let items = &self.display_items_cache;
        if items.is_empty() {
            return;
        }
        let current = self.file_selection.resolve(items);
        let max = items.len() as i32 - 1;
        let mut target = (current as i32 + delta).clamp(0, max) as usize;
        // Skip headers in the direction of movement
        let dir = if delta >= 0 { 1i32 } else { -1 };
        while target < items.len() && matches!(items[target], FilesPaneItem::Header(_)) {
            let next = target as i32 + dir;
            if next < 0 || next > max {
                break;
            }
            target = next as usize;
        }
        self.select_file_at(target);
    }

    /// Get the file list for the files pane (staged then unstaged for uncommitted,
    /// or flat list for committed)
    pub fn build_files_pane_items(
        &self,
        diff: Option<&CommitDiffInfo>,
        is_uncommitted: bool,
        repo_path: &str,
    ) -> Vec<FilesPaneItem> {
        let raw_files: Vec<FileDiffInfo> = if is_uncommitted {
            if let Some(diff) = diff {
                if !diff.staged_files.is_empty() || !diff.unstaged_files.is_empty() {
                    let mut all = diff.staged_files.clone();
                    all.extend(diff.unstaged_files.clone());
                    all
                } else {
                    diff.files.clone()
                }
            } else {
                return Vec::new();
            }
        } else if let Some(diff) = diff {
            diff.files.clone()
        } else {
            return Vec::new();
        };

        // Apply fuzzy filter
        let filtered: Vec<FileDiffInfo> = if self.files_filter.is_empty() {
            raw_files
        } else {
            let query = self.files_filter.to_lowercase();
            raw_files
                .into_iter()
                .filter(|f| {
                    let path = f.path.to_string_lossy().to_lowercase();
                    query.chars().all(|c| path.contains(c))
                })
                .collect()
        };

        // Build items with optional folder grouping and staged/unstaged separation
        let mut items = if self.files_group_by_folder && is_uncommitted {
            Self::build_staged_unstaged_folder_items(&filtered)
        } else if self.files_group_by_folder {
            Self::build_folder_grouped_items(&filtered)
        } else if is_uncommitted {
            Self::build_staged_unstaged_items(&filtered)
        } else {
            filtered.into_iter().map(FilesPaneItem::File).collect()
        };

        // Append archived files section (only for uncommitted changes view)
        if is_uncommitted {
            let archived = Self::list_archived_files(repo_path);
            if !archived.is_empty() {
                items.push(FilesPaneItem::Header("Archived Files".to_string()));
                items.extend(archived.into_iter().map(FilesPaneItem::File));
            }
        }
        items
    }

    /// List files in `.archive/` directory as FileDiffInfo items.
    fn list_archived_files(repo_path: &str) -> Vec<FileDiffInfo> {
        let archive_dir = std::path::Path::new(repo_path).join(".archive");
        if !archive_dir.is_dir() {
            return Vec::new();
        }
        let mut files = Vec::new();
        Self::walk_archive_dir(&archive_dir, &archive_dir, &mut files);
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    }

    fn walk_archive_dir(
        base: &std::path::Path,
        dir: &std::path::Path,
        out: &mut Vec<FileDiffInfo>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::walk_archive_dir(base, &path, out);
            } else if let Ok(rel) = path.strip_prefix(base) {
                out.push(FileDiffInfo {
                    path: rel.to_path_buf(),
                    kind: FileChangeKind::Added,
                    is_binary: false,
                    insertions: 0,
                    deletions: 0,
                    stage_status: None,
                });
            }
        }
    }

    fn build_staged_unstaged_items(files: &[FileDiffInfo]) -> Vec<FilesPaneItem> {
        let staged: Vec<_> = files
            .iter()
            .filter(|f| matches!(f.stage_status, Some(StageStatus::Staged)))
            .cloned()
            .collect();
        let unstaged: Vec<_> = files
            .iter()
            .filter(|f| !matches!(f.stage_status, Some(StageStatus::Staged)))
            .cloned()
            .collect();

        let mut items = Vec::new();
        if !staged.is_empty() {
            items.push(FilesPaneItem::Header("Staged Changes".to_string()));
            items.extend(staged.into_iter().map(FilesPaneItem::File));
        }
        if !unstaged.is_empty() {
            items.push(FilesPaneItem::Header("Unstaged Changes".to_string()));
            items.extend(unstaged.into_iter().map(FilesPaneItem::File));
        }
        items
    }

    fn build_folder_grouped_items(files: &[FileDiffInfo]) -> Vec<FilesPaneItem> {
        Self::folder_group(files)
    }

    /// Staged/unstaged sections with folder grouping within each section.
    fn build_staged_unstaged_folder_items(files: &[FileDiffInfo]) -> Vec<FilesPaneItem> {
        let staged: Vec<_> = files
            .iter()
            .filter(|f| matches!(f.stage_status, Some(StageStatus::Staged)))
            .cloned()
            .collect();
        let unstaged: Vec<_> = files
            .iter()
            .filter(|f| !matches!(f.stage_status, Some(StageStatus::Staged)))
            .cloned()
            .collect();

        let mut items = Vec::new();
        if !staged.is_empty() {
            items.push(FilesPaneItem::Header("Staged Changes".to_string()));
            items.extend(Self::folder_group(&staged));
        }
        if !unstaged.is_empty() {
            items.push(FilesPaneItem::Header("Unstaged Changes".to_string()));
            items.extend(Self::folder_group(&unstaged));
        }
        items
    }

    /// Group files by parent directory into Header + File items.
    fn folder_group(files: &[FileDiffInfo]) -> Vec<FilesPaneItem> {
        use std::collections::BTreeMap;

        let mut folders: BTreeMap<String, Vec<FileDiffInfo>> = BTreeMap::new();
        for file in files {
            let folder = file
                .path
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let key = if folder.is_empty() {
                ".".to_string()
            } else {
                folder
            };
            folders.entry(key).or_default().push(file.clone());
        }

        let mut items = Vec::new();
        for (folder, folder_files) in &folders {
            // Skip header for root-level files ("./")
            if folder != "." {
                items.push(FilesPaneItem::Header(format!("{}/", folder)));
            }
            for f in folder_files {
                items.push(FilesPaneItem::File(f.clone()));
            }
        }
        items
    }

    /// Check if the current selection is in the "Archived Files" section.
    pub fn is_in_archived_section(&self) -> bool {
        section_of(&self.display_items_cache, self.file_selected_index())
            == Some("Archived Files")
    }

    /// Set the file selection directly (used by refresh_after_file_op).
    pub fn set_selection(&mut self, path: Option<std::path::PathBuf>, section: Option<String>) {
        self.file_selection = FileSelection { section, path };
    }
}
