//! Command-palette integration coverage for contextual Enter-menu actions.

mod common;

use std::fs;

use git2::{Repository, Signature};
use keifu::action::Action;
use keifu::app::{App, AppMode, InputAction};
use keifu::git::GitRepository;

use common::{commit_file, init_repo, Seed};

fn make_app(repo: GitRepository) -> App {
    App::from_repo(repo).unwrap()
}

#[test]
fn command_palette_exposes_and_dispatches_available_commit_actions() {
    let (_td, repo) = init_repo(Seed::Empty);
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = make_app(repo);

    // Every action the normal Enter menu exposes is directly discoverable in
    // the command palette, not hidden behind a second menu.
    app.handle_action(Action::OpenCommitMenu).unwrap();
    let expected: Vec<String> = match &app.mode {
        AppMode::CommitMenu { items, .. } => {
            items.iter().map(|item| item.label().to_string()).collect()
        }
        other => panic!("expected Enter menu, got {other:?}"),
    };
    app.handle_action(Action::Cancel).unwrap();
    for expected in expected {
        let labels: Vec<String> = app
            .palette_results(&expected)
            .items
            .into_iter()
            .map(|item| item.label)
            .collect();
        assert!(
            labels.iter().any(|label| label == &expected),
            "{expected}: {labels:?}"
        );
    }

    app.handle_action(Action::OpenCommandPalette).unwrap();
    for c in "Cherry-pick".chars() {
        app.handle_action(Action::InputChar(c)).unwrap();
    }
    app.handle_action(Action::MenuSelect).unwrap();
    assert!(
        matches!(app.mode, AppMode::Confirm { ref message, .. } if message.starts_with("Cherry-pick commit")),
        "palette dispatch must use the existing Cherry-pick confirmation, got {:?}",
        app.mode
    );

    // Filtering resets the cursor, and arrow navigation remains constrained
    // to the matching rows rather than a stale unfiltered index.
    app.handle_action(Action::Cancel).unwrap();
    app.handle_action(Action::OpenCommandPalette).unwrap();
    for c in "branch".chars() {
        app.handle_action(Action::InputChar(c)).unwrap();
    }
    let second_visible_label = app.palette_results("branch").items[1].label.clone();
    app.handle_action(Action::MoveDown).unwrap();
    assert!(
        matches!(app.mode, AppMode::CommandPalette { selected: 1, .. }),
        "filtered palette selection must advance within the visible branch rows"
    );
    app.handle_action(Action::MenuSelect).unwrap();
    assert!(
        match second_visible_label.as_str() {
            "Create branch here" => matches!(
                app.mode,
                AppMode::Input {
                    action: InputAction::CreateBranch,
                    ..
                }
            ),
            "Filter branches" => matches!(app.mode, AppMode::BranchFilter { .. }),
            "Search branches" => matches!(
                app.mode,
                AppMode::Input {
                    action: InputAction::Search,
                    ..
                }
            ),
            label if label.starts_with("Checkout ") => matches!(app.mode, AppMode::Confirm { .. }),
            other => panic!("unexpected second filtered row: {other}"),
        },
        "selected filtered row '{second_visible_label}' dispatched the wrong action"
    );
}

#[test]
fn command_palette_uses_enter_menu_preconditions_for_stashes_and_branch_tips() {
    let (td, repo) = init_repo(Seed::Empty);
    let first = commit_file(repo.repo(), "a.txt", "a", "first");
    repo.repo()
        .branch("feature", &repo.repo().find_commit(first).unwrap(), false)
        .unwrap();
    commit_file(repo.repo(), "b.txt", "b", "second");
    let mut app = make_app(repo);

    // The checked-out branch cannot be deleted; the palette must omit that
    // otherwise valid action instead of surfacing a dead-end command.
    let labels: Vec<String> = app
        .palette_results("Delete branch")
        .items
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert!(
        !labels.iter().any(|label| label == "Delete branch"),
        "{labels:?}"
    );

    fs::write(td.path().join("a.txt"), "stashed").unwrap();
    {
        let mut git = Repository::open(td.path()).unwrap();
        let sig = Signature::now("Test User", "test@example.com").unwrap();
        git.stash_save(&sig, "wip", None).unwrap();
    }
    app.refresh(true).unwrap();
    let stash_idx = app
        .graph_layout
        .nodes
        .iter()
        .position(|node| node.is_stash)
        .expect("a stash node is present");
    app.graph_nav.graph_list_state.select(Some(stash_idx));

    let labels: Vec<String> = app
        .palette_results("stash")
        .items
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert!(
        labels.iter().any(|label| label == "Apply stash"),
        "{labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|label| label == "Pop stash (apply + drop)"),
        "{labels:?}"
    );
    assert!(
        !labels.iter().any(|label| label == "Cherry-pick"),
        "{labels:?}"
    );

    // A stash has a commit payload, but is not an ordinary commit context.
    // Registry shortcuts must not leak alongside the dedicated stash actions.
    for ordinary in [
        "Commit actions menu",
        "Create branch here",
        "Mark commit for compare",
        "Jump to merge base with main",
    ] {
        let labels: Vec<String> = app
            .palette_results(ordinary)
            .items
            .into_iter()
            .map(|item| item.label)
            .collect();
        assert!(
            !labels.iter().any(|label| label == ordinary),
            "stash palette leaked {ordinary}: {labels:?}"
        );
    }

    fs::write(td.path().join("b.txt"), "uncommitted").unwrap();
    app.refresh(true).unwrap();
    let uncommitted_idx = app
        .graph_layout
        .nodes
        .iter()
        .position(|node| node.is_uncommitted)
        .expect("an uncommitted node is present");
    app.graph_nav.graph_list_state.select(Some(uncommitted_idx));

    // Enter switches this selection to Files; the palette must similarly omit
    // ordinary commit actions rather than offering dead ends.
    for ordinary in [
        "Checkout",
        "Cherry-pick",
        "Reset to this commit...",
        "Revert this commit",
        "Copy commit hash",
    ] {
        let labels: Vec<String> = app
            .palette_results(ordinary)
            .items
            .into_iter()
            .map(|item| item.label)
            .collect();
        assert!(
            !labels.iter().any(|label| label == ordinary),
            "uncommitted palette leaked {ordinary}: {labels:?}"
        );
    }
}
