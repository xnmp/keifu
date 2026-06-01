//! Tests for the section-aware file selection neighbor logic
//! used by refresh_after_file_op. These are pure data tests — no git repos needed.

use std::path::PathBuf;

use keifu::files_pane_state::{FileSelection, FilesPaneItem, section_of};
use keifu::git::{FileChangeKind, FileDiffInfo};

fn h(name: &str) -> FilesPaneItem {
    FilesPaneItem::SectionHeader(name.to_string())
}

fn f(name: &str) -> FilesPaneItem {
    FilesPaneItem::File(FileDiffInfo {
        path: PathBuf::from(name),
        kind: FileChangeKind::Modified,
        is_binary: false,
        insertions: 0,
        deletions: 0,
        stage_status: None,
    })
}

fn path_at(items: &[FilesPaneItem], idx: usize) -> &str {
    match &items[idx] {
        FilesPaneItem::File(f) => f.path.to_str().unwrap(),
        FilesPaneItem::SectionHeader(t) | FilesPaneItem::FolderHeader(t) => {
            panic!("index {} is header: {}", idx, t)
        }
    }
}

/// Reimplements the neighbor-finding logic from App::refresh_after_file_op
/// so we can test it in isolation with synthetic item lists.
fn compute_next_selection(
    old_items: &[FilesPaneItem],
    old_idx: usize,
    new_items: &[FilesPaneItem],
) -> FileSelection {
    let old_section = section_of(old_items, old_idx);

    let next_in_section: Vec<PathBuf> = old_items[old_idx + 1..]
        .iter()
        .take_while(|item| matches!(item, FilesPaneItem::File(_)))
        .filter_map(|item| match item {
            FilesPaneItem::File(f) => Some(f.path.clone()),
            _ => None,
        })
        .collect();

    let prev_in_section: Vec<PathBuf> = old_items[..old_idx]
        .iter()
        .rev()
        .take_while(|item| matches!(item, FilesPaneItem::File(_)))
        .filter_map(|item| match item {
            FilesPaneItem::File(f) => Some(f.path.clone()),
            _ => None,
        })
        .collect();

    let target = next_in_section
        .iter()
        .chain(prev_in_section.iter())
        .find_map(|path| {
            let i = new_items.iter().position(
                |item| matches!(item, FilesPaneItem::File(fi) if fi.path == *path),
            )?;
            if section_of(new_items, i) == old_section {
                Some((path.clone(), old_section.map(|s| s.to_string())))
            } else {
                None
            }
        });

    if let Some((path, sec)) = target {
        FileSelection {
            path: Some(path),
            section: sec,
        }
    } else {
        let mut sel = FileSelection::default();
        if let Some(FilesPaneItem::File(f)) = old_items.get(old_idx) {
            sel.path = Some(f.path.clone());
            sel.section = old_section.map(|s| s.to_string());
        }
        sel
    }
}

fn selected_path_after(
    old_items: &[FilesPaneItem],
    old_idx: usize,
    new_items: &[FilesPaneItem],
) -> String {
    let sel = compute_next_selection(old_items, old_idx, new_items);
    let idx = sel.resolve(new_items);
    path_at(new_items, idx).to_string()
}

#[test]
fn stage_first_unstaged_selects_next_unstaged() {
    let old = vec![h("Unstaged Changes"), f("a.txt"), f("b.txt"), f("c.txt")];
    let new = vec![
        h("Staged Changes"), f("a.txt"),
        h("Unstaged Changes"), f("b.txt"), f("c.txt"),
    ];
    assert_eq!(selected_path_after(&old, 1, &new), "b.txt");
}

#[test]
fn stage_last_unstaged_selects_prev_unstaged() {
    let old = vec![h("Unstaged Changes"), f("a.txt"), f("b.txt"), f("c.txt")];
    let new = vec![
        h("Staged Changes"), f("c.txt"),
        h("Unstaged Changes"), f("a.txt"), f("b.txt"),
    ];
    assert_eq!(selected_path_after(&old, 3, &new), "b.txt");
}

#[test]
fn stage_middle_unstaged_selects_next_unstaged() {
    let old = vec![h("Unstaged Changes"), f("a.txt"), f("b.txt"), f("c.txt")];
    let new = vec![
        h("Staged Changes"), f("b.txt"),
        h("Unstaged Changes"), f("a.txt"), f("c.txt"),
    ];
    assert_eq!(selected_path_after(&old, 2, &new), "c.txt");
}

#[test]
fn stage_only_unstaged_falls_back() {
    let old = vec![h("Unstaged Changes"), f("a.txt")];
    let new = vec![h("Staged Changes"), f("a.txt")];
    assert_eq!(selected_path_after(&old, 1, &new), "a.txt");
}

#[test]
fn stage_with_existing_staged_selects_next_unstaged() {
    let old = vec![
        h("Staged Changes"), f("x.txt"),
        h("Unstaged Changes"), f("app.rs"), f("fdsklt"), f("gshifdg"),
    ];
    let new = vec![
        h("Staged Changes"), f("app.rs"), f("x.txt"),
        h("Unstaged Changes"), f("fdsklt"), f("gshifdg"),
    ];
    assert_eq!(selected_path_after(&old, 3, &new), "fdsklt");
}

#[test]
fn unstage_first_staged_selects_next_staged() {
    let old = vec![
        h("Staged Changes"), f("a.txt"), f("b.txt"),
        h("Unstaged Changes"), f("c.txt"),
    ];
    let new = vec![
        h("Staged Changes"), f("b.txt"),
        h("Unstaged Changes"), f("a.txt"), f("c.txt"),
    ];
    assert_eq!(selected_path_after(&old, 1, &new), "b.txt");
}

#[test]
fn unstage_only_staged_falls_back() {
    let old = vec![
        h("Staged Changes"), f("a.txt"),
        h("Unstaged Changes"), f("b.txt"),
    ];
    let new = vec![h("Unstaged Changes"), f("a.txt"), f("b.txt")];
    assert_eq!(selected_path_after(&old, 1, &new), "a.txt");
}

#[test]
fn file_removed_selects_next_in_section() {
    let old = vec![h("Unstaged Changes"), f("a.txt"), f("b.txt"), f("c.txt")];
    let new = vec![h("Unstaged Changes"), f("a.txt"), f("c.txt")];
    assert_eq!(selected_path_after(&old, 2, &new), "c.txt");
}

#[test]
fn last_file_removed_selects_prev_in_section() {
    let old = vec![h("Unstaged Changes"), f("a.txt"), f("b.txt")];
    let new = vec![h("Unstaged Changes"), f("a.txt")];
    assert_eq!(selected_path_after(&old, 2, &new), "a.txt");
}

#[test]
fn archived_section_stays_in_archived() {
    let old = vec![
        h("Unstaged Changes"), f("a.txt"),
        h("Archived Files"), f("old.txt"), f("stale.txt"),
    ];
    let new = vec![
        h("Unstaged Changes"), f("a.txt"), f("old.txt"),
        h("Archived Files"), f("stale.txt"),
    ];
    assert_eq!(selected_path_after(&old, 3, &new), "stale.txt");
}
