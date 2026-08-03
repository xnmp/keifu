use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use keifu::{
    action::Action,
    app::{AppMode, FocusedPanel},
    config::Config,
    keybindings::{map_key_to_action, map_key_to_action_with_keymap},
    keymap::ResolvedKeymap,
};

fn map_graph(key: KeyEvent) -> Option<Action> {
    map_key_to_action(
        key,
        &AppMode::Normal,
        FocusedPanel::Graph,
        false,
        false,
        false,
    )
}

fn map_editor(key: KeyEvent) -> Option<Action> {
    map_key_to_action(
        key,
        &AppMode::Normal,
        FocusedPanel::CommitDetail,
        true,
        false,
        false,
    )
}

fn map_graph_with_keymap(key: KeyEvent, keymap: &ResolvedKeymap) -> Option<Action> {
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

fn resolved_keymap(source: &str) -> ResolvedKeymap {
    let config: Config = toml::from_str(source).expect("keymap config parses");
    ResolvedKeymap::from_table(&config.keymap)
}

#[test]
fn enhanced_keyboard_shift_forms_preserve_bindings_and_editor_text() {
    // With the Kitty alternate-key flag, Crossterm supplies `G` without the
    // SHIFT modifier. Without it, terminals report the base `g` plus SHIFT.
    // Both shapes must retain the user's intended shifted key.
    assert_eq!(
        map_graph(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::SHIFT,)),
        Some(Action::GoToBottom)
    );
    assert_eq!(
        map_graph(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE)),
        Some(Action::GoToBottom)
    );
    assert_eq!(
        map_graph(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
        Some(Action::ToggleHelp)
    );
    assert_eq!(
        map_editor(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::SHIFT,)),
        Some(Action::EditorChar('G'))
    );
    assert_eq!(
        map_editor(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE)),
        Some(Action::EditorChar('G'))
    );
}

#[test]
fn enhanced_keyboard_ctrl_shift_forms_preserve_commit_search() {
    // Alternate-key reporting supplies `F` and clears SHIFT, while the base
    // protocol form carries `f` plus SHIFT. Both must remain commit search;
    // unshifted Ctrl+F remains branch quick search.
    assert_eq!(
        map_graph(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::CONTROL)),
        Some(Action::StartCommitFilter)
    );
    assert_eq!(
        map_graph(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        )),
        Some(Action::StartCommitFilter)
    );
    assert_eq!(
        map_graph(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)),
        Some(Action::Search)
    );
}

#[test]
fn enhanced_keyboard_shift_forms_preserve_configured_shortcuts() {
    let keymap =
        resolved_keymap("[keymap]\npush = [\"Shift+P\"]\nopen-issue-list = [\"Alt+Shift+I\"]\n");

    // Alternate-key reporting supplies the shifted character and removes
    // SHIFT; the base protocol supplies the lowercase character plus SHIFT.
    // Both forms must honor configured shortcuts before legacy dispatch.
    assert_eq!(
        map_graph_with_keymap(
            KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE),
            &keymap
        ),
        Some(Action::Push)
    );
    assert_eq!(
        map_graph_with_keymap(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::SHIFT),
            &keymap
        ),
        Some(Action::Push)
    );
    assert_eq!(
        map_graph_with_keymap(
            KeyEvent::new(KeyCode::Char('I'), KeyModifiers::ALT,),
            &keymap
        ),
        Some(Action::OpenIssueList)
    );
    assert_eq!(
        map_graph_with_keymap(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::ALT | KeyModifiers::SHIFT,),
            &keymap
        ),
        Some(Action::OpenIssueList)
    );
}
