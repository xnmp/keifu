use keifu::config::Config;

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
