//! Remote control server for debugging (enabled with --debug-listen)
//!
//! Speaks newline-delimited JSON over TCP so that an agent (or a human with
//! netcat) can drive the running TUI: send key/mouse input, dump the screen
//! as plain text, and inspect the application state.
//!
//! Injected key/mouse events go through the exact same mapping the real event
//! loop uses (`map_key_to_action` / `map_mouse_to_action` → `handle_action`),
//! so behavior is identical to a human at the terminal.
//!
//! Protocol (one request per line, one JSON response per line):
//!   {"cmd":"keys","keys":"j j <enter>"}         feed key input
//!   {"cmd":"mouse","kind":"click","x":5,"y":3}   click / scroll_up / scroll_down
//!   {"cmd":"dump"}                               plain-text screen dump
//!   {"cmd":"state"}                              app state summary

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{backend::TestBackend, Terminal};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    app::{App, AppMode, FocusedPanel},
    keybindings::{map_key_to_action, map_mouse_to_action},
    ui,
};

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum DebugRequest {
    Keys {
        keys: String,
    },
    Mouse {
        kind: String,
        x: u16,
        y: u16,
    },
    /// Inject a bracketed-paste chunk (same path as a real terminal paste).
    Paste {
        text: String,
    },
    /// Screen dump; width/height override the real terminal size
    Dump {
        width: Option<u16>,
        height: Option<u16>,
    },
    State,
}

pub struct DebugCommand {
    pub request: DebugRequest,
    pub reply: Sender<Value>,
}

/// Bind the listener and spawn the server thread.
pub fn spawn(addr: &str) -> Result<Receiver<DebugCommand>> {
    let listener =
        TcpListener::bind(addr).with_context(|| format!("Failed to bind debug server to {addr}"))?;
    let (tx, rx) = mpsc::channel::<DebugCommand>();

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let Ok(read_half) = stream.try_clone() else {
                continue;
            };
            let mut writer = stream;
            for line in BufReader::new(read_half).lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                let response = match serde_json::from_str::<DebugRequest>(&line) {
                    Ok(request) => {
                        let (reply_tx, reply_rx) = mpsc::channel();
                        if tx
                            .send(DebugCommand {
                                request,
                                reply: reply_tx,
                            })
                            .is_err()
                        {
                            return; // Main loop is gone
                        }
                        reply_rx
                            .recv_timeout(Duration::from_secs(5))
                            .unwrap_or_else(|_| json!({"ok": false, "error": "timeout"}))
                    }
                    Err(e) => json!({"ok": false, "error": format!("invalid request: {e}")}),
                };
                if writeln!(writer, "{response}").is_err() {
                    break;
                }
            }
        }
    });

    Ok(rx)
}

/// Execute a debug request against the app. `width`/`height` should be the
/// real terminal size so screen dumps match what the user sees.
pub fn handle_request(app: &mut App, width: u16, height: u16, request: DebugRequest) -> Value {
    match request {
        DebugRequest::Keys { keys } => match parse_key_sequence(&keys) {
            Ok(events) => {
                for key in events {
                    app.maybe_hint_capslock(&key);
                    if let Some(action) = map_key_to_action(
                        key,
                        &app.mode,
                        app.focused_panel,
                        app.editing_commit_message,
                        app.files_pane.files_filter_active,
                        app.commit_filter_active,
                    ) {
                        if let Err(e) = app.handle_action(action) {
                            app.show_error(format!("{}", e));
                        }
                    }
                }
                json!({"ok": true})
            }
            Err(e) => json!({"ok": false, "error": e}),
        },
        DebugRequest::Mouse { kind, x, y } => {
            let kind = match kind.as_str() {
                "click" => MouseEventKind::Down(MouseButton::Left),
                "right_click" => MouseEventKind::Down(MouseButton::Right),
                "scroll_up" => MouseEventKind::ScrollUp,
                "scroll_down" => MouseEventKind::ScrollDown,
                other => {
                    return json!({"ok": false, "error": format!("unknown mouse kind: {other}")})
                }
            };
            // Same path as real mouse input in the event loop.
            let event = MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            };
            if let Some(action) = map_mouse_to_action(event) {
                if let Err(e) = app.handle_action(action) {
                    app.show_error(format!("{}", e));
                }
            }
            json!({"ok": true})
        }
        DebugRequest::Paste { text } => {
            if let Err(e) = app.handle_paste(text) {
                app.show_error(format!("{}", e));
            }
            json!({"ok": true})
        }
        DebugRequest::Dump {
            width: req_width,
            height: req_height,
        } => {
            // Headless terminals (e.g. under `script`) can report 0x0
            let width = req_width.unwrap_or(width).clamp(20, 500);
            let height = req_height.unwrap_or(height).clamp(6, 300);
            match render_to_text(app, width, height) {
                Ok(screen) => {
                    json!({"ok": true, "width": width, "height": height, "screen": screen})
                }
                Err(e) => json!({"ok": false, "error": format!("{e}")}),
            }
        }
        DebugRequest::State => state_json(app),
    }
}

