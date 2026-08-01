use keifu::ui::help_popup::HelpPopup;
use keifu::ui::theme::Theme;
use ratatui::{backend::TestBackend, layout::Rect, Terminal};

fn rendered_help(is_uncommitted: bool) -> String {
    let mut terminal = Terminal::new(TestBackend::new(120, 100)).unwrap();
    let theme = Theme::dark();
    terminal
        .draw(|frame| {
            frame.render_widget(
                HelpPopup::new(is_uncommitted, &theme, 0),
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
fn help_popup_displays_current_global_and_filter_shortcuts() {
    let help = rendered_help(false);

    for expected in [
        "Alt+I",
        "Alt+K",
        "Ctrl+P / Ctrl+Alt+P / :",
        "Ctrl+, / ,",
        "Ctrl+Shift+F",
        "Filter files",
    ] {
        assert!(help.contains(expected), "help popup omitted {expected:?}");
    }

    for (key, description) in [
        ("Ctrl+Shift+F", "Filter commits (message/author/hash)"),
        ("/", "Filter files"),
    ] {
        assert!(
            help.lines()
                .any(|line| line.contains(key) && line.contains(description)),
            "help popup mismatched {key:?} and {description:?}"
        );
    }
}

#[test]
fn help_popup_keeps_context_specific_file_entries() {
    let help = rendered_help(true);

    for expected in [
        "Stage/unstage file",
        "Accept ours (on conflicted file)",
        "Abort the in-progress operation",
    ] {
        assert!(help.contains(expected), "help popup omitted {expected:?}");
    }
}
