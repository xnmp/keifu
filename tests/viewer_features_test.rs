//! Integration tests for the three viewer features:
//!   1. Compare two arbitrary commits (tree-to-tree diff + mark/compare flow)
//!   2. Per-file history (git log --follow) + opening a history entry's diff
//!   3. Commit signature status (%G? plumbing)

use std::fs;
use std::path::{Path, PathBuf};

use git2::{Oid, Repository, Signature, Time};
use tempfile::TempDir;

use keifu::action::Action;
use keifu::app::{App, AppMode, FileHistoryEntry};
use keifu::diff_cache::DiffTarget;
use keifu::git::{
    commit_signature_status, file_history, signature_status_label, CommitDiffInfo, FileChangeKind,
    GitRepository,
};

// ── Helpers ─────────────────────────────────────────────────────────

fn init_repo() -> (TempDir, Repository) {
    let tempdir = tempfile::tempdir().unwrap();
    let repo = Repository::init(tempdir.path()).unwrap();
    {
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test User").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        cfg.set_bool("commit.gpgsign", false).unwrap();
    }
    (tempdir, repo)
}

/// Commit `path`=`contents` on top of HEAD with an explicit epoch time so
/// commit ordering is deterministic (no same-second ties).
fn commit_at(repo: &Repository, path: &str, contents: &str, message: &str, secs: i64) -> Oid {
    let workdir = repo.workdir().unwrap();
    let full = workdir.join(path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&full, contents).unwrap();

    let mut index = repo.index().unwrap();
    index.add_path(Path::new(path)).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = Signature::new("Test User", "test@example.com", &Time::new(secs, 0)).unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .unwrap()
}

/// Rename `old_path` → `new_path` (identical content) and commit.
fn rename_commit(repo: &Repository, old_path: &str, new_path: &str, secs: i64) -> Oid {
    let workdir = repo.workdir().unwrap();
    let content = fs::read(workdir.join(old_path)).unwrap();
    fs::write(workdir.join(new_path), &content).unwrap();
    fs::remove_file(workdir.join(old_path)).unwrap();

    let mut index = repo.index().unwrap();
    index.remove_path(Path::new(old_path)).unwrap();
    index.add_path(Path::new(new_path)).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = Signature::new("Test User", "test@example.com", &Time::new(secs, 0)).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "rename", &tree, &[&parent])
        .unwrap()
}

fn open_app(dir: &TempDir) -> App {
    let grepo = GitRepository::open(dir.path()).unwrap();
    App::from_repo(grepo).unwrap()
}

fn select_commit(app: &mut App, oid: Oid) {
    let idx = app
        .graph_layout
        .nodes
        .iter()
        .position(|n| n.commit.as_ref().map(|c| c.oid) == Some(oid))
        .expect("commit node present in graph");
    app.graph_nav.graph_list_state.select(Some(idx));
}

// ── Feature 1: two-commit comparison ────────────────────────────────

#[test]
fn from_range_counts_are_older_to_newer() {
    let (dir, repo) = init_repo();
    let c1 = commit_at(&repo, "file.txt", "line1\n", "c1", 1000);
    let c2 = commit_at(&repo, "file.txt", "line1\nline2\nline3\n", "c2", 2000);

    let forward = CommitDiffInfo::from_range(&repo, c1, c2).unwrap();
    assert_eq!(forward.total_files, 1);
    let f = &forward.files[0];
    assert_eq!(f.path, PathBuf::from("file.txt"));
    assert_eq!(f.insertions, 2, "older→newer adds two lines");
    assert_eq!(f.deletions, 0);

    // Reversing the pair flips insertions and deletions.
    let reverse = CommitDiffInfo::from_range(&repo, c2, c1).unwrap();
    assert_eq!(reverse.files[0].insertions, 0);
    assert_eq!(reverse.files[0].deletions, 2);

    drop(dir);
}

#[test]
fn from_range_spans_multiple_commits() {
    let (dir, repo) = init_repo();
    let c1 = commit_at(&repo, "a.txt", "a\n", "c1", 1000);
    let _c2 = commit_at(&repo, "b.txt", "b\n", "c2", 2000);
    let c3 = commit_at(&repo, "c.txt", "c\n", "c3", 3000);

    let info = CommitDiffInfo::from_range(&repo, c1, c3).unwrap();
    let mut names: Vec<String> = info
        .files
        .iter()
        .map(|f| f.path.to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec!["b.txt".to_string(), "c.txt".to_string()]);
    assert_eq!(info.total_files, 2);
    assert!(info.files.iter().all(|f| f.kind == FileChangeKind::Added));

    drop(dir);
}

