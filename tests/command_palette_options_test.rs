mod common;

use git2::{BranchType, Repository};
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

use common::{add_bare_origin, commit_file, current_branch, git_cli, init_repo, repo_path, Seed};
use keifu::action::Action;
use keifu::app::{App, AppMode};
use keifu::config::{Config, GraphRenderer, UiState};
use keifu::git::operations::{checkout_branch, create_branch, delete_branch};
use keifu::palette::filter_checkout_branches;
use keifu::toast::ToastKind;
use keifu::ui::dialog::BranchPickerWidget;
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

fn branch_picker_screen(app: &App) -> String {
    let AppMode::BranchPicker { branches, selected } = &app.mode else {
        panic!("expected checkout picker, got {:?}", app.mode);
    };
    let labels: Vec<String> = filter_checkout_branches(branches, app.checkout_picker_query())
        .into_iter()
        .map(|branch| {
            if branch.is_remote {
                format!("remote {}", branch.name)
            } else {
                branch.name.clone()
            }
        })
        .collect();
    let area = Rect::new(0, 0, 42, 8);
    let mut buffer = Buffer::empty(area);
    BranchPickerWidget::new(
        &labels,
        app.checkout_picker_query(),
        *selected,
        &Theme::dark(),
    )
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
    std::env::set_var("KEIFU_CONFIG_DIR", config_home.path());

    let (_repo_dir, git_repo) = init_repo(Seed::TrackedFile);
    let path = repo_path(&git_repo).to_string();
    let repo = git_repo.repo();
    let initial = repo.head().unwrap().peel_to_commit().unwrap().id();
    let default_branch = current_branch(repo);
    create_branch(repo, "local-work", initial).unwrap();
    create_branch(repo, "vanishing", initial).unwrap();
    for i in 0..16 {
        create_branch(repo, &format!("topic-{i:02}"), initial).unwrap();
    }

    let _origin = add_bare_origin(&path);
    create_branch(repo, "remote-source", initial).unwrap();
    checkout_branch(repo, "remote-source").unwrap();
    commit_file(repo, "remote.txt", "remote\n", "remote work");
    git_cli(&path, &["push", "origin", "remote-source:remote-work"]);
    checkout_branch(repo, &default_branch).unwrap();
    delete_branch(repo, "remote-source").unwrap();
    git_cli(&path, &["fetch", "origin"]);
    // Git permits this local name even though the same display name is also a
    // remote-tracking ref. The picker must retain both authoritative identities.
    create_branch(repo, "origin/remote-work", initial).unwrap();
    assert!(repo.find_branch("remote-work", BranchType::Local).is_err());

    let mut app = App::from_repo(git_repo).unwrap();

    // The graph's existing Checkout action preserves the same authoritative
    // identity when a local branch looks like a remote-tracking ref.
    for _ in 0..app.graph_layout.nodes.len() {
        let labels = app.selected_node_branches();
        if labels.contains(&"origin/remote-work") && labels.contains(&"topic-00") {
            break;
        }
        app.handle_action(Action::MoveDown).unwrap();
    }
    let selected_labels = app.selected_node_branches();
    assert!(selected_labels.contains(&"origin/remote-work"));
    assert!(selected_labels.contains(&"topic-00"));
    app.handle_action(Action::OpenCommitMenu).unwrap();
    type_text(&mut app, "checkout");
    app.handle_action(Action::MenuSelect).unwrap();
    type_text(&mut app, "origin/remote-work");
    let graph_matches = match &app.mode {
        AppMode::BranchPicker { branches, .. } => {
            filter_checkout_branches(branches, app.checkout_picker_query())
        }
        other => panic!("expected graph checkout picker, got {other:?}"),
    };
    assert_eq!(graph_matches.len(), 1);
    assert!(!graph_matches[0].is_remote);
    app.handle_action(Action::MenuSelect).unwrap();
    assert!(matches!(app.mode, AppMode::Normal));
    assert_eq!(
        Repository::open(&path).unwrap().head().unwrap().shorthand(),
        Some("origin/remote-work"),
        "graph checkout must not reinterpret a local slash-name as remote"
    );

    // Existing commands remain keyboard-navigable, searchable, and executable.
    // Create a ref behind the app's back so Refresh has a user-visible effect.
    let external_repo = Repository::open(&path).unwrap();
    create_branch(&external_repo, "fresh-after-start", initial).unwrap();
    assert!(!app
        .branches
        .iter()
        .any(|branch| branch.name == "fresh-after-start"));
    open_palette(&mut app, "");
    app.handle_action(Action::MoveDown).unwrap();
    assert!(matches!(
        app.mode,
        AppMode::CommandPalette { selected: 1, .. }
    ));
    app.handle_action(Action::MoveUp).unwrap();
    type_text(&mut app, "refresh");
    assert!(app
        .palette_results("refresh")
        .items
        .iter()
        .any(|item| item.label == "Refresh"));
    app.handle_action(Action::MenuSelect).unwrap();
    assert!(matches!(app.mode, AppMode::Normal));
    assert!(app
        .branches
        .iter()
        .any(|branch| branch.name == "fresh-after-start"));

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
    let remote_index = match &app.mode {
        AppMode::BranchPicker { branches, .. } => {
            let matching = filter_checkout_branches(branches, app.checkout_picker_query());
            assert_eq!(matching.len(), 2, "local and remote collision must survive");
            matching.iter().position(|branch| branch.is_remote).unwrap()
        }
        other => panic!("expected checkout picker, got {other:?}"),
    };
    for _ in 0..remote_index {
        app.handle_action(Action::MoveDown).unwrap();
    }
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

    // Navigation windows around the selected result instead of moving the
    // highlight beyond the visible viewport.
    open_palette(&mut app, "checkout");
    app.handle_action(Action::MenuSelect).unwrap();
    type_text(&mut app, "topic");
    for _ in 0..12 {
        app.handle_action(Action::MoveDown).unwrap();
    }
    let picker = branch_picker_screen(&app);
    assert!(
        picker.contains("> topic-12"),
        "selected fuzzy result must remain visible:\n{picker}"
    );
    app.handle_action(Action::Cancel).unwrap();

    // A stale result exercises the real event-loop error composition: the
    // picker closes, the error becomes a red toast, and input remains usable.
    open_palette(&mut app, "checkout");
    app.handle_action(Action::MenuSelect).unwrap();
    type_text(&mut app, "vanishing");
    Repository::open(&path)
        .unwrap()
        .find_branch("vanishing", BranchType::Local)
        .unwrap()
        .delete()
        .unwrap();
    app.dispatch_action(Action::MenuSelect);
    assert!(matches!(app.mode, AppMode::Normal));
    let error_toast = app.toasts.visible().last().expect("checkout error toast");
    assert_eq!(error_toast.kind, ToastKind::Error);
    assert!(error_toast.text.contains("vanishing"));

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

    // State-backed settings added to the registry are projected automatically,
    // including status-bar visibility, and persist through the same action.
    open_palette(&mut app, "show status bar");
    let status_before = palette_screen(&app, "show status bar");
    assert!(status_before.contains("Show status bar"));
    assert!(status_before.contains("On"), "screen was:\n{status_before}");
    app.handle_action(Action::MenuSelect).unwrap();
    assert!(!app.status_bar_visible);
    let status_after = palette_screen(&app, "show status bar");
    assert!(status_after.contains("Off"), "screen was:\n{status_after}");
    assert!(!UiState::load().status_bar_visible);

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

    // Reconstruct the application through its production startup path and prove
    // the reopened user-facing palette reports the persisted values.
    drop(app);
    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&path).unwrap();
    let mut reloaded = App::new().unwrap();
    std::env::set_current_dir(original_dir).unwrap();
    open_palette(&mut reloaded, "diff line wrap");
    let persisted_wrap = palette_screen(&reloaded, "diff line wrap");
    assert!(
        persisted_wrap.contains("On"),
        "screen was:\n{persisted_wrap}"
    );
    open_palette(&mut reloaded, "show status bar");
    assert!(!reloaded.status_bar_visible);
    let persisted_status = palette_screen(&reloaded, "show status bar");
    assert!(
        persisted_status.contains("Off"),
        "screen was:\n{persisted_status}"
    );
    open_palette(&mut reloaded, "graph renderer");
    let persisted_renderer = palette_screen(&reloaded, "graph renderer");
    assert!(
        persisted_renderer.contains("unicode"),
        "screen was:\n{persisted_renderer}"
    );

    std::env::remove_var("KEIFU_CONFIG_DIR");
}
