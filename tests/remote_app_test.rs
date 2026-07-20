//! App-level tests for multi-remote resolution and remote-branch deletion.
//!
//! These exercise the deterministic parts of the flows: which mode a fetch/pull
//! lands in (picker vs. direct) and the synchronous remote-branch delete driven
//! through the Confirm dialog. The threaded network completion is covered at the
//! operations layer (`pull_remote_test`).

use keifu::action::Action;
use keifu::app::{App, AppMode, ConfirmAction, FocusedPanel, RemoteOp};
use keifu::git::operations::{create_branch, delete_branch};
use keifu::git::GitRepository;

mod common;
use common::{
    add_bare_origin, add_bare_remote, commit_file, current_branch, git_cli, head_oid, init_repo,
    Seed,
};

/// Build an App over a fresh open of the repo at `path`, focused on the graph.
fn app_at(path: &str) -> App {
    let mut app = App::from_repo(GitRepository::open(path).unwrap()).unwrap();
    app.focused_panel = FocusedPanel::Graph;
    app
}

#[test]
fn multi_remote_fetch_opens_remote_picker() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let _origin = add_bare_origin(&path);
    let _backup = add_bare_remote(&path, "backup");

    let mut app = app_at(&path);
    app.handle_action(Action::Fetch).unwrap();

    match &app.mode {
        AppMode::RemotePicker { remotes, op, .. } => {
            assert_eq!(*op, RemoteOp::Fetch);
            let mut r = remotes.clone();
            r.sort();
            assert_eq!(r, vec!["backup".to_string(), "origin".to_string()]);
        }
        other => panic!("expected a RemotePicker, got {other:?}"),
    }
}

#[test]
fn single_remote_fetch_skips_picker() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let _origin = add_bare_origin(&path);

    let mut app = app_at(&path);
    app.handle_action(Action::Fetch).unwrap();

    // A lone remote fetches straight away — no prompt.
    assert!(
        !matches!(app.mode, AppMode::RemotePicker { .. }),
        "single-remote fetch must not prompt"
    );
    assert!(app.is_fetching(), "fetch should have started in the background");
}

#[test]
fn pull_without_remote_reports_message() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let mut app = App::from_repo(git_repo).unwrap();
    app.focused_panel = FocusedPanel::Graph;

    app.handle_action(Action::Pull).unwrap();

    assert!(matches!(app.mode, AppMode::Normal));
    let toast_texts: Vec<&str> = app.toasts.visible().iter().map(|t| t.text.as_str()).collect();
    assert!(
        toast_texts.iter().any(|t| t.contains("No remote")),
        "expected a no-remote toast, got {toast_texts:?}"
    );
}

#[test]
fn delete_remote_branch_flow_removes_branch_from_remote() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let c0 = head_oid(git_repo.repo());
    let branch = current_branch(git_repo.repo());
    let _origin = add_bare_origin(&path);

    // Publish a feature branch, then drop the local copy so the node at c0
    // carries only the HEAD branch and the remote-tracking `origin/feature`.
    create_branch(git_repo.repo(), "feature", c0).unwrap();
    git_cli(&path, &["push", "origin", "feature"]);
    git_cli(&path, &["fetch", "origin"]);
    delete_branch(git_repo.repo(), "feature").unwrap();

    let mut app = app_at(&path);

    // Select the node carrying the origin/feature label.
    let pos = app
        .graph_nav
        .branch_positions
        .iter()
        .position(|(_, n)| n == "origin/feature")
        .expect("origin/feature should be a graph ref");
    let (node_idx, _) = app.graph_nav.branch_positions[pos];
    app.graph_nav.graph_list_state.select(Some(node_idx));
    app.graph_nav.selected_branch_position = Some(pos);

    // `d` opens the delete flow; the only deletable ref is the remote branch.
    app.handle_action(Action::DeleteBranch).unwrap();
    match &app.mode {
        AppMode::Confirm { action, .. } => assert!(
            matches!(action, ConfirmAction::DeleteRemoteBranch { remote, branch }
                if remote == "origin" && branch == "feature"),
            "expected a DeleteRemoteBranch confirm, got {action:?}"
        ),
        other => panic!("expected a Confirm dialog, got {other:?}"),
    }

    // Confirming performs the remote delete synchronously.
    app.handle_action(Action::Confirm).unwrap();

    assert!(
        git_cli(&path, &["ls-remote", "--heads", "origin", "feature"]).trim().is_empty(),
        "feature should be deleted from origin"
    );
    // HEAD branch is untouched.
    assert_eq!(current_branch(app.repo.repo()), branch);
}

/// End-to-end: the show/hide-remotes toggle (Shift+O → `ToggleRemoteBranches`)
/// removes a remote-only branch's exclusive commit from the graph — not just
/// its label — while leaving local work in place, and restores it when toggled
/// back.
#[test]
fn toggle_remote_branches_hides_remote_only_commits() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let base = current_branch(git_repo.repo());

    // Craft a commit that is reachable ONLY through a remote ref: branch off,
    // commit, publish that commit under refs/remotes/origin/*, then delete the
    // local branch so nothing local points at it.
    git_cli(&path, &["checkout", "-b", "tmp"]);
    let remote_oid = commit_file(git_repo.repo(), "remote-only.txt", "remote\n", "remote only work");
    git_cli(
        &path,
        &["update-ref", "refs/remotes/origin/agent-work", &remote_oid.to_string()],
    );
    git_cli(&path, &["checkout", &base]);
    git_cli(&path, &["branch", "-D", "tmp"]);
    let local_oid = head_oid(git_repo.repo());

    let mut app = app_at(&path);
    let shows_remote = |app: &App| app.commits.iter().any(|c| c.oid == remote_oid);
    let shows_local = |app: &App| app.commits.iter().any(|c| c.oid == local_oid);

    // Default: remotes shown, so the remote-only commit is in the graph.
    assert!(!app.hide_remote_branches);
    assert!(shows_remote(&app), "remote-only commit should show by default");

    // Toggle hides the remote-only commit but keeps local work.
    app.handle_action(Action::ToggleRemoteBranches).unwrap();
    assert!(app.hide_remote_branches);
    assert!(!shows_remote(&app), "remote-only commit should be hidden");
    assert!(shows_local(&app), "hiding remotes must not drop local work");

    // Toggle back restores it.
    app.handle_action(Action::ToggleRemoteBranches).unwrap();
    assert!(!app.hide_remote_branches);
    assert!(shows_remote(&app), "remote-only commit should return when shown again");
}
