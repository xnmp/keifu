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

#[test]
fn full_update_override_remains_active_in_editor_and_filter_states() {
    let keymap = resolved("[keymap]\nfull-update = [\"F8\"]\n");
    for (panel, editing, files_filter, commit_filter) in [
        (FocusedPanel::CommitDetail, true, false, false),
        (FocusedPanel::Files, false, true, false),
        (FocusedPanel::Graph, false, false, true),
    ] {
        let map = |key| {
            map_key_to_action_with_keymap(
                key,
                &AppMode::Normal,
                panel,
                editing,
                files_filter,
                commit_filter,
                &keymap,
            )
        };
        assert_eq!(
            map(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE)),
            Some(Action::FullUpdate)
        );
        assert_eq!(map(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE)), None);
    }
}

#[test]
fn filter_backspace_actions_can_be_replaced_without_leaving_the_defaults_active() {
    let keymap = resolved(
        "[keymap]\ncommit-filter-backspace = [\"F7\"]\nfiles-filter-backspace = [\"F8\"]\n",
    );
    for (panel, files_filter, commit_filter, key, expected) in [
        (
            FocusedPanel::Graph,
            false,
            true,
            KeyCode::F(7),
            Action::CommitFilterBackspace,
        ),
        (
            FocusedPanel::Files,
            true,
            false,
            KeyCode::F(8),
            Action::FilesFilterBackspace,
        ),
    ] {
        let map = |key| {
            map_key_to_action_with_keymap(
                key,
                &AppMode::Normal,
                panel,
                false,
                files_filter,
                commit_filter,
                &keymap,
            )
        };
        assert_eq!(map(KeyEvent::new(key, KeyModifiers::NONE)), Some(expected));
        assert_eq!(
            map(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
            None
        );
    }
}

#[test]
fn registry_defaults_match_real_alternatives_and_detect_default_conflicts() {
    let defaults = resolved("");
    assert_eq!(defaults.display_bindings("toggle-stage"), "s / a");
    assert_eq!(
        defaults.display_bindings("editor-newline"),
        "Shift+Enter / Alt+Enter"
    );
    assert_eq!(
        defaults.display_bindings("go-to-top"),
        "g / Home / Ctrl+Home"
    );
    assert_eq!(defaults.display_bindings("editor-kill-line"), "Ctrl+U");

    let collision = resolved("[keymap]\neditor-delete-word = [\"Ctrl+U\"]\n");
    assert!(collision.warnings().iter().any(|warning| {
        warning.reason.contains("editor-kill-line")
            && warning.reason.contains("editor-delete-word")
            && warning.reason.contains("Ctrl+U")
    }));
}

#[test]
fn canonical_entry_replaces_an_earlier_alias_in_routing_and_labels() {
    let keymap =
        resolved("[keymap]\ncommand-palette = [\"F2\"]\nopen-command-palette = [\"F3\"]\n");
    assert_eq!(
        graph(&keymap, KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        graph(&keymap, KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE)),
        Some(Action::OpenCommandPalette)
    );
    assert_eq!(keymap.display_bindings("open-command-palette"), "F3");
}

#[test]
fn shared_text_commands_remain_remappable_inside_both_filters() {
    let keymap = resolved(
        "[keymap]\ncancel = [\"F2\"]\nconfirm = [\"F3\"]\ninput-backspace-word = [\"F4\"]\ninput-clear-line = [\"F6\"]\n",
    );
    for (panel, files_filter, commit_filter) in [
        (FocusedPanel::Graph, false, true),
        (FocusedPanel::Files, true, false),
    ] {
        let map = |code| {
            map_key_to_action_with_keymap(
                KeyEvent::new(code, KeyModifiers::NONE),
                &AppMode::Normal,
                panel,
                false,
                files_filter,
                commit_filter,
                &keymap,
            )
        };
        assert_eq!(map(KeyCode::F(2)), Some(Action::Cancel));
        assert_eq!(map(KeyCode::F(3)), Some(Action::Confirm));
        assert_eq!(map(KeyCode::F(4)), Some(Action::InputBackspaceWord));
        assert_eq!(map(KeyCode::F(6)), Some(Action::InputClearLine));
    }
}

#[test]
fn context_specific_default_alternatives_do_not_create_false_conflicts() {
    let keymap = resolved("[keymap]\ntoggle-stage = [\"Ctrl+Home\"]\n");
    assert!(!keymap
        .warnings()
        .iter()
        .any(|warning| warning.reason.contains("go-to-top")));
}

#[test]
fn replacing_an_alias_removes_warnings_for_its_obsolete_binding() {
    let keymap =
        resolved("[keymap]\ncommand-palette = [\"F5\"]\nopen-command-palette = [\"F3\"]\n");
    assert!(!keymap
        .warnings()
        .iter()
        .any(|warning| warning.reason.contains("F5")));
}

#[test]
fn go_to_top_g_default_conflicts_in_list_and_detail_contexts() {
    let keymap = resolved("[keymap]\nopen-review-picker = [\"g\"]\n");
    assert!(keymap.warnings().iter().any(|warning| {
        warning.reason.contains("go-to-top")
            && warning.reason.contains("open-review-picker")
            && warning.reason.contains("g")
    }));
}

#[test]
fn filter_text_q_and_y_do_not_conflict_with_cancel_or_confirm() {
    let keymap = resolved(
        "[keymap]\ncommit-filter-backspace = [\"q\"]\nfiles-filter-backspace = [\"y\"]\n",
    );
    assert!(!keymap.warnings().iter().any(|warning| {
        warning.reason.contains("cancel") || warning.reason.contains("confirm")
    }));
}
