//! Terminal control (raw mode, alternate screen)

use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Initialize the terminal and enable raw mode and the alternate screen
pub fn init() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal
pub fn restore() -> Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

/// Terminals commonly cap the base64 payload of an OSC 52 sequence around
/// 100KB (e.g. xterm's default `set-selection` limit); some are stricter.
/// Payloads larger than this are truncated so the escape sequence doesn't
/// get silently dropped or desync the terminal's parser.
const OSC52_MAX_BASE64_LEN: usize = 100_000;

/// An OSC 52 "set clipboard" escape sequence, plus whether the source text
/// had to be truncated to fit under [`OSC52_MAX_BASE64_LEN`].
struct Osc52Payload {
    sequence: String,
    truncated: bool,
}

/// Build the OSC 52 escape sequence for `text`, capping the base64 payload
/// at [`OSC52_MAX_BASE64_LEN`] bytes. Truncation happens on the raw-byte
/// side, rounded down to a multiple of 3, so the base64 encoding of the
/// truncated payload is still correctly padded.
fn build_osc52_sequence(text: &str) -> Osc52Payload {
    let bytes = text.as_bytes();
    let max_input_len = (OSC52_MAX_BASE64_LEN / 4) * 3;
    let (payload, truncated) = if bytes.len() > max_input_len {
        (&bytes[..max_input_len], true)
    } else {
        (bytes, false)
    };
    Osc52Payload {
        sequence: format!("\x1b]52;c;{}\x07", base64_encode(payload)),
        truncated,
    }
}

/// Copy text to the system clipboard via the OSC 52 escape sequence.
/// Supported by most modern terminals (kitty, Ghostty, WezTerm, iTerm2,
/// Windows Terminal, ...) and works over SSH, with no external tools.
///
/// Used as a fallback when no clipboard shell tool is available (see
/// `app::copy_to_clipboard`). Writes directly to the same stdout handle the
/// ratatui backend renders to and flushes immediately; call this outside of
/// a draw call so the escape bytes aren't interleaved with frame output —
/// the sequence itself is invisible to the renderer either way.
///
/// Returns `true` if the payload was truncated to fit the terminal's OSC 52
/// size limit.
pub fn copy_to_clipboard_osc52(text: &str) -> Result<bool> {
    use std::io::Write;
    let Osc52Payload { sequence, truncated } = build_osc52_sequence(text);
    let mut stdout = io::stdout();
    write!(stdout, "{sequence}")?;
    stdout.flush()?;
    Ok(truncated)
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = u32::from_be_bytes([
            0,
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ]);
        out.push(TABLE[(n >> 18 & 63) as usize] as char);
        out.push(TABLE[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{base64_encode, build_osc52_sequence, OSC52_MAX_BASE64_LEN};

    #[test]
    fn encodes_base64_with_padding() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"main"), "bWFpbg==");
    }

    #[test]
    fn wraps_payload_in_osc52_sequence() {
        let payload = build_osc52_sequence("main");
        assert_eq!(payload.sequence, "\x1b]52;c;bWFpbg==\x07");
        assert!(!payload.truncated);
    }

    #[test]
    fn empty_text_produces_empty_payload() {
        let payload = build_osc52_sequence("");
        assert_eq!(payload.sequence, "\x1b]52;c;\x07");
        assert!(!payload.truncated);
    }

    #[test]
    fn small_payload_is_not_truncated() {
        let text = "x".repeat(1000);
        let payload = build_osc52_sequence(&text);
        assert!(!payload.truncated);
        // Round-trips: decode length matches source length (4 base64 chars
        // per 3 source bytes, ignoring padding).
        let expected_len = 4 * 1000usize.div_ceil(3);
        let inner = payload
            .sequence
            .strip_prefix("\x1b]52;c;")
            .and_then(|s| s.strip_suffix('\x07'))
            .unwrap();
        assert_eq!(inner.len(), expected_len);
    }

    #[test]
    fn oversized_payload_is_truncated_to_cap() {
        // One byte over the cap-in-bytes boundary forces truncation.
        let max_input_len = (OSC52_MAX_BASE64_LEN / 4) * 3;
        let text = "a".repeat(max_input_len + 1);
        let payload = build_osc52_sequence(&text);
        assert!(payload.truncated);

        let inner = payload
            .sequence
            .strip_prefix("\x1b]52;c;")
            .and_then(|s| s.strip_suffix('\x07'))
            .unwrap();
        assert!(inner.len() <= OSC52_MAX_BASE64_LEN);
        // Truncated cleanly to a multiple-of-3 input, so no padding chars.
        assert!(!inner.ends_with('='));
    }

    #[test]
    fn payload_at_exact_cap_boundary_is_not_truncated() {
        let max_input_len = (OSC52_MAX_BASE64_LEN / 4) * 3;
        let text = "a".repeat(max_input_len);
        let payload = build_osc52_sequence(&text);
        assert!(!payload.truncated);
    }
}
