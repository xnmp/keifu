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
    let mut terminal = Terminal::new(TestBackend::new(120, 100)).unwrap();
    let theme = Theme::dark();
    terminal
        .draw(|frame| {
            frame.render_widget(
                HelpPopup::with_keymap(false, true, &theme, 0, &keymap),
                Rect::new(0, 0, 120, 100),
            );
        })
        .unwrap();
    let help = terminal
        .backend()
        .buffer()
        .content()
        .chunks(120)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(help.lines().any(|line| {
        line.contains("Ctrl+Alt+F2 / F6") && line.contains("Pull (fetch + integrate)")
    }));
    assert!(help
        .lines()
        .any(|line| line.contains("Unassigned") && line.contains("Fetch from remote")));
}
