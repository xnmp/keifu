//! Reflog-backed undo: drive each covered op through the real App flow, then
//! undo it and assert exact restoration. Safety-critical — correctness first.

use std::fs;
use std::path::Path;

use git2::{build::CheckoutBuilder, BranchType, Oid, Repository, Signature, Time};
use keifu::action::Action;
use keifu::app::{App, AppMode, ConfirmAction, FocusedPanel};
use keifu::git::GitRepository;
use tempfile::TempDir;

fn commit_wd(repo: &Repository, secs: i64, path: &str, content: &str) -> Oid {
    let wd = repo.workdir().unwrap();
    fs::write(wd.join(path), content).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(path)).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = Signature::new("T", "t@e", &Time::new(secs, 0)).unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, "m", &tree, &parents)
        .unwrap()
}

fn checkout(repo: &Repository, refname: &str) {
    repo.set_head(refname).unwrap();
    repo.checkout_head(Some(CheckoutBuilder::new().force())).unwrap();
}

/// main = a (tip), feature = a <- b (ahead of main), lightweight tag `lw` and
/// annotated tag `ann` both at a. HEAD on main. Real working tree.
fn fixture() -> (TempDir, App, Oid, Oid) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let a = commit_wd(&repo, 1000, "a.txt", "a");
    repo.branch("feature", &repo.find_commit(a).unwrap(), false).unwrap();
    checkout(&repo, "refs/heads/feature");
    let b = commit_wd(&repo, 2000, "b.txt", "b");
    checkout(&repo, "refs/heads/main");

    // Tags at a (scoped so the borrow ends before `repo` is dropped).
    {
        let obj = repo.find_object(a, None).unwrap();
        repo.tag_lightweight("lw", &obj, false).unwrap();
        let sig = Signature::new("T", "t@e", &Time::new(1000, 0)).unwrap();
        repo.tag("ann", &obj, &sig, "annotated", false).unwrap();
    }

    drop(repo);
    let app = App::from_repo(GitRepository::open(dir.path()).unwrap()).unwrap();
    (dir, app, a, b)
}

fn branch_exists(app: &App, name: &str) -> bool {
    app.repo.repo().find_branch(name, BranchType::Local).is_ok()
}

fn branch_tip(app: &App, name: &str) -> Option<Oid> {
    app.repo
        .repo()
        .find_branch(name, BranchType::Local)
        .ok()
        .and_then(|b| b.get().target())
}

fn tag_exists(app: &App, name: &str) -> bool {
    app.repo
        .repo()
        .find_reference(&format!("refs/tags/{name}"))
        .is_ok()
}

fn head(app: &App) -> Oid {
    app.repo.head_oid().unwrap()
}

/// Confirm the given op, then drive Ctrl+Z → confirm through the real handlers.
fn run_op_then_undo(app: &mut App, op: ConfirmAction) {
    app.mode = AppMode::Confirm {
        message: String::new(),
        action: op,
    };
    app.handle_action(Action::Confirm).unwrap();

    app.focused_panel = FocusedPanel::Graph;
    app.mode = AppMode::Normal;
    app.handle_action(Action::UndoLastOp).unwrap();
    assert!(
        matches!(
            app.mode,
            AppMode::Confirm {
                action: ConfirmAction::Undo,
                ..
            }
        ),
        "undo should raise a confirmation"
    );
    app.handle_action(Action::Confirm).unwrap();
}

#[test]
fn undo_branch_delete_recreates_at_the_tip_oid() {
    let (_dir, mut app, _a, feat_tip) = fixture();
    run_op_then_undo(&mut app, ConfirmAction::DeleteBranch("feature".into()));
    assert!(branch_exists(&app, "feature"));
    assert_eq!(branch_tip(&app, "feature"), Some(feat_tip));
    assert!(app.undo_ledger.is_empty());
}

#[test]
fn undo_tag_delete_recreates_the_tag() {
    let (_dir, mut app, _a, _b) = fixture();
    run_op_then_undo(&mut app, ConfirmAction::DeleteTag("lw".into()));
    assert!(tag_exists(&app, "lw"));
}

