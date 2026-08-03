mod common;

use std::{fs, path::Path};

use common::{init_repo, Seed};
use git2::{Oid, Repository, Signature};
use keifu::action::Action;

fn commit_as(
    repo: &Repository,
    path: &str,
    contents: &str,
    message: &str,
    name: &str,
    email: &str,
) -> Oid {
    let workdir = repo.workdir().unwrap();
    if let Some(parent) = workdir.join(path).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(workdir.join(path), contents).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(path)).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::now(name, email).unwrap();
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parents,
    )
    .unwrap()
}

#[test]
fn scoped_filters_match_observables_and_retain_ancestry_after_refresh() {
    let (_td, repo) = init_repo(Seed::Empty);
    let git = repo.repo();
    commit_as(
        git,
        "src/parser.rs",
        "base",
        "Initial parser",
        "Alice Example",
        "alice@example.com",
    );
    commit_as(
        git,
        "src/parser.rs",
        "fixed",
        "Fix Parser Regression",
        "Alice Example",
        "alice@example.com",
    );
    commit_as(
        git,
        "docs/guide.md",
        "guide",
        "Fix parser notes",
        "Bob Example",
        "bob@example.com",
    );
    let mut app = keifu::app::App::from_repo(repo).unwrap();

    app.handle_action(Action::StartCommitFilter).unwrap();
    for c in "message=fix parser; author=ALICE@EXAMPLE.COM; file=src/parser.rs".chars() {
        app.handle_action(Action::CommitFilterChar(c)).unwrap();
    }
    let messages: Vec<_> = app
        .visible_commit_indices
        .iter()
        .filter_map(|&i| app.graph_layout.nodes[i].commit.as_ref())
        .map(|c| c.message.as_str())
        .collect();
    assert!(messages.contains(&"Fix Parser Regression"));
    assert!(messages.contains(&"Initial parser"));
    assert!(!messages.contains(&"Fix parser notes"));
    let matching_node = app
        .graph_layout
        .nodes
        .iter()
        .find(|node| {
            node.commit
                .as_ref()
                .is_some_and(|commit| commit.message == "Fix Parser Regression")
        })
        .unwrap();
    let retained_ancestor = app
        .graph_layout
        .nodes
        .iter()
        .find(|node| {
            node.commit
                .as_ref()
                .is_some_and(|commit| commit.message == "Initial parser")
        })
        .unwrap();
    assert!(app.node_passes_commit_filter(matching_node));
    assert!(!app.node_passes_commit_filter(retained_ancestor));
    let selected = app
        .graph_nav
        .selected_node(&app.graph_layout)
        .unwrap()
        .commit
        .as_ref()
        .unwrap()
        .oid;
    app.refresh(true).unwrap();
    assert_eq!(
        app.graph_nav
            .selected_node(&app.graph_layout)
            .unwrap()
            .commit
            .as_ref()
            .unwrap()
            .oid,
        selected
    );
}

#[test]
fn path_is_case_sensitive_while_author_name_is_case_insensitive() {
    let (_td, repo) = init_repo(Seed::Empty);
    let git = repo.repo();
    commit_as(
        git,
        "src/Parser.rs",
        "one",
        "Refactor code",
        "Ada Lovelace",
        "ada@example.com",
    );
    let mut app = keifu::app::App::from_repo(repo).unwrap();
    app.handle_action(Action::StartCommitFilter).unwrap();
    for c in "author=LOVELACE; file=src/parser.rs".chars() {
        app.handle_action(Action::CommitFilterChar(c)).unwrap();
    }
    assert!(app.visible_commit_indices.is_empty());
    app.handle_action(Action::Cancel).unwrap();
    app.handle_action(Action::StartCommitFilter).unwrap();
    for c in "author=LOVELACE; file=Parser.rs".chars() {
        app.handle_action(Action::CommitFilterChar(c)).unwrap();
    }
    assert_eq!(
        app.visible_commit_indices
            .iter()
            .filter(|&&i| app.graph_layout.nodes[i].commit.is_some())
            .count(),
        1
    );
}
