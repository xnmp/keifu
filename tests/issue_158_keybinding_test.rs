use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use keifu::action::Action;
use keifu::app::{AppMode, FocusedPanel};
use keifu::keybindings::map_key_to_action;

#[test]
fn graph_mode_l_pulls() {
    let key = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE);
    assert_eq!(
        map_key_to_action(
            key,
            &AppMode::Normal,
            FocusedPanel::Graph,
            false,
            false,
            false,
        ),
        Some(Action::Pull)
    );
}