#[test]
fn mark_then_compare_sets_ordered_range_and_drives_diff() {
    let (dir, repo) = init_repo();
    let c1 = commit_at(&repo, "f.txt", "1\n", "c1", 1000);
    let _c2 = commit_at(&repo, "f.txt", "1\n2\n", "c2", 2000);
    let c3 = commit_at(&repo, "f.txt", "1\n2\n3\n", "c3", 3000);
    drop(repo);

    let mut app = open_app(&dir);

    // Mark the NEWER commit first, then the older one — the range must still be
    // stored older → newer.
    select_commit(&mut app, c3);
    app.handle_action(Action::MarkForCompare).unwrap();
    assert_eq!(app.compare_marked, Some(c3));
    assert_eq!(app.compare_range, None);

    select_commit(&mut app, c1);
    app.handle_action(Action::MarkForCompare).unwrap();
    assert_eq!(app.compare_marked, None);
    assert_eq!(app.compare_range, Some((c1, c3)));

    // The (quick) range diff is now the active diff and lists the changed file.
    app.update_diff_cache();
    let diff = app.cached_diff_or_quick().expect("range diff available");
    assert!(diff.files.iter().any(|f| f.path == Path::new("f.txt")));

    // Esc clears the comparison instead of quitting.
    app.handle_action(Action::Quit).unwrap();
    assert_eq!(app.compare_range, None);
    assert!(!app.should_quit);

    drop(dir);
}

#[test]
fn marking_same_commit_twice_unmarks() {
    let (dir, repo) = init_repo();
    let c1 = commit_at(&repo, "f.txt", "1\n", "c1", 1000);
    drop(repo);

    let mut app = open_app(&dir);
    select_commit(&mut app, c1);

    app.handle_action(Action::MarkForCompare).unwrap();
    assert_eq!(app.compare_marked, Some(c1));

    app.handle_action(Action::MarkForCompare).unwrap();
    assert_eq!(app.compare_marked, None);
    assert_eq!(app.compare_range, None);

    drop(dir);
}

// ── Feature 2: per-file history ─────────────────────────────────────

#[test]
fn file_history_follows_renames() {
    let (dir, repo) = init_repo();
    let c1 = commit_at(&repo, "old.txt", "content\n", "add old", 1000);
    let c2 = commit_at(&repo, "old.txt", "content\nmore\n", "edit old", 2000);
    let c3 = rename_commit(&repo, "old.txt", "new.txt", 3000);
    let path = dir.path().to_string_lossy().to_string();

    let oids = file_history(&path, "new.txt", 200).unwrap();

    // --follow crosses the rename: the pre-rename commits are included.
    assert!(oids.contains(&c3), "rename commit present");
    assert!(oids.contains(&c2), "pre-rename edit present");
    assert!(oids.contains(&c1), "pre-rename add present");
    assert_eq!(oids.len(), 3);

    drop(dir);
}

#[test]
fn file_history_respects_limit() {
    let (dir, repo) = init_repo();
    commit_at(&repo, "f.txt", "1\n", "c1", 1000);
    commit_at(&repo, "f.txt", "1\n2\n", "c2", 2000);
    commit_at(&repo, "f.txt", "1\n2\n3\n", "c3", 3000);
    let path = dir.path().to_string_lossy().to_string();

    let oids = file_history(&path, "f.txt", 2).unwrap();
    assert_eq!(oids.len(), 2, "capped to the requested limit");

    drop(dir);
}

#[test]
fn file_history_enter_opens_that_commits_file_diff() {
    let (dir, repo) = init_repo();
    let _c1 = commit_at(&repo, "f.txt", "1\n", "c1", 1000);
    let c2 = commit_at(&repo, "f.txt", "1\n2\n", "c2", 2000);
    drop(repo);

    let mut app = open_app(&dir);
    app.mode = AppMode::FileHistory {
        path: PathBuf::from("f.txt"),
        entries: vec![FileHistoryEntry {
            oid: c2,
            short_id: c2.to_string()[..7].to_string(),
            date: "2000-01-01".to_string(),
            subject: "c2".to_string(),
        }],
        selected: 0,
    };

    app.handle_action(Action::MenuSelect).unwrap();

    match &app.mode {
        AppMode::FileDiff {
            diff_target,
            content,
            ..
        } => {
            assert_eq!(*diff_target, DiffTarget::Commit(c2));
            assert_eq!(content.path, PathBuf::from("f.txt"));
            // c2 added exactly one line to f.txt.
            assert_eq!(content.total_additions, 1);
        }
        other => panic!("expected FileDiff, got {other:?}"),
    }

    drop(dir);
}

// ── Feature 3: signature status ─────────────────────────────────────

#[test]
fn unsigned_commit_reads_as_unsigned() {
    let (dir, repo) = init_repo();
    let c1 = commit_at(&repo, "f.txt", "1\n", "c1", 1000);
    let path = dir.path().to_string_lossy().to_string();

    // No GPG key material in CI, so the commit is genuinely unsigned.
    assert_eq!(commit_signature_status(&path, c1).unwrap(), 'N');
    assert_eq!(signature_status_label('N'), "unsigned");

    drop(dir);
}
