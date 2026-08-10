use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use keifu::{
    action::Action,
    app::{AppMode, FocusedPanel},
    keybindings::map_key_to_action,
    palette::CheckoutBranch,
};

fn mapped(code: KeyCode) -> Option<Action> {
    let mode = AppMode::BranchPicker {
        branches: vec![CheckoutBranch {
            name: "feat/142-example".to_string(),
            is_remote: false,
        }],
        selected: 0,
    };
    map_key_to_action(
        KeyEvent::new(code, KeyModifiers::NONE),
        &mode,
        FocusedPanel::Graph,
        false,
        false,
        false,
    )
}

#[test]
fn populated_checkout_picker_routes_keyboard_input_at_the_event_boundary() {
    assert_eq!(mapped(KeyCode::Char('t')), Some(Action::InputChar('t')));
    assert_eq!(mapped(KeyCode::Down), Some(Action::MoveDown));
    assert_eq!(mapped(KeyCode::Up), Some(Action::MoveUp));
    assert_eq!(mapped(KeyCode::Backspace), Some(Action::InputBackspace));
    assert_eq!(mapped(KeyCode::Enter), Some(Action::MenuSelect));
    assert_eq!(mapped(KeyCode::Esc), Some(Action::Cancel));
}
