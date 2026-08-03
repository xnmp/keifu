use keifu::{app::App, settings, ui};
use ratatui::{backend::TestBackend, Terminal};

fn rendered_screen(app: &mut App) -> String {
    let width = 120;
    let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
    terminal.draw(|frame| ui::draw(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(width as usize)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn panel_title_setting_controls_all_panel_titles_without_removing_borders_or_content() {
    let mut app = App::test_fixture();
    assert!(
        app.panel_titles_visible,
        "panel titles are visible by default"
    );
    assert!(
        settings::descriptors()
            .iter()
            .any(|descriptor| descriptor.label == "Show panel titles"),
        "Settings must expose the shared panel-title control"
    );

    let visible = rendered_screen(&mut app);
    for title in ["Commits", "Changed Files", "Commit Detail"] {
        assert!(
            visible.contains(title),
            "the default TUI must render the {title:?} title:\n{visible}"
        );
    }

    app.panel_titles_visible = false;
    let hidden = rendered_screen(&mut app);
    for title in ["Commits", "Changed Files", "Commit Detail"] {
        assert!(
            !hidden.contains(title),
            "disabling panel titles must omit {title:?}:\n{hidden}"
        );
    }
    assert!(
        hidden.contains('╭') && hidden.contains('╯'),
        "hiding titles must retain the bordered panel layout:\n{hidden}"
    );
    assert!(
        hidden.contains("empty commit"),
        "hiding titles must retain panel content:\n{hidden}"
    );
}

#[test]
fn active_commit_filter_remains_visible_when_static_panel_titles_are_hidden() {
    let mut app = App::test_fixture();
    app.panel_titles_visible = false;
    app.commit_filter_active = true;
    app.commit_filter = "author:alice".to_string();

    let rendered = rendered_screen(&mut app);
    assert!(
        rendered.contains("Commits: author:alice_"),
        "the live commit-filter prompt must remain visible when static panel titles are hidden:\n{rendered}"
    );
}