/// Render the current app state to a plain-text screen using a test backend.
///
/// Rendering also records the pane layout rects on `App` (mouse hit-testing
/// depends on them), so a `dump` at a given size must precede any `mouse`
/// command that expects that layout — see `docs/debugging.md`.
fn render_to_text(app: &mut App, width: u16, height: u16) -> Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    let started = std::time::Instant::now();
    terminal.draw(|frame| ui::draw(frame, app))?;
    app.perf.record("draw.dump", started.elapsed());

    let buffer = terminal.backend().buffer();
    let mut lines = Vec::with_capacity(height as usize);
    for y in 0..height {
        let mut line = String::new();
        for x in 0..width {
            if let Some(cell) = buffer.cell((x, y)) {
                line.push_str(cell.symbol());
            }
        }
        lines.push(line.trim_end().to_string());
    }
    Ok(lines.join("\n"))
}

/// Stable string name for the current mode. Exhaustive so a new `AppMode`
/// variant forces this to be updated rather than silently mislabeled.
fn mode_name(mode: &AppMode) -> &'static str {
    match mode {
        AppMode::Normal => "normal",
        AppMode::Help => "help",
        AppMode::Input { .. } => "input",
        AppMode::Confirm { .. } => "confirm",
        AppMode::CommitMenu { .. } => "commit_menu",
        AppMode::MetadataMenu { .. } => "metadata_menu",
        AppMode::Settings { .. } => "settings",
        AppMode::PullDivergence { .. } => "pull_divergence",
        AppMode::CiChecks => "ci_checks",
        AppMode::PrThread => "pr_thread",
        AppMode::PrCompose { .. } => "pr_compose",
        AppMode::PrMergePicker { .. } => "pr_merge_picker",
        AppMode::PrReviewPicker { .. } => "pr_review_picker",
        AppMode::IssueList => "issue_list",
        AppMode::IssueDetail => "issue_detail",
        AppMode::IssueCompose { .. } => "issue_compose",
        AppMode::IssueLabelPicker { .. } => "issue_label_picker",
        AppMode::IssueLabelFilter { .. } => "issue_label_filter",
        AppMode::BranchFilter { .. } => "branch_filter",
        AppMode::BranchPicker { .. } => "branch_picker",
        AppMode::BranchDeletePicker { .. } => "branch_delete_picker",
        AppMode::TagPicker { .. } => "tag_picker",
        AppMode::RemotePicker { .. } => "remote_picker",
        AppMode::FileDiff { .. } => "file_diff",
        AppMode::FileHistory { .. } => "file_history",
        AppMode::CommandPalette { .. } => "command_palette",
    }
}

fn state_json(app: &App) -> Value {
    let focused = match app.focused_panel {
        FocusedPanel::Graph => "graph",
        FocusedPanel::Files => "files",
        FocusedPanel::CommitDetail => "commit_detail",
    };
    let selected = app.graph_nav.graph_list_state.selected();
    let selected_node = selected.and_then(|idx| app.graph_layout.nodes.get(idx));
    let selected_commit = selected_node
        .and_then(|node| node.commit.as_ref())
        .map(|commit| commit.short_id.clone());
    let selected_branches = selected_node
        .map(|node| node.branch_names.clone())
        .unwrap_or_default();

    json!({
        "ok": true,
        "mode": mode_name(&app.mode),
        "focused_panel": focused,
        "selected_index": selected,
        "selected_commit": selected_commit,
        "selected_branches": selected_branches,
        "head": app.head_name,
        "node_count": app.graph_layout.nodes.len(),
        "commit_count": app.commits.len(),
        "editing_commit_message": app.editing_commit_message,
        "is_fetching": app.is_fetching(),
        "is_pushing": app.is_pushing(),
        "is_pulling": app.is_pulling(),
        // Settings-menu-managed flags, exposed so tests can assert persistence
        // across restarts.
        "hide_remote_branches": app.hide_remote_branches,
        "hide_stashes": app.hide_stashes,
        "trace_enabled": app.trace_enabled,
        "diff_word_wrap": app.diff_word_wrap,
        "graph_renderer": app.config.ui.graph_renderer.as_str(),
    })
}

/// Parse a key sequence like "j j <enter> G <c-d> @"
///
/// Whitespace-separated tokens; single characters are sent as-is (uppercase
/// implies Shift), and special keys use angle brackets: <enter> <esc> <tab>
/// <backtab> <space> <up> <down> <left> <right> <home> <end> <pgup> <pgdn>
/// <backspace> <c-x> (Ctrl+x)
fn parse_key_sequence(input: &str) -> std::result::Result<Vec<KeyEvent>, String> {
    let mut events = Vec::new();
    for token in input.split_whitespace() {
        events.push(parse_key_token(token)?);
    }
    Ok(events)
}

