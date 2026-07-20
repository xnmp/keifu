//! Transient status-message handling.

use super::*;

impl App {
    /// Set a status message (will auto-clear after a few seconds)
    pub fn set_message(&mut self, msg: impl Into<String>) {
        self.message = Some(msg.into());
        self.message_time = Some(std::time::Instant::now());
    }

    /// How long a status message stays on screen after being set.
    const MESSAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    /// Get current message if not expired (5 seconds timeout).
    ///
    /// The timeout always applies — including while a network operation is in
    /// progress. Progress/result feedback for network ops is surfaced via
    /// toasts (which carry their own TTL), so a `set_message` string is always
    /// transient. An earlier version kept the message alive for the whole
    /// duration of any network op, which meant a stale one-shot message (e.g.
    /// "Opening PR #N in browser") re-surfaced on every silent auto-fetch —
    /// making it flash back every ~60s. Respecting the timeout regardless of
    /// busy state fixes that.
    pub fn get_message(&self) -> Option<&str> {
        let msg = self.message.as_deref()?;
        let time = self.message_time.as_ref()?;

        if time.elapsed() < Self::MESSAGE_TIMEOUT {
            Some(msg)
        } else {
            None
        }
    }

    /// Returns the instant when the current message should expire and be cleared
    /// from the display. Returns `None` if there is no message or it has already
    /// expired.
    pub fn message_expiry_time(&self) -> Option<std::time::Instant> {
        let _msg = self.message.as_ref()?;
        let time = self.message_time.as_ref()?;
        if time.elapsed() < Self::MESSAGE_TIMEOUT {
            Some(*time + Self::MESSAGE_TIMEOUT)
        } else {
            None
        }
    }

    /// Get search match count
    pub fn search_match_count(&self) -> usize {
        self.search_state.fuzzy_matches.len()
    }

    /// Push a toast notification (kind drives color + TTL).
    pub fn toast(&mut self, kind: crate::toast::ToastKind, text: impl Into<String>) {
        self.toasts
            .push(kind, text, std::time::Instant::now());
    }

    /// The next instant a toast or the status message will need a redraw.
    pub fn next_render_deadline(&self) -> Option<std::time::Instant> {
        [self.message_expiry_time(), self.toasts.next_expiry()]
            .into_iter()
            .flatten()
            .min()
    }
}
