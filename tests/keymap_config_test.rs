use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use keifu::action::Action;
use keifu::app::{AppMode, FocusedPanel};
use keifu::config::Config;
use keifu::keybindings::map_key_to_action_with_keymap;
use keifu::keymap::{KeyBinding, ResolvedKeymap};

fn resolved(source: &str) -> ResolvedKeymap {
    let config: Config = toml::from_str(source).expect("config parses");
    ResolvedKeymap::from_table(&config.keymap)
}

fn graph(keymap: &ResolvedKeymap, key: KeyEvent) -> Option<Action> {
    map_key_to_action_with_keymap(
        key,
        &AppMode::Normal,
        FocusedPanel::Graph,
        false,
        false,
        false,
        keymap,
    )
}

#[test]
fn parses_and_formats_supported_single_key_bindings() {
    for (source, code, modifiers, formatted) in [
        (
            "Ctrl+Alt+Shift+F2",
            KeyCode::F(2),
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
            "Ctrl+Alt+Shift+F2",
        ),
        ("Alt+/", KeyCode::Char('/'), KeyModifiers::ALT, "Alt+/"),
        (
            "Shift+G",
            KeyCode::Char('G'),
            KeyModifiers::SHIFT,
            "Shift+G",
        ),
        (
            "PageDown",
            KeyCode::PageDown,
            KeyModifiers::NONE,
            "PageDown",
        ),
    ] {
        let binding = source.parse::<KeyBinding>().expect(source);
        assert_eq!(binding.code, code);
        assert_eq!(binding.modifiers, modifiers);
        assert_eq!(binding.to_string(), formatted);
    }
    assert!("Ctrl++P".parse::<KeyBinding>().is_err());
    assert!("Ctrl+K Ctrl+C".parse::<KeyBinding>().is_err());
    assert!("Hyper+P".parse::<KeyBinding>().is_err());
}

#[test]
fn no_keymap_preserves_defaults_and_overrides_replace_them() {
    let defaults = resolved("");
    assert_eq!(
        graph(
            &defaults,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)
        ),
        Some(Action::Pull)
    );

    let custom = resolved("[keymap]\npull = [\"Ctrl+Alt+F2\", \"F6\"]\n");
    assert_eq!(
        graph(
            &custom,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)
        ),
        None
    );
    for key in [
        KeyEvent::new(KeyCode::F(2), KeyModifiers::CONTROL | KeyModifiers::ALT),
        KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE),
    ] {
        assert_eq!(graph(&custom, key), Some(Action::Pull));
    }
    assert_eq!(custom.display_bindings("pull"), "Ctrl+Alt+F2 / F6");
}

#[test]
fn an_empty_list_unassigns_the_action() {
    let keymap = resolved("[keymap]\npull = []\n");
    assert_eq!(
        graph(
            &keymap,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)
        ),
        None
    );
    assert_eq!(keymap.display_bindings("pull"), "Unassigned");
}

#[test]
fn overrides_apply_in_normal_panel_and_modal_contexts() {
    let keymap = resolved(
        "[keymap]\nopen-commit-menu = [\"F2\"]\ntoggle-stage = [\"F3\"]\nmenu-select = [\"F4\"]\n",
    );
    assert_eq!(
        graph(&keymap, KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)),
        Some(Action::OpenCommitMenu)
    );
    assert_eq!(
        map_key_to_action_with_keymap(
            KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE),
            &AppMode::Normal,
            FocusedPanel::Files,
            false,
            false,
            false,
            &keymap,
        ),
        Some(Action::ToggleStage)
    );
    let picker = AppMode::BranchPicker {
        branches: vec![],
        selected: 0,
    };
    assert_eq!(
        map_key_to_action_with_keymap(
            KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE),
            &picker,
            FocusedPanel::Graph,
            false,
            false,
            false,
            &keymap,
        ),
        Some(Action::MenuSelect)
    );
}

#[test]
fn same_context_conflict_is_last_wins_but_cross_context_reuse_is_allowed() {
    let conflicting = resolved("[keymap]\nfetch = [\"F2\"]\npull = [\"F2\"]\n");
    assert_eq!(
        graph(
            &conflicting,
            KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)
        ),
        Some(Action::Pull)
    );
    let warning = conflicting
        .warnings()
        .iter()
        .find(|w| w.reason.contains("conflict"))
        .expect("conflict warning");
    assert!(warning.reason.contains("fetch"));
    assert!(warning.reason.contains("pull"));
    assert!(warning.reason.contains("F2"));

    let disjoint = resolved("[keymap]\npull = [\"F3\"]\ntoggle-stage = [\"F3\"]\n");
    assert!(disjoint.warnings().is_empty());

    let reversed = resolved("[keymap]\npull = [\"F2\"]\nfetch = [\"F2\"]\n");
    assert_eq!(
        graph(&reversed, KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)),
        Some(Action::Fetch),
        "source order, rather than identifier sort order, determines the winner"
    );
}

#[test]
fn malformed_unknown_and_alias_entries_recover_independently() {
    let keymap = resolved(
        "[keymap]\npull = [\"Ctrl++P\"]\nnot-a-command = [\"F9\"]\nfetch = [\"F2\"]\ncommand-palette = [\"F4\"]\n",
    );
    assert_eq!(
        graph(
            &keymap,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)
        ),
        Some(Action::Pull),
        "malformed override keeps the action default"
    );
    assert_eq!(
        graph(&keymap, KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)),
        Some(Action::Fetch),
        "other valid overrides still apply"
    );
    assert_eq!(
        graph(&keymap, KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE)),
        Some(Action::OpenCommandPalette),
        "compatibility alias resolves"
    );
    let text = keymap
        .warnings()
        .iter()
        .map(|w| format!("{}: {}", w.entry, w.reason))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("pull"));
    assert!(text.contains("Ctrl++P"));
    assert!(text.contains("not-a-command"));
    assert!(text.contains("unknown action"));
}
