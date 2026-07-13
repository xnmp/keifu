//! Transient status-message handling.

use super::*;

impl App {
    /// Set a status message (will auto-clear after a few seconds)
    pub fn set_message(&mut self, msg: impl Into<String>) {
        self.message = Some(msg.into());
        self.message_time = Some(std::time::Instant::now());
    }

    /// Get current message if not expired (5 seconds timeout)
    pub fn get_message(&self) -> Option<&str> {
        const MESSAGE_TIMEOUT_SECS: u64 = 5;

        // Don't timeout while a network operation is in progress
        if self.is_network_busy() {
            return self.message.as_deref();
        }

        let msg = self.message.as_deref()?;
        let time = self.message_time.as_ref()?;

        if time.elapsed().as_secs() < MESSAGE_TIMEOUT_SECS {
            Some(msg)
        } else {
            None
        }
    }

    /// Returns the instant when the current message should expire and be cleared
    /// from the display. Returns `None` if no message, already expired, or network
    /// busy (busy messages don't expire).
    pub fn message_expiry_time(&self) -> Option<std::time::Instant> {
        const MESSAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

        if self.is_network_busy() {
            return None;
        }
        let _msg = self.message.as_ref()?;
        let time = self.message_time.as_ref()?;
        if time.elapsed() < MESSAGE_TIMEOUT {
            Some(*time + MESSAGE_TIMEOUT)
        } else {
            None
        }
    }

    /// Get search match count
    pub fn search_match_count(&self) -> usize {
        self.search_state.fuzzy_matches.len()
    }
}