fn parse_key_token(token: &str) -> std::result::Result<KeyEvent, String> {
    let mut chars = token.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        let modifiers = if c.is_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        return Ok(KeyEvent::new(KeyCode::Char(c), modifiers));
    }

    let inner = token
        .strip_prefix('<')
        .and_then(|t| t.strip_suffix('>'))
        .ok_or_else(|| format!("invalid key token: {token}"))?
        .to_ascii_lowercase();

    // Ctrl+Alt combo, e.g. <c-a-w>.
    if let Some(c) = inner.strip_prefix("c-a-") {
        let mut it = c.chars();
        if let (Some(ch), None) = (it.next(), it.next()) {
            return Ok(KeyEvent::new(
                KeyCode::Char(ch),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ));
        }
        return Err(format!("invalid ctrl-alt key token: {token}"));
    }

    if let Some(c) = inner.strip_prefix("c-") {
        // Ctrl + a named special key, e.g. <c-up>.
        let named = match c {
            "up" => Some(KeyCode::Up),
            "down" => Some(KeyCode::Down),
            "left" => Some(KeyCode::Left),
            "right" => Some(KeyCode::Right),
            _ => None,
        };
        if let Some(code) = named {
            return Ok(KeyEvent::new(code, KeyModifiers::CONTROL));
        }
        let mut it = c.chars();
        if let (Some(ch), None) = (it.next(), it.next()) {
            return Ok(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL));
        }
        return Err(format!("invalid ctrl key token: {token}"));
    }

    let code = match inner.as_str() {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => return Ok(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pgup" => KeyCode::PageUp,
        "pgdn" => KeyCode::PageDown,
        "backspace" => KeyCode::Backspace,
        other => return Err(format!("unknown key token: <{other}>")),
    };
    Ok(KeyEvent::new(code, KeyModifiers::NONE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_special_keys() {
        let events = parse_key_sequence("j G <enter> <c-d> <space>").unwrap();
        assert_eq!(events.len(), 5);
        assert_eq!(events[0].code, KeyCode::Char('j'));
        assert_eq!(events[0].modifiers, KeyModifiers::NONE);
        assert_eq!(events[1].code, KeyCode::Char('G'));
        assert_eq!(events[1].modifiers, KeyModifiers::SHIFT);
        assert_eq!(events[2].code, KeyCode::Enter);
        assert_eq!(events[3].code, KeyCode::Char('d'));
        assert_eq!(events[3].modifiers, KeyModifiers::CONTROL);
        assert_eq!(events[4].code, KeyCode::Char(' '));
    }

    #[test]
    fn rejects_unknown_tokens() {
        assert!(parse_key_sequence("<bogus>").is_err());
        assert!(parse_key_sequence("ab").is_err());
    }

    #[test]
    fn parses_backtab_and_ctrl() {
        let events = parse_key_sequence("<backtab> <c-p>").unwrap();
        assert_eq!(events[0].code, KeyCode::BackTab);
        assert_eq!(events[0].modifiers, KeyModifiers::SHIFT);
        assert_eq!(events[1].code, KeyCode::Char('p'));
        assert_eq!(events[1].modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn parses_ctrl_arrow_keys() {
        let events = parse_key_sequence("<c-up> <c-down> <c-left> <c-right>").unwrap();
        assert_eq!(events[0].code, KeyCode::Up);
        assert_eq!(events[0].modifiers, KeyModifiers::CONTROL);
        assert_eq!(events[1].code, KeyCode::Down);
        assert_eq!(events[1].modifiers, KeyModifiers::CONTROL);
        assert_eq!(events[2].code, KeyCode::Left);
        assert_eq!(events[2].modifiers, KeyModifiers::CONTROL);
        assert_eq!(events[3].code, KeyCode::Right);
        assert_eq!(events[3].modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn parses_ctrl_alt_combo() {
        let events = parse_key_sequence("<c-a-w>").unwrap();
        assert_eq!(events[0].code, KeyCode::Char('w'));
        assert_eq!(events[0].modifiers, KeyModifiers::CONTROL | KeyModifiers::ALT);
    }

    #[test]
    fn parses_json_request_variants() {
        let keys: DebugRequest =
            serde_json::from_str(r#"{"cmd":"keys","keys":"j j"}"#).unwrap();
        assert!(matches!(keys, DebugRequest::Keys { .. }));

        let mouse: DebugRequest =
            serde_json::from_str(r#"{"cmd":"mouse","kind":"click","x":5,"y":3}"#).unwrap();
        assert!(matches!(mouse, DebugRequest::Mouse { x: 5, y: 3, .. }));

        let dump: DebugRequest =
            serde_json::from_str(r#"{"cmd":"dump","width":100,"height":30}"#).unwrap();
        assert!(matches!(
            dump,
            DebugRequest::Dump {
                width: Some(100),
                height: Some(30)
            }
        ));

        let state: DebugRequest = serde_json::from_str(r#"{"cmd":"state"}"#).unwrap();
        assert!(matches!(state, DebugRequest::State));
    }
}
