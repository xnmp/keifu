//! Commit-menu Push gating (issue #87).
//!
//! The menu's Push item targets the checked-out HEAD branch (`initiate_push`
//! pushes HEAD), so it must only appear when the *selected* branch is HEAD and
//! actually has work to push. These tests assert the observable menu contract
//! for each selectable branch label — remote-only, non-HEAD local, and HEAD in
//! its various publish/ahead/in-sync states.

use keifu::action::Action;
use keifu::app::{App, AppMode, CommitMenuItem, FocusedPanel};
use keifu::git::operations::{create_branch, delete_branch};
use keifu::git::GitRepository;

mod common;
use common::{add_bare_origin, commit_file, current_branch, git_cli, head_oid, init_repo, Seed};

/// Build an App over a fresh open of the repo at `path`, focused on the graph.
fn app_at(path: &str) -> App {
    let mut app = App::from_repo(GitRepository::open(path).unwrap()).unwrap();
    app.focused_panel = FocusedPanel::Graph;
    app
}

/// Select the graph node carrying the branch label `branch`, open its commit
/// menu, and return the constructed item list. Panics if the label isn't a
/// graph ref or the menu doesn't open.
fn menu_items_for_branch(app: &mut App, branch: &str) -> Vec<CommitMenuItem> {
    let pos = app
        .graph_nav
        .branch_positions
        .iter()
        .position(|(_, n)| n == branch)
        .unwrap_or_else(|| panic!("{branch} should be a graph ref"));
    let (node_idx, _) = app.graph_nav.branch_positions[pos];
    app.graph_nav.graph_list_state.select(Some(node_idx));
    app.graph_nav.selected_branch_position = Some(pos);

    app.handle_action(Action::OpenCommitMenu).unwrap();
    match &app.mode {
        AppMode::CommitMenu { items, .. } => items.clone(),
        other => panic!("expected a CommitMenu, got {other:?}"),
    }
}

#[test]
fn push_absent_for_remote_only_branch() {
    // Publish `feature` to origin, drop the local copy, then advance HEAD so the
    // node keeps only the remote-tracking `origin/feature` label.
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let c0 = head_oid(git_repo.repo());
    let _origin = add_bare_origin(&path);

    create_branch(git_repo.repo(), "feature", c0).unwrap();
    git_cli(&path, &["push", "origin", "feature"]);
    git_cli(&path, &["fetch", "origin"]);
    delete_branch(git_repo.repo(), "feature").unwrap();
    // Move HEAD forward so origin/feature sits alone on the c0 node.
    commit_file(git_repo.repo(), "b.txt", "b", "advance");

    let mut app = app_at(&path);
    let items = menu_items_for_branch(&mut app, "origin/feature");

    assert!(
        !items.contains(&CommitMenuItem::Push),
        "Push must not be offered for a remote-only branch, got {items:?}"
    );
    assert!(
        !items.contains(&CommitMenuItem::Pull),
        "Pull is HEAD-only and must not appear for a remote branch, got {items:?}"
    );
}

#[test]
fn push_absent_for_non_head_local_branch() {
    // `feature` points at an earlier commit than HEAD, so it's a distinct,
    // non-HEAD local label.
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();

    git_cli(&path, &["branch", "feature"]); // feature -> current HEAD
    commit_file(git_repo.repo(), "b.txt", "b", "advance"); // HEAD moves on, feature stays

    let mut app = app_at(&path);
    let items = menu_items_for_branch(&mut app, "feature");

    assert!(
        !items.contains(&CommitMenuItem::Push),
        "Push must not be offered for a non-HEAD local branch, got {items:?}"
    );
    assert!(
        !items.contains(&CommitMenuItem::Pull),
        "Pull must not appear for a non-HEAD branch, got {items:?}"
    );
}

#[test]
fn push_present_for_head_branch_ahead_of_upstream() {
    // Publish HEAD (upstream at c0), then commit once more so HEAD is ahead.
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let _origin = add_bare_origin(&path);
    let head = current_branch(git_repo.repo());

    git_cli(&path, &["push", "-u", "origin", "HEAD"]);
    commit_file(git_repo.repo(), "b.txt", "b", "unpushed work");

    let mut app = app_at(&path);
    let items = menu_items_for_branch(&mut app, &head);

    assert!(
        items.contains(&CommitMenuItem::Push),
        "a HEAD branch ahead of its upstream must offer Push, got {items:?}"
    );
}

#[test]
fn push_present_for_head_branch_without_upstream() {
    // Fresh repo, no remote: HEAD has no upstream, so Push offers the publish
    // flow.
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let head = current_branch(git_repo.repo());

    let mut app = app_at(&path);
    let items = menu_items_for_branch(&mut app, &head);

    assert!(
        items.contains(&CommitMenuItem::Push),
        "an unpublished HEAD branch must offer Push (publish), got {items:?}"
    );
}

#[test]
fn push_absent_for_head_branch_in_sync_with_upstream() {
    // Publish HEAD and make no further commits: ahead == 0 with an upstream,
    // nothing to push.
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let _origin = add_bare_origin(&path);
    let head = current_branch(git_repo.repo());

    git_cli(&path, &["push", "-u", "origin", "HEAD"]);

    let mut app = app_at(&path);
    let items = menu_items_for_branch(&mut app, &head);

    assert!(
        !items.contains(&CommitMenuItem::Push),
        "a HEAD branch in sync with its upstream has nothing to push, got {items:?}"
    );
    // Pull remains HEAD-gated and is unaffected by the Push change.
    assert!(
        items.contains(&CommitMenuItem::Pull),
        "Pull should still be offered on the HEAD branch, got {items:?}"
    );
}
