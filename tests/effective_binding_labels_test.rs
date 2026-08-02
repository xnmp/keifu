mod common;

use common::{commit_file, init_repo, Seed};
use keifu::app::App;
use keifu::keymap::ResolvedKeymap;
use keifu::ui::help_popup::HelpPopup;
use keifu::ui::theme::Theme;
use ratatui::{backend::TestBackend, layout::Rect, Terminal};

fn configured_keymap() -> ResolvedKeymap {
    let table = "pull = [\"Ctrl+Alt+F2\", \"F6\"]\nfetch = []"
        .parse::<toml::Table>()
        .unwrap();
    ResolvedKeymap::from_table(&table)
}

fn rendered_help(keymap: &ResolvedKeymap) -> String {
    let mut terminal = Terminal::new(TestBackend::new(120, 100)).unwrap();
    let theme = Theme::dark();
    terminal
        .draw(|frame| {
            frame.render_widget(
                HelpPopup::with_keymap(false, true, &theme, 0, keymap),
                Rect::new(0, 0, 120, 100),
            );
        })
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(120)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn command_palette_labels_use_effective_bindings() {
    let (_td, repo) = init_repo(Seed::Empty);
    commit_file(repo.repo(), "a.txt", "a", "first");
    let mut app = App::from_repo(repo).unwrap();
    app.keymap = configured_keymap();

    let pull = app
        .palette_results("Pull")
        .items
        .into_iter()
        .find(|item| item.label == "Pull")
        .unwrap();
    assert_eq!(pull.hint.as_deref(), Some("Ctrl+Alt+F2 / F6"));

    let fetch = app
        .palette_results("Fetch")
        .items
        .into_iter()
        .find(|item| item.label == "Fetch")
        .unwrap();
    assert_eq!(fetch.hint.as_deref(), Some("Unassigned"));
}

#[test]
fn help_popup_renders_effective_multiple_and_unassigned_bindings() {
    let keymap = configured_keymap();
    let help = rendered_help(&keymap);

    assert!(help.lines().any(|line| {
        line.contains("Ctrl+Alt+F2 / F6") && line.contains("Pull (fetch + integrate)")
    }));
    assert!(help
        .lines()
        .any(|line| line.contains("Unassigned") && line.contains("Fetch from remote")));
}

#[test]
fn help_navigation_rows_render_every_effective_action_binding() {
    let table = r#"
move-up = ["F2"]
move-down = ["F3"]
panel-left = ["F4"]
panel-right = ["F5"]
page-down = ["F6"]
page-up = ["F7"]
go-to-top = ["F8"]
go-to-bottom = ["F9"]
jump-to-head = ["F10"]
focus-graph = ["F11"]
stop-editing = ["F12"]
quit = ["F13"]
"#
    .parse::<toml::Table>()
    .unwrap();
    let help = rendered_help(&ResolvedKeymap::from_table(&table));

    for (description, effective) in [
        ("Move up/down", "F2 / F3"),
        ("Switch panels", "F4 / F5"),
        ("Page down/up", "F6 / F7"),
        ("Go to top", "F8"),
        ("Go to bottom", "F9"),
        ("Jump to HEAD", "F10"),
        (
            "Return to graph / stop editing / quit (from graph)",
            "F11 / F12 / F13",
        ),
    ] {
        assert!(
            help.lines()
                .any(|line| line.contains(effective) && line.contains(description)),
            "help omitted {description:?} with {effective:?}\n{help}"
        );
    }
}
