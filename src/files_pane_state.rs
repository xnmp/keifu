//! Files pane state: selection tracking, display item building, filtering, archive listing.

use crate::git::{
    CommitDiffInfo, FileChangeKind, FileDiffInfo, StageStatus,
};

/// Item in the files pane (header or file entry)
#[derive(Debug, Clone)]
pub enum FilesPaneItem {
    SectionHeader(String),
    FolderHeader(String),
    File(FileDiffInfo),
}

/// Tracks which file is selected in the files pane by (section, path).
/// Resolved to an index from the current display items at point of use,
/// so it never goes stale when items reshuffle.
#[derive(Debug, Clone, Default)]
pub struct FileSelection {
    pub section: Option<String>,
    pub path: Option<std::path::PathBuf>,
}

impl FileSelection {
    /// Resolve this selection to an index within the given items.
    /// Returns the index of the matching item (file or folder header).
    pub fn resolve(&self, items: &[FilesPaneItem]) -> usize {
        // If path is None but section is set, we're selecting a folder header
        if self.path.is_none() {
            if let Some(ref section) = self.section {
                for (i, item) in items.iter().enumerate() {
                    if matches!(item, FilesPaneItem::FolderHeader(t) if t == section) {
                        return i;
                    }
                }
            }
        }

        // Try to find exact match (section + path)
        if let Some(ref path) = self.path {
            if let Some(ref section) = self.section {
                let mut current_section: Option<&str> = None;
                for (i, item) in items.iter().enumerate() {
                    match item {
                        FilesPaneItem::SectionHeader(t) | FilesPaneItem::FolderHeader(t) => {
                            current_section = Some(t);
                        }
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
        // Fall back to first non-section-header item
        items
            .iter()
            .position(|item| !matches!(item, FilesPaneItem::SectionHeader(_)))
            .unwrap_or(0)
    }

    /// Set selection from an index in the given items.
    pub(crate) fn set_from_index(&mut self, idx: usize, items: &[FilesPaneItem]) {
        match items.get(idx) {
            Some(FilesPaneItem::File(f)) => {
                self.path = Some(f.path.clone());
                self.section = items[..=idx]
                    .iter()
                    .rev()
                    .find_map(|item| match item {
                        FilesPaneItem::SectionHeader(t) | FilesPaneItem::FolderHeader(t) => {
                            Some(t.clone())
                        }
                        _ => None,
                    });
            }
            Some(FilesPaneItem::FolderHeader(t)) => {
                self.path = None;
                self.section = Some(t.clone());
            }
            _ => {}
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
            FilesPaneItem::SectionHeader(t) | FilesPaneItem::FolderHeader(t) => Some(t.as_str()),
            _ => None,
        })
}

/// State for the files pane subsystem.
#[derive(Default)]
pub struct FilesPaneState {
    pub file_selection: FileSelection,
    display_items_cache: Vec<FilesPaneItem>,
    pub files_group_by_folder: bool,
    pub files_filter: String,
    pub files_filter_active: bool,
}

impl FilesPaneState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sync the display items cache from the current diff.
    pub fn sync_file_list_cache(
        &mut self,
        diff: Option<&CommitDiffInfo>,
        is_uncommitted: bool,
        repo_path: &str,
    ) {
        self.display_items_cache = self.build_files_pane_items(diff, is_uncommitted, repo_path);
    }

    /// Files in display order — the flat list the diff viewer's file
    /// navigation cycles through. Must stay in the same index space as
    /// `display_index_to_flat_index`/`flat_index_to_display_index`
    /// (a partially-staged file appears once per section it's shown in).
    pub fn display_file_list(&self) -> Vec<FileDiffInfo> {
        self.display_items_cache
            .iter()
            .filter_map(|item| match item {
                FilesPaneItem::File(f) => Some(f.clone()),
                _ => None,
            })
            .collect()
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
    /// For a single file, returns just that file.
    /// For a folder header, returns all files under that folder (until the next header).
    pub fn selected_files(&self) -> Vec<&FileDiffInfo> {
        let idx = self.file_selected_index();
        let items = &self.display_items_cache;

        match items.get(idx) {
            Some(FilesPaneItem::File(f)) => vec![f],
            Some(FilesPaneItem::FolderHeader(_)) => items[idx + 1..]
                .iter()
                .take_while(|item| matches!(item, FilesPaneItem::File(_)))
                .filter_map(|item| match item {
                    FilesPaneItem::File(f) => Some(f),
                    _ => None,
                })
                .collect(),
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
        // Skip only section headers (folder headers are selectable)
        let dir = if delta >= 0 { 1i32 } else { -1 };
        while target < items.len() && matches!(items[target], FilesPaneItem::SectionHeader(_)) {
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
                items.push(FilesPaneItem::SectionHeader("Archived Files".to_string()));
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
        let conflicted: Vec<_> = files
            .iter()
            .filter(|f| matches!(f.stage_status, Some(StageStatus::Conflicted)))
            .cloned()
            .collect();
        let staged: Vec<_> = files
            .iter()
            .filter(|f| matches!(f.stage_status, Some(StageStatus::Staged)))
            .cloned()
            .collect();
        let unstaged: Vec<_> = files
            .iter()
            .filter(|f| {
                !matches!(
                    f.stage_status,
                    Some(StageStatus::Staged) | Some(StageStatus::Conflicted)
                )
            })
            .cloned()
            .collect();

        let mut items = Vec::new();
        // Merge Changes first: it needs the most attention.
        if !conflicted.is_empty() {
            items.push(FilesPaneItem::SectionHeader("Merge Changes".to_string()));
            items.extend(conflicted.into_iter().map(FilesPaneItem::File));
        }
        if !staged.is_empty() {
            items.push(FilesPaneItem::SectionHeader("Staged Changes".to_string()));
            items.extend(staged.into_iter().map(FilesPaneItem::File));
        }
        if !unstaged.is_empty() {
            items.push(FilesPaneItem::SectionHeader("Unstaged Changes".to_string()));
            items.extend(unstaged.into_iter().map(FilesPaneItem::File));
        }
        items
    }

    fn build_folder_grouped_items(files: &[FileDiffInfo]) -> Vec<FilesPaneItem> {
        Self::folder_group(files)
    }

    /// Staged/unstaged sections with folder grouping within each section.
    fn build_staged_unstaged_folder_items(files: &[FileDiffInfo]) -> Vec<FilesPaneItem> {
        let conflicted: Vec<_> = files
            .iter()
            .filter(|f| matches!(f.stage_status, Some(StageStatus::Conflicted)))
            .cloned()
            .collect();
        let staged: Vec<_> = files
            .iter()
            .filter(|f| matches!(f.stage_status, Some(StageStatus::Staged)))
            .cloned()
            .collect();
        let unstaged: Vec<_> = files
            .iter()
            .filter(|f| {
                !matches!(
                    f.stage_status,
                    Some(StageStatus::Staged) | Some(StageStatus::Conflicted)
                )
            })
            .cloned()
            .collect();

        let mut items = Vec::new();
        if !conflicted.is_empty() {
            items.push(FilesPaneItem::SectionHeader("Merge Changes".to_string()));
            items.extend(Self::folder_group(&conflicted));
        }
        if !staged.is_empty() {
            items.push(FilesPaneItem::SectionHeader("Staged Changes".to_string()));
            items.extend(Self::folder_group(&staged));
        }
        if !unstaged.is_empty() {
            items.push(FilesPaneItem::SectionHeader("Unstaged Changes".to_string()));
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
                items.push(FilesPaneItem::FolderHeader(format!("{}/", folder)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn file(path: &str, kind: FileChangeKind, stage: Option<StageStatus>) -> FileDiffInfo {
        FileDiffInfo {
            path: PathBuf::from(path),
            kind,
            is_binary: false,
            insertions: 1,
            deletions: 0,
            stage_status: stage,
        }
    }

    fn staged(path: &str) -> FileDiffInfo {
        file(path, FileChangeKind::Modified, Some(StageStatus::Staged))
    }

    fn unstaged(path: &str) -> FileDiffInfo {
        file(path, FileChangeKind::Modified, Some(StageStatus::Unstaged))
    }

    fn plain(path: &str) -> FileDiffInfo {
        file(path, FileChangeKind::Modified, None)
    }

    fn header(text: &str) -> FilesPaneItem {
        FilesPaneItem::SectionHeader(text.to_string())
    }

    fn fitem(f: FileDiffInfo) -> FilesPaneItem {
        FilesPaneItem::File(f)
    }

    fn make_diff(files: Vec<FileDiffInfo>) -> CommitDiffInfo {
        CommitDiffInfo {
            files,
            total_insertions: 0,
            total_deletions: 0,
            total_files: 0,
            truncated: false,
            staged_files: vec![],
            unstaged_files: vec![],
        }
    }

    fn make_uncommitted_diff(
        staged_files: Vec<FileDiffInfo>,
        unstaged_files: Vec<FileDiffInfo>,
    ) -> CommitDiffInfo {
        let mut all = staged_files.clone();
        all.extend(unstaged_files.clone());
        CommitDiffInfo {
            files: all,
            total_insertions: 0,
            total_deletions: 0,
            total_files: 0,
            truncated: false,
            staged_files,
            unstaged_files,
        }
    }

    fn path_at(items: &[FilesPaneItem], idx: usize) -> &str {
        match &items[idx] {
            FilesPaneItem::File(f) => f.path.to_str().unwrap(),
            FilesPaneItem::SectionHeader(t) | FilesPaneItem::FolderHeader(t) => {
                panic!("expected file at index {}, got header: {}", idx, t)
            }
        }
    }

    // ─── FileSelection::resolve ───────────────────────────────────────

    #[test]
    fn resolve_exact_match_section_and_path() {
        let items = vec![
            header("Staged Changes"),
            fitem(staged("a.rs")),
            header("Unstaged Changes"),
            fitem(unstaged("b.rs")),
        ];
        let sel = FileSelection {
            section: Some("Unstaged Changes".to_string()),
            path: Some(PathBuf::from("b.rs")),
        };
        assert_eq!(sel.resolve(&items), 3);
    }

    #[test]
    fn resolve_path_match_wrong_section_falls_back_to_any_section() {
        let items = vec![
            header("Staged Changes"),
            fitem(staged("a.rs")),
            header("Unstaged Changes"),
            fitem(unstaged("b.rs")),
        ];
        let sel = FileSelection {
            section: Some("Nonexistent Section".to_string()),
            path: Some(PathBuf::from("b.rs")),
        };
        // Falls back to path match in any section
        assert_eq!(sel.resolve(&items), 3);
    }

    #[test]
    fn resolve_path_match_no_section_set() {
        let items = vec![
            header("Staged Changes"),
            fitem(staged("a.rs")),
            fitem(staged("b.rs")),
        ];
        let sel = FileSelection {
            section: None,
            path: Some(PathBuf::from("b.rs")),
        };
        assert_eq!(sel.resolve(&items), 2);
    }

    #[test]
    fn resolve_falls_back_to_first_file_when_path_gone() {
        let items = vec![
            header("Staged Changes"),
            fitem(staged("a.rs")),
            fitem(staged("b.rs")),
        ];
        let sel = FileSelection {
            section: Some("Staged Changes".to_string()),
            path: Some(PathBuf::from("gone.rs")),
        };
        assert_eq!(sel.resolve(&items), 1); // first file, skipping header
    }

    #[test]
    fn resolve_never_returns_header_index() {
        let items = vec![
            header("Section"),
            fitem(plain("a.rs")),
        ];
        let sel = FileSelection::default();
        let idx = sel.resolve(&items);
        assert!(matches!(items[idx], FilesPaneItem::File(_)));
    }

    #[test]
    fn resolve_returns_zero_for_empty_items() {
        let sel = FileSelection {
            section: Some("Staged Changes".to_string()),
            path: Some(PathBuf::from("a.rs")),
        };
        assert_eq!(sel.resolve(&[]), 0);
    }

    // ─── section_of ──────────────────────────────────────────────────

    #[test]
    fn section_of_file_under_header() {
        let items = vec![
            header("Staged Changes"),
            fitem(staged("a.rs")),
            fitem(staged("b.rs")),
        ];
        assert_eq!(section_of(&items, 1), Some("Staged Changes"));
        assert_eq!(section_of(&items, 2), Some("Staged Changes"));
    }

    #[test]
    fn section_of_returns_none_when_no_header_above() {
        let items = vec![fitem(plain("a.rs")), fitem(plain("b.rs"))];
        assert_eq!(section_of(&items, 0), None);
        assert_eq!(section_of(&items, 1), None);
    }

    #[test]
    fn section_of_header_returns_its_own_text() {
        let items = vec![header("My Section"), fitem(plain("a.rs"))];
        assert_eq!(section_of(&items, 0), Some("My Section"));
    }

    #[test]
    fn section_of_empty_items() {
        assert_eq!(section_of(&[], 0), None);
    }

    // ─── folder_group ────────────────────────────────────────────────

    #[test]
    fn folder_group_root_files_have_no_header() {
        let files = vec![plain("a.rs"), plain("b.rs")];
        let items = FilesPaneState::folder_group(&files);
        // Root files should not produce a header
        assert!(items.iter().all(|i| matches!(i, FilesPaneItem::File(_))));
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn folder_group_nested_files_get_folder_header() {
        let files = vec![plain("src/main.rs"), plain("src/lib.rs")];
        let items = FilesPaneState::folder_group(&files);
        assert_eq!(items.len(), 3);
        assert!(matches!(&items[0], FilesPaneItem::FolderHeader(t) if t == "src/"));
        assert_eq!(path_at(&items, 1), "src/main.rs");
        assert_eq!(path_at(&items, 2), "src/lib.rs");
    }

    #[test]
    fn folder_group_sorted_alphabetically_by_folder() {
        let files = vec![
            plain("z_dir/file.rs"),
            plain("a_dir/file.rs"),
        ];
        let items = FilesPaneState::folder_group(&files);
        // BTreeMap sorts keys, so a_dir comes before z_dir
        assert!(matches!(&items[0], FilesPaneItem::FolderHeader(t) if t == "a_dir/"));
        assert!(matches!(&items[2], FilesPaneItem::FolderHeader(t) if t == "z_dir/"));
    }

    #[test]
    fn folder_group_multiple_files_same_folder() {
        let files = vec![
            plain("src/a.rs"),
            plain("src/b.rs"),
            plain("src/c.rs"),
        ];
        let items = FilesPaneState::folder_group(&files);
        // One header + three files
        assert_eq!(items.len(), 4);
        assert!(matches!(&items[0], FilesPaneItem::FolderHeader(t) if t == "src/"));
    }

    #[test]
    fn folder_group_mix_of_root_and_nested() {
        let files = vec![
            plain("root.rs"),
            plain("src/nested.rs"),
        ];
        let items = FilesPaneState::folder_group(&files);
        // BTreeMap: "." < "src", root files come first with no header
        let mut saw_root_file = false;
        let mut saw_folder_header = false;
        for item in &items {
            match item {
                FilesPaneItem::File(f) if f.path == Path::new("root.rs") => {
                    // root file should appear before any folder header
                    assert!(!saw_folder_header);
                    saw_root_file = true;
                }
                FilesPaneItem::FolderHeader(t) if t == "src/" => saw_folder_header = true,
                _ => {}
            }
        }
        assert!(saw_root_file);
        assert!(saw_folder_header);
    }

    // ─── build_staged_unstaged_items ─────────────────────────────────

    #[test]
    fn staged_unstaged_only_staged() {
        let files = vec![staged("a.rs"), staged("b.rs")];
        let items = FilesPaneState::build_staged_unstaged_items(&files);
        assert!(matches!(&items[0], FilesPaneItem::SectionHeader(t) if t == "Staged Changes"));
        assert_eq!(items.len(), 3); // 1 header + 2 files
    }

    #[test]
    fn staged_unstaged_only_unstaged() {
        let files = vec![unstaged("a.rs")];
        let items = FilesPaneState::build_staged_unstaged_items(&files);
        assert!(matches!(&items[0], FilesPaneItem::SectionHeader(t) if t == "Unstaged Changes"));
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn staged_unstaged_both() {
        let files = vec![staged("s.rs"), unstaged("u.rs")];
        let items = FilesPaneState::build_staged_unstaged_items(&files);
        assert!(matches!(&items[0], FilesPaneItem::SectionHeader(t) if t == "Staged Changes"));
        assert_eq!(path_at(&items, 1), "s.rs");
        assert!(matches!(&items[2], FilesPaneItem::SectionHeader(t) if t == "Unstaged Changes"));
        assert_eq!(path_at(&items, 3), "u.rs");
    }

    #[test]
    fn staged_unstaged_empty() {
        let items = FilesPaneState::build_staged_unstaged_items(&[]);
        assert!(items.is_empty());
    }

    // ─── build_files_pane_items ──────────────────────────────────────

    #[test]
    fn build_committed_diff_flat_list() {
        let diff = make_diff(vec![plain("a.rs"), plain("b.rs")]);
        let state = FilesPaneState::new();
        let items = state.build_files_pane_items(Some(&diff), false, "/nonexistent");
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| matches!(i, FilesPaneItem::File(_))));
    }

    #[test]
    fn build_uncommitted_with_staged_unstaged() {
        let diff = make_uncommitted_diff(vec![staged("s.rs")], vec![unstaged("u.rs")]);
        let state = FilesPaneState::new();
        let items = state.build_files_pane_items(Some(&diff), true, "/nonexistent");
        assert!(matches!(&items[0], FilesPaneItem::SectionHeader(t) if t == "Staged Changes"));
        assert_eq!(path_at(&items, 1), "s.rs");
        assert!(matches!(&items[2], FilesPaneItem::SectionHeader(t) if t == "Unstaged Changes"));
        assert_eq!(path_at(&items, 3), "u.rs");
    }

    #[test]
    fn build_with_folder_grouping_committed() {
        let diff = make_diff(vec![plain("src/a.rs"), plain("src/b.rs"), plain("root.rs")]);
        let mut state = FilesPaneState::new();
        state.files_group_by_folder = true;
        let items = state.build_files_pane_items(Some(&diff), false, "/nonexistent");
        // Should have folder headers
        let headers: Vec<_> = items
            .iter()
            .filter_map(|i| match i {
                FilesPaneItem::SectionHeader(t) | FilesPaneItem::FolderHeader(t) => {
                    Some(t.as_str())
                }
                _ => None,
            })
            .collect();
        assert!(headers.contains(&"src/"));
    }

    #[test]
    fn build_with_filter_active() {
        let diff = make_diff(vec![plain("foo.rs"), plain("bar.py"), plain("baz.rs")]);
        let mut state = FilesPaneState::new();
        state.files_filter = "foo".to_string();
        let items = state.build_files_pane_items(Some(&diff), false, "/nonexistent");
        assert_eq!(items.len(), 1);
        assert_eq!(path_at(&items, 0), "foo.rs");
    }

    #[test]
    fn build_filter_no_matches() {
        let diff = make_diff(vec![plain("foo.rs")]);
        let mut state = FilesPaneState::new();
        state.files_filter = "zzz".to_string();
        let items = state.build_files_pane_items(Some(&diff), false, "/nonexistent");
        assert!(items.is_empty());
    }

    #[test]
    fn build_none_diff_returns_empty() {
        let state = FilesPaneState::new();
        let items = state.build_files_pane_items(None, false, "/nonexistent");
        assert!(items.is_empty());
        let items = state.build_files_pane_items(None, true, "/nonexistent");
        assert!(items.is_empty());
    }

    // ─── Index mapping ───────────────────────────────────────────────

    fn make_state_with_items(items: Vec<FilesPaneItem>) -> FilesPaneState {
        FilesPaneState {
            display_items_cache: items,
            ..FilesPaneState::default()
        }
    }

    #[test]
    fn display_to_flat_skips_headers() {
        let state = make_state_with_items(vec![
            header("Staged"),
            fitem(staged("a.rs")),
            fitem(staged("b.rs")),
            header("Unstaged"),
            fitem(unstaged("c.rs")),
        ]);
        // display 1 (a.rs) → flat 0
        assert_eq!(state.display_index_to_flat_index(1), 0);
        // display 2 (b.rs) → flat 1
        assert_eq!(state.display_index_to_flat_index(2), 1);
        // display 4 (c.rs) → flat 2
        assert_eq!(state.display_index_to_flat_index(4), 2);
    }

    #[test]
    fn flat_to_display_finds_correct_position() {
        let state = make_state_with_items(vec![
            header("Staged"),
            fitem(staged("a.rs")),
            fitem(staged("b.rs")),
            header("Unstaged"),
            fitem(unstaged("c.rs")),
        ]);
        assert_eq!(state.flat_index_to_display_index(0), 1);
        assert_eq!(state.flat_index_to_display_index(1), 2);
        assert_eq!(state.flat_index_to_display_index(2), 4);
    }

    #[test]
    fn index_roundtrip_flat_display_flat() {
        let state = make_state_with_items(vec![
            header("Section"),
            fitem(plain("a.rs")),
            fitem(plain("b.rs")),
            header("Section 2"),
            fitem(plain("c.rs")),
        ]);
        for flat in 0..3 {
            let display = state.flat_index_to_display_index(flat);
            let back = state.display_index_to_flat_index(display);
            assert_eq!(back, flat, "roundtrip failed for flat index {}", flat);
        }
    }

    #[test]
    fn index_boundary_first_and_last() {
        let state = make_state_with_items(vec![
            fitem(plain("first.rs")),
            header("Mid"),
            fitem(plain("last.rs")),
        ]);
        assert_eq!(state.display_index_to_flat_index(0), 0);
        assert_eq!(state.display_index_to_flat_index(2), 1);
        assert_eq!(state.flat_index_to_display_index(0), 0);
        assert_eq!(state.flat_index_to_display_index(1), 2);
    }

    // ─── Navigation (move_file_selection) ────────────────────────────

    #[test]
    fn move_down_advances_to_next_file() {
        let mut state = make_state_with_items(vec![
            fitem(plain("a.rs")),
            fitem(plain("b.rs")),
            fitem(plain("c.rs")),
        ]);
        state.select_file_at(0);
        state.move_file_selection(1);
        assert_eq!(state.file_selected_index(), 1);
    }

    #[test]
    fn move_up_goes_to_previous_file() {
        let mut state = make_state_with_items(vec![
            fitem(plain("a.rs")),
            fitem(plain("b.rs")),
            fitem(plain("c.rs")),
        ]);
        state.select_file_at(2);
        state.move_file_selection(-1);
        assert_eq!(state.file_selected_index(), 1);
    }

    #[test]
    fn move_skips_headers() {
        let mut state = make_state_with_items(vec![
            header("Staged"),
            fitem(staged("a.rs")),
            header("Unstaged"),
            fitem(unstaged("b.rs")),
        ]);
        state.select_file_at(1); // a.rs
        state.move_file_selection(1); // should skip header at 2, land on b.rs at 3
        assert_eq!(state.file_selected_index(), 3);
    }

    #[test]
    fn move_clamps_at_bottom() {
        let mut state = make_state_with_items(vec![
            fitem(plain("a.rs")),
            fitem(plain("b.rs")),
        ]);
        state.select_file_at(1);
        state.move_file_selection(1);
        assert_eq!(state.file_selected_index(), 1); // stays at last
    }

    #[test]
    fn move_clamps_at_top() {
        let mut state = make_state_with_items(vec![
            fitem(plain("a.rs")),
            fitem(plain("b.rs")),
        ]);
        state.select_file_at(0);
        state.move_file_selection(-1);
        assert_eq!(state.file_selected_index(), 0);
    }

    #[test]
    fn move_empty_list_is_noop() {
        let mut state = make_state_with_items(vec![]);
        state.move_file_selection(1);
        state.move_file_selection(-1);
        // No items to select — resolve() always falls back to index 0.
        assert_eq!(state.file_selected_index(), 0);
    }

    // ─── Filtering ──────────────────────────────────────────────────

    #[test]
    fn filter_is_case_insensitive() {
        let diff = make_diff(vec![plain("Foo.RS"), plain("bar.py")]);
        let mut state = FilesPaneState::new();
        state.files_filter = "foo".to_string();
        let items = state.build_files_pane_items(Some(&diff), false, "/nonexistent");
        assert_eq!(items.len(), 1);
        assert_eq!(path_at(&items, 0), "Foo.RS");
    }

    #[test]
    fn filter_fuzzy_all_chars_must_appear() {
        let diff = make_diff(vec![plain("abcdef.rs"), plain("xyz.rs")]);
        let mut state = FilesPaneState::new();
        // 'a', 'c', 'f' all appear in "abcdef.rs" but not in "xyz.rs"
        state.files_filter = "acf".to_string();
        let items = state.build_files_pane_items(Some(&diff), false, "/nonexistent");
        assert_eq!(items.len(), 1);
        assert_eq!(path_at(&items, 0), "abcdef.rs");
    }

    #[test]
    fn filter_empty_shows_all() {
        let diff = make_diff(vec![plain("a.rs"), plain("b.rs"), plain("c.rs")]);
        let mut state = FilesPaneState::new();
        state.files_filter = "".to_string();
        let items = state.build_files_pane_items(Some(&diff), false, "/nonexistent");
        assert_eq!(items.len(), 3);
    }

    // ─── set_from_index / set_selection ──────────────────────────────

    #[test]
    fn set_from_index_captures_section_and_path() {
        let items = vec![
            header("Staged Changes"),
            fitem(staged("a.rs")),
            header("Unstaged Changes"),
            fitem(unstaged("b.rs")),
        ];
        let mut sel = FileSelection::default();
        sel.set_from_index(3, &items);
        assert_eq!(sel.path, Some(PathBuf::from("b.rs")));
        assert_eq!(sel.section, Some("Unstaged Changes".to_string()));
    }

    #[test]
    fn set_from_index_no_header_above() {
        let items = vec![fitem(plain("a.rs")), fitem(plain("b.rs"))];
        let mut sel = FileSelection::default();
        sel.set_from_index(0, &items);
        assert_eq!(sel.path, Some(PathBuf::from("a.rs")));
        assert_eq!(sel.section, None);
    }

    // ─── build_staged_unstaged_folder_items ──────────────────────────

    #[test]
    fn folder_grouping_within_staged_unstaged_sections() {
        let diff = make_uncommitted_diff(
            vec![staged("src/a.rs"), staged("src/b.rs")],
            vec![unstaged("tests/c.rs")],
        );
        let mut state = FilesPaneState::new();
        state.files_group_by_folder = true;
        let items = state.build_files_pane_items(Some(&diff), true, "/nonexistent");
        // Staged Changes > src/ > a.rs, b.rs > Unstaged Changes > tests/ > c.rs
        let headers: Vec<String> = items
            .iter()
            .filter_map(|i| match i {
                FilesPaneItem::SectionHeader(t) | FilesPaneItem::FolderHeader(t) => {
                    Some(t.clone())
                }
                _ => None,
            })
            .collect();
        assert!(headers.contains(&"Staged Changes".to_string()));
        assert!(headers.contains(&"src/".to_string()));
        assert!(headers.contains(&"Unstaged Changes".to_string()));
        assert!(headers.contains(&"tests/".to_string()));
    }
}