#[test]
fn undo_annotated_tag_delete_says_lightweight() {
    let (_dir, mut app, _a, _b) = fixture();
    // Delete the annotated tag; the recorded confirm must flag the downgrade.
    app.mode = AppMode::Confirm {
        message: String::new(),
        action: ConfirmAction::DeleteTag("ann".into()),
    };
    app.handle_action(Action::Confirm).unwrap();
    let confirm = &app.undo_ledger.peek().unwrap().confirm;
    assert!(
        confirm.contains("lightweight"),
        "annotated-tag undo must announce the lightweight downgrade: {confirm}"
    );

    // And it still restores the tag.
    app.focused_panel = FocusedPanel::Graph;
    app.mode = AppMode::Normal;
    app.handle_action(Action::UndoLastOp).unwrap();
    app.handle_action(Action::Confirm).unwrap();
    assert!(tag_exists(&app, "ann"));
}

#[test]
fn undo_merge_resets_head_and_keeps_tree() {
    let (_dir, mut app, a, b) = fixture();
    // Fast-forward merge feature into main: HEAD moves a -> b.
    run_op_then_undo(
        &mut app,
        ConfirmAction::Merge { name: "feature".into(), is_remote: false },
    );
    // Undo resets main back to a.
    assert_eq!(head(&app), a, "HEAD reset to the pre-merge commit");
    assert_ne!(head(&app), b);
    assert!(app.undo_ledger.is_empty());
}

#[test]
fn undo_rename_renames_back() {
    let (_dir, mut app, _a, feat_tip) = fixture();
    // Rename via the Input flow.
    app.mode = AppMode::Input {
        title: "Rename".into(),
        input: "renamed".into(),
        action: keifu::app::InputAction::RenameBranch {
            old_name: "feature".into(),
        },
    };
    app.handle_action(Action::Confirm).unwrap();
    assert!(branch_exists(&app, "renamed") && !branch_exists(&app, "feature"));

    app.focused_panel = FocusedPanel::Graph;
    app.mode = AppMode::Normal;
    app.handle_action(Action::UndoLastOp).unwrap();
    app.handle_action(Action::Confirm).unwrap();
    assert!(branch_exists(&app, "feature") && !branch_exists(&app, "renamed"));
    assert_eq!(branch_tip(&app, "feature"), Some(feat_tip));
}

// ── verification failures: never guess ─────────────────────────────────

#[test]
fn undo_dropped_when_branch_was_recreated_since() {
    let (_dir, mut app, a, _b) = fixture();
    app.mode = AppMode::Confirm {
        message: String::new(),
        action: ConfirmAction::DeleteBranch("feature".into()),
    };
    app.handle_action(Action::Confirm).unwrap();
    assert_eq!(app.undo_ledger.len(), 1);

    // Someone recreates the branch (at a different commit) before we undo.
    app.repo
        .repo()
        .branch("feature", &app.repo.repo().find_commit(a).unwrap(), false)
        .unwrap();

    app.focused_panel = FocusedPanel::Graph;
    app.mode = AppMode::Normal;
    app.handle_action(Action::UndoLastOp).unwrap();
    // Verification fails → error dialog, entry dropped, no confirm, no action.
    assert!(matches!(app.mode, AppMode::Error { .. }));
    assert!(app.undo_ledger.is_empty(), "the stale entry is discarded");
    assert!(branch_exists(&app, "feature"));
}

#[test]
fn undo_merge_blocked_by_a_dirty_tree() {
    let (_dir, mut app, _a, b) = fixture();
    app.mode = AppMode::Confirm {
        message: String::new(),
        action: ConfirmAction::Merge { name: "feature".into(), is_remote: false },
    };
    app.handle_action(Action::Confirm).unwrap();
    assert_eq!(head(&app), b);
    assert_eq!(app.undo_ledger.len(), 1);

    // Dirty a tracked file — the reset must refuse.
    let wd = app.repo.repo().workdir().unwrap().to_path_buf();
    fs::write(wd.join("a.txt"), "uncommitted edit").unwrap();

    app.focused_panel = FocusedPanel::Graph;
    app.mode = AppMode::Normal;
    app.handle_action(Action::UndoLastOp).unwrap();
    assert!(matches!(app.mode, AppMode::Error { .. }), "dirty tree blocks undo");
    assert!(app.undo_ledger.is_empty());
    assert_eq!(head(&app), b, "HEAD unchanged — no reset happened");
}

#[test]
fn checkout_is_not_recorded_in_the_undo_ledger() {
    let (_dir, mut app, _a, _b) = fixture();
    app.mode = AppMode::Confirm {
        message: String::new(),
        action: ConfirmAction::Checkout("feature".into()),
    };
    app.handle_action(Action::Confirm).unwrap();
    assert!(
        app.undo_ledger.is_empty(),
        "checkouts pollute the ledger and are deliberately not recorded"
    );
}
