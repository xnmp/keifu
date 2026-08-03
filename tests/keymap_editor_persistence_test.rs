mod common;

use common::{commit_file, init_repo, Seed};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use keifu::{
    action::Action,
    app::App,
    config::Config,
    keybindings::map_key_to_action_with_keymap,
};

/// The editor writes through the same config document path as the settings
/// registry, so a saved keymap must leave neighboring user-authored TOML alone.
#[test]
fn saving_keymap_preserves_comments_and_unrelated_settings() {
    let mut config: Config = toml::from_str(
        r#"
[refresh]
# Keep this comment.
auto_refresh = false

[keymap]
pull = ["F7"]
"#,
    )
    .unwrap();
    config.keymap.insert(
        "pull".into(),
        toml::Value::Array(vec![toml::Value::String("Ctrl+Alt+P".into())]),
    );

    let mut document = r#"
[refresh]
# Keep this comment.
auto_refresh = false

[keymap]
# A user note about the shortcut.
pull = ["F7"]
"#
    .parse()
    .unwrap();
    config.apply_to_document(&mut document);
    let saved = document.to_string();

    assert!(saved.contains("# Keep this comment."));
    assert!(saved.contains("# A user note about the shortcut."));
    assert!(saved.contains("auto_refresh = false"));
    assert!(saved.contains("pull = [\"Ctrl+Alt+P\"]"));

    let reloaded: Config = toml::from_str(&saved).unwrap();
    assert_eq!(
        reloaded.keymap["pull"].as_array().unwrap()[0].as_str(),
        Some("Ctrl+Alt+P")
    );
}

#[test]
fn settings_opens_a_dedicated_keyboard_shortcuts_editor() {
    let (_td, repo) = init_repo(Seed::Empty);
    commit_file(repo.repo(), "a.txt", "a", "initial");
    let mut app = App::from_repo(repo).unwrap();
    app.handle_action(Action::OpenSettings).unwrap();

    let action = map_key_to_action_with_keymap(
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
        &app.mode,
        app.focused_panel,
        app.editing_commit_message,
        false,
        false,
        &app.keymap,
    )
    .expect("Ctrl+K should open the keyboard-shortcuts editor from Settings");
    app.handle_action(action).unwrap();

    assert!(
        format!("{:?}", app.mode).contains("KeymapEditor"),
        "Settings should show the keyboard-shortcuts editor, got {:?}",
        app.mode
    );
}

#[test]
fn captured_conflicting_binding_stays_pending_until_saved_and_cancel_discards_it() {
    let (_td, repo) = init_repo(Seed::Empty);
    commit_file(repo.repo(), "a.txt", "a", "initial");
    let mut app = App::from_repo(repo).unwrap();
    app.handle_action(Action::OpenKeymapEditor).unwrap();
    app.handle_action(Action::MenuSelect).unwrap();

    let captured = map_key_to_action_with_keymap(
        KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE),
        &app.mode,
        app.focused_panel,
        false,
        false,
        false,
        &app.keymap,
    )
    .expect("capture mode must receive the raw pressed key");
    app.handle_action(captured).unwrap();

    let pending = format!("{:?}", app.mode);
    assert!(pending.contains("F5"), "captured chord was not reflected: {pending}");
    assert!(
        app.config.keymap.is_empty(),
        "capture must not mutate persisted config before Save"
    );

    app.handle_action(Action::Cancel).unwrap();
    assert!(app.config.keymap.is_empty(), "cancel must discard the pending edit");
}
