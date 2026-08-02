mod common;

use git2::{BranchType, Repository};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

use common::{add_bare_origin, commit_file, current_branch, git_cli, init_repo, repo_path, Seed};
use keifu::action::Action;
use keifu::app::{App, AppMode};
use keifu::config::{Config, GraphRenderer, UiState};
use keifu::git::operations::{checkout_branch, create_branch, delete_branch};
use keifu::ui::{command_palette::CommandPaletteWidget, theme::Theme};

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_action(Action::InputChar(c)).unwrap();
    }
}

fn open_palette(app: &mut App, query: &str) {
    app.handle_action(Action::OpenCommandPalette).unwrap();
    type_text(app, query);
}

fn palette_screen(app: &App, query: &str) -> String {
    let results = app.palette_results(query);
    let area = Rect::new(0, 0, 90, 20);
    let mut buffer = Buffer::empty(area);
    CommandPaletteWidget::new(query, &results.items, results.more, 0, &Theme::dark())
        .render(area, &mut buffer);
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn palette_checkout_picker_and_registry_settings_are_observable_and_persisted() {
    // Keep state/config writes from palette setting actions inside this test's
    // throwaway directory. This integration-test binary contains only this test,
    // so changing its process environment cannot race a sibling test.
    let config_home = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_CONFIG_HOME", config_home.path());

    let (_repo_dir, git_repo) = init_repo(Seed::TrackedFile);
    let path = repo_path(&git_repo).to_string();
    let repo = git_repo.repo();
    let initial = repo.head().unwrap().peel_to_commit().unwrap().id();
    let default_branch = current_branch(repo);
    create_branch(repo, "local-work", initial).unwrap();

    let _origin = add_bare_origin(&path);
    create_branch(repo, "remote-source", initial).unwrap();
    checkout_branch(repo, "remote-source").unwrap();
    commit_file(repo, "remote.txt", "remote\n", "remote work");
    git_cli(&path, &["push", "origin", "remote-source:remote-work"]);
    checkout_branch(repo, &default_branch).unwrap();
    delete_branch(repo, "remote-source").unwrap();
    git_cli(&path, &["fetch", "origin"]);
    assert!(repo.find_branch("remote-work", BranchType::Local).is_err());

    let mut app = App::from_repo(git_repo).unwrap();

    // The established command registry remains searchable.
    open_palette(&mut app, "refresh");
    assert!(app
        .palette_results("refresh")
        .items
        .iter()
        .any(|item| item.label == "Refresh"));
    app.handle_action(Action::Cancel).unwrap();

    // Checkout is a command that opens a dedicated, searchable branch picker.
    open_palette(&mut app, "checkout");
    let checkout = app.palette_results("checkout");
    assert_eq!(checkout.items[0].label, "Checkout branch…");
    app.handle_action(Action::MenuSelect).unwrap();
    assert!(matches!(app.mode, AppMode::BranchPicker { .. }));

    type_text(&mut app, "local-work");
    app.handle_action(Action::MenuSelect).unwrap();
    assert_eq!(
        Repository::open(&path).unwrap().head().unwrap().shorthand(),
        Some("local-work")
    );

    // A remote result flows through the same picker and creates a tracking
    // local branch, which is the downstream Git contract users depend on.
    open_palette(&mut app, "checkout");
    app.handle_action(Action::MenuSelect).unwrap();
    type_text(&mut app, "origin/remote-work");
    app.handle_action(Action::MenuSelect).unwrap();
    let reopened = Repository::open(&path).unwrap();
    assert_eq!(reopened.head().unwrap().shorthand(), Some("remote-work"));
    let tracking = reopened
        .find_branch("remote-work", BranchType::Local)
        .unwrap()
        .upstream()
        .unwrap();
    assert_eq!(tracking.name().unwrap(), Some("origin/remote-work"));
    drop(tracking);
    drop(reopened);

    // The rendered palette shows the current setting value. Selecting the row
    // updates the live app and leaves the row open with its new value visible.
    open_palette(&mut app, "diff line wrap");
    let before = palette_screen(&app, "diff line wrap");
    assert!(before.contains("Diff line wrap"), "screen was:\n{before}");
    assert!(
        before.contains("setting Toggle Diff line wrap"),
        "setting tag and action must remain visually separated:\n{before}"
    );
    assert!(before.contains("Off"), "screen was:\n{before}");
    app.handle_action(Action::MenuSelect).unwrap();
    assert!(app.diff_word_wrap);
    assert!(matches!(app.mode, AppMode::CommandPalette { .. }));
    let after = palette_screen(&app, "diff line wrap");
    assert!(after.contains("On"), "screen was:\n{after}");
    assert!(UiState::load().diff_word_wrap);

    // Graph renderer proves config-backed enum cycling uses the same registry
    // path and persists its restart-time value.
    open_palette(&mut app, "graph renderer");
    let renderer_before = palette_screen(&app, "graph renderer");
    assert!(renderer_before.contains("Graph renderer"));
    assert!(renderer_before.contains("auto"));
    app.handle_action(Action::MenuSelect).unwrap();
    assert_eq!(app.config.ui.graph_renderer, GraphRenderer::Unicode);
    let renderer_after = palette_screen(&app, "graph renderer");
    assert!(renderer_after.contains("unicode"));
    assert_eq!(Config::load().ui.graph_renderer, GraphRenderer::Unicode);

    std::env::remove_var("XDG_CONFIG_HOME");
}
