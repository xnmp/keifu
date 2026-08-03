mod common;

use std::{fs, path::Path};

use common::{init_repo, Seed};
use git2::{Oid, Repository, Signature};
use keifu::{action::Action, ui};
use ratatui::{backend::TestBackend, style::Color, Terminal};

fn render_graph(app: &mut keifu::app::App) -> (String, Vec<Color>) {
    let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
    terminal.draw(|frame| ui::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let width = 120;
    let cells = buffer.content();
    let screen = cells
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    (screen, cells.iter().map(|cell| cell.fg).collect())
}

fn message_foreground(screen: &str, colors: &[Color], message: &str) -> Color {
    let offset = screen.find(message).unwrap();
    let row = screen[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count();
    let column = screen[..offset]
        .rsplit('\n')
        .next()
        .unwrap()
        .chars()
        .count();
    colors[row * 120 + column]
}

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
    let (rendered, colors) = render_graph(&mut app);
    assert!(rendered
        .contains("Commits: message=fix parser; author=ALICE@EXAMPLE.COM; file=src/parser.rs_"));
    assert_ne!(
        message_foreground(&rendered, &colors, "Fix Parser Regression"),
        message_foreground(&rendered, &colors, "Initial parser"),
        "direct matches render normally while retained ancestry is dimmed"
    );
    app.handle_action(Action::Confirm).unwrap();
    app.handle_action(Action::MoveDown).unwrap();
    app.handle_action(Action::StartCommitFilter).unwrap();
    app.handle_action(Action::Cancel).unwrap();
    assert_eq!(
        app.graph_layout.nodes.len(),
        3,
        "clearing restores every row"
    );
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

#[test]
fn changed_paths_are_loaded_only_for_file_queries_and_cached_by_commit() {
    let (_td, repo) = init_repo(Seed::Empty);
    let git = repo.repo();
    commit_as(
        git,
        "src/parser.rs",
        "one",
        "Fix parser",
        "Ada Lovelace",
        "ada@example.com",
    );
    let mut app = keifu::app::App::from_repo(repo).unwrap();

    assert!(app.commit_changed_paths.is_empty());
    app.handle_action(Action::StartCommitFilter).unwrap();
    for c in "message=fix".chars() {
        app.handle_action(Action::CommitFilterChar(c)).unwrap();
    }
    assert!(
        app.commit_changed_paths.is_empty(),
        "message-only filtering does not inspect commit trees"
    );

    app.handle_action(Action::InputClearLine).unwrap();
    for c in "file=src".chars() {
        app.handle_action(Action::CommitFilterChar(c)).unwrap();
    }
    let cached_oids: Vec<_> = app.commit_changed_paths.keys().copied().collect();
    assert!(
        !cached_oids.is_empty(),
        "a populated file clause loads changed paths"
    );
    app.handle_action(Action::CommitFilterChar('/')).unwrap();
    assert_eq!(
        app.commit_changed_paths.len(),
        cached_oids.len(),
        "later file-query keystrokes reuse the immutable OID-keyed cache"
    );
}

#[test]
fn dirty_worktree_keeps_the_uncommitted_connector_without_restoring_unrelated_history() {
    let (_td, repo) = init_repo(Seed::Empty);
    let git = repo.repo();
    commit_as(
        git,
        "src/base.rs",
        "base",
        "Initial parser",
        "Alice Example",
        "alice@example.com",
    );
    commit_as(
        git,
        "src/matching.rs",
        "match",
        "Target parser change",
        "Alice Example",
        "alice@example.com",
    );
    commit_as(
        git,
        "docs/unrelated.md",
        "unrelated",
        "Unrelated documentation",
        "Bob Example",
        "bob@example.com",
    );
    commit_as(
        git,
        "src/head.rs",
        "head",
        "Current head work",
        "Bob Example",
        "bob@example.com",
    );
    fs::write(git.workdir().unwrap().join("scratch.txt"), "dirty").unwrap();

    let mut app = keifu::app::App::from_repo(repo).unwrap();
    app.handle_action(Action::StartCommitFilter).unwrap();
    for c in "file=src/matching.rs".chars() {
        app.handle_action(Action::CommitFilterChar(c)).unwrap();
    }

    assert!(app.has_uncommitted_node());
    let messages: Vec<_> = app
        .graph_layout
        .nodes
        .iter()
        .filter_map(|node| node.commit.as_ref())
        .map(|commit| commit.message.as_str())
        .collect();
    assert!(messages.contains(&"Target parser change"));
    assert!(messages.contains(&"Initial parser"));
    assert!(messages.contains(&"Current head work"));
    assert!(!messages.contains(&"Unrelated documentation"));

    app.handle_action(Action::GoToTop).unwrap();
    assert!(
        app.graph_nav
            .selected_node(&app.graph_layout)
            .is_some_and(|node| node.is_uncommitted),
        "filtered navigation retains the working-tree staging row"
    );
    // Editing an active query rebuilds the graph. The selection must not be
    // clamped away from the staging row during that rebuild.
    app.handle_action(Action::CommitFilterChar(' ')).unwrap();
    app.handle_action(Action::CommitFilterBackspace).unwrap();
    assert!(
        app.graph_nav
            .selected_node(&app.graph_layout)
            .is_some_and(|node| node.is_uncommitted),
        "a graph rebuild preserves an uncommitted-row selection under a filter"
    );
}
