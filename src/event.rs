//! Event loop and key input handling

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEvent, MouseEvent, MouseEventKind};

/// Poll for events with a custom timeout.
pub fn poll_event_with_timeout(timeout: Duration) -> Result<Option<Event>> {
    if event::poll(timeout)? {
        Ok(Some(event::read()?))
    } else {
        Ok(None)
    }
}

/// Extract key event
pub fn get_key_event(event: &Event) -> Option<KeyEvent> {
    if let Event::Key(key) = event {
        Some(*key)
    } else {
        None
    }
}

/// Extract mouse scroll event (returns positive for scroll down, negative for scroll up)
pub fn get_mouse_scroll(event: &Event) -> Option<i32> {
    if let Event::Mouse(MouseEvent { kind, .. }) = event {
        match kind {
            MouseEventKind::ScrollDown => Some(1),
            MouseEventKind::ScrollUp => Some(-1),
            _ => None,
        }
    } else {
        None
    }
}

/// Extract the full mouse event (button, position) for the input layer.
pub fn get_mouse_event(event: &Event) -> Option<MouseEvent> {
    if let Event::Mouse(m) = event {
        Some(*m)
    } else {
        None
    }
}

/// Extract a bracketed-paste payload. `Some(text)` when the terminal delivered a
/// paste as a single event (requires `EnableBracketedPaste`).
pub fn get_paste_event(event: &Event) -> Option<String> {
    if let Event::Paste(text) = event {
        Some(text.clone())
    } else {
        None
    }
}
