//! Transient status-message handling.

use super::*;
use std::time::{Duration, Instant};

/// How long a transient status message stays on screen before it clears.
const MESSAGE_TIMEOUT: Duration = Duration::from_secs(5);

/// Pure visibility rule for a status message, so the timeout/stickiness logic is
/// unit-testable without a clock or a live network op.
///
/// A message is shown while it is within the timeout. Once the timeout has
/// elapsed it is gone — *unless* it is a sticky network-progress message and an
/// op is still in flight, in which case it stays up until the op completes and
/// clears it. Crucially, a non-sticky (plain) message is NEVER resurrected by
/// network activity: that resurrection was the "stale message re-flashes every
/// few minutes" bug, where the periodic silent auto-fetch flipped the busy flag
/// and revived a long-expired message.
fn message_visible(elapsed: Duration, sticky: bool, network_busy: bool) -> bool {
    elapsed < MESSAGE_TIMEOUT || (sticky && network_busy)
}

impl App {
    /// Set a transient status message (auto-clears after the timeout).
    pub fn set_message(&mut self, msg: impl Into<String>) {
        self.message = Some(msg.into());
        self.message_time = Some(Instant::now());
        self.message_sticky = false;
    }

    /// Set a network-progress message ("Pulling…") that persists for the whole
    /// in-flight operation rather than obeying the plain timeout. Must be
    /// paired with `clear_progress_message()` on op completion. Only pull uses
    /// this — fetch and push report their start via a toast instead.
    pub(crate) fn set_progress_message(&mut self, msg: impl Into<String>) {
        self.message = Some(msg.into());
        self.message_time = Some(Instant::now());
        self.message_sticky = true;
    }

    /// Clear a sticky progress message once its network op finishes, so it can't
    /// be revived by a later op flipping the busy flag. No-op for plain messages
    /// (they self-expire), so an unrelated transient message isn't wiped when a
    /// background op happens to complete.
    pub(crate) fn clear_progress_message(&mut self) {
        if self.message_sticky {
            self.message = None;
            self.message_time = None;
            self.message_sticky = false;
        }
    }

    /// Get current message if it should still be shown.
    pub fn get_message(&self) -> Option<&str> {
        let msg = self.message.as_deref()?;
        let time = self.message_time.as_ref()?;
        if message_visible(time.elapsed(), self.message_sticky, self.is_network_busy()) {
            Some(msg)
        } else {
            None
        }
    }

    /// Returns the instant when the current message should expire and be cleared
    /// from the display. Returns `None` if there is no message, it has already
    /// expired, or it is a sticky progress message held open by a live op (which
    /// clears on completion rather than on a fixed deadline).
    pub fn message_expiry_time(&self) -> Option<Instant> {
        let time = self.message_time.as_ref()?;
        let _msg = self.message.as_ref()?;
        if self.message_sticky && self.is_network_busy() {
            return None;
        }
        let elapsed = time.elapsed();
        if elapsed < MESSAGE_TIMEOUT {
            Some(*time + MESSAGE_TIMEOUT)
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
        self.toasts.push(kind, text, std::time::Instant::now());
    }

    /// Nudge the user once per reported Caps Lock session (#130). Crossterm's
    /// keyboard-enhancement protocol attaches this state to compatible key
    /// events; terminals that omit it leave the state empty and silently take
    /// this re-arming path. Text-entry contexts deliberately skip the warning.
    pub fn maybe_hint_capslock(&mut self, key: &crossterm::event::KeyEvent) {
        if !key
            .state
            .contains(crossterm::event::KeyEventState::CAPS_LOCK)
        {
            self.capslock_hint_armed = true;
            return;
        }
        if !self.capslock_hint_armed {
            return;
        }
        if crate::keybindings::is_text_editing_context(
            &self.mode,
            self.focused_panel,
            self.editing_commit_message,
            self.files_pane.files_filter_active,
            self.commit_filter_active,
        ) {
            return;
        }
        self.capslock_hint_armed = false;
        self.toast(
            crate::toast::ToastKind::Info,
            "Caps Lock is on — keys are case-sensitive",
        );
    }

    /// The next instant a toast or the status message will need a redraw.
    pub fn next_render_deadline(&self) -> Option<std::time::Instant> {
        [self.message_expiry_time(), self.toasts.next_expiry()]
            .into_iter()
            .flatten()
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::{message_visible, MESSAGE_TIMEOUT};
    use std::time::Duration;

    #[test]
    fn fresh_message_is_visible() {
        assert!(message_visible(Duration::from_secs(1), false, false));
        assert!(message_visible(Duration::from_secs(1), false, true));
    }

    #[test]
    fn plain_message_expires_after_timeout() {
        let expired = MESSAGE_TIMEOUT + Duration::from_secs(1);
        assert!(!message_visible(expired, false, false));
    }

    #[test]
    fn expired_plain_message_is_not_revived_by_network_busy() {
        // Regression for the "stale message re-flashes every few minutes" bug:
        // a long-expired plain message must stay gone even while a background
        // network op (e.g. the silent auto-fetch) is in flight.
        let expired = MESSAGE_TIMEOUT + Duration::from_secs(120);
        assert!(!message_visible(expired, false, true));
    }

    #[test]
    fn sticky_progress_message_persists_while_busy() {
        let expired = MESSAGE_TIMEOUT + Duration::from_secs(30);
        assert!(message_visible(expired, true, true));
    }

    #[test]
    fn sticky_message_clears_once_op_no_longer_busy() {
        let expired = MESSAGE_TIMEOUT + Duration::from_secs(1);
        assert!(!message_visible(expired, true, false));
    }
}

#[cfg(test)]
mod capslock_hint_tests {
    use crate::app::App;
    use crate::git::repository::GitRepository;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn test_app() -> (tempfile::TempDir, App) {
        let tempdir = tempfile::tempdir().unwrap();
        git2::Repository::init(tempdir.path()).unwrap();
        let repo = GitRepository::open(tempdir.path()).unwrap();
        let app = App::from_repo(repo).unwrap();
        (tempdir, app)
    }

    fn key_with_state(state: KeyEventState) -> KeyEvent {
        KeyEvent::new_with_kind_and_state(
            KeyCode::Char('k'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
            state,
        )
    }

    #[test]
    fn first_keypress_with_reported_capslock_state_toasts() {
        let (_tmp, mut app) = test_app();
        assert!(app.toasts.is_empty());
        app.maybe_hint_capslock(&key_with_state(KeyEventState::CAPS_LOCK));
        assert_eq!(app.toasts.visible().len(), 1);
        assert_eq!(app.toasts.visible()[0].kind, crate::toast::ToastKind::Info);
    }

    #[test]
    fn no_toast_while_editing_commit_message() {
        let (_tmp, mut app) = test_app();
        app.focused_panel = crate::app::FocusedPanel::CommitDetail;
        app.editing_commit_message = true;
        app.maybe_hint_capslock(&key_with_state(KeyEventState::CAPS_LOCK));
        assert!(
            app.toasts.is_empty(),
            "typing uppercase in the commit editor is normal, not a CapsLock signal"
        );
    }

    #[test]
    fn no_toast_while_a_text_filter_is_active() {
        let (_tmp, mut app) = test_app();
        app.focused_panel = crate::app::FocusedPanel::Files;
        app.files_pane.files_filter_active = true;
        app.maybe_hint_capslock(&key_with_state(KeyEventState::CAPS_LOCK));
        assert!(app.toasts.is_empty());
    }

    #[test]
    fn no_toast_while_input_mode_is_capturing_text() {
        let (_tmp, mut app) = test_app();
        app.mode = crate::app::AppMode::Input {
            title: "Branch name".into(),
            input: String::new(),
            action: crate::app::InputAction::CreateBranch,
        };
        app.maybe_hint_capslock(&key_with_state(KeyEventState::CAPS_LOCK));
        assert!(app.toasts.is_empty());
    }

    #[test]
    fn further_capslock_keypresses_in_the_same_session_do_not_toast() {
        let (_tmp, mut app) = test_app();
        app.maybe_hint_capslock(&key_with_state(KeyEventState::CAPS_LOCK));
        assert_eq!(app.toasts.visible().len(), 1);
        app.maybe_hint_capslock(&key_with_state(KeyEventState::CAPS_LOCK));
        assert_eq!(
            app.toasts.visible().len(),
            1,
            "a Caps Lock session must only warn once"
        );
    }

    #[test]
    fn inactive_keypress_rearms_the_next_capslock_session() {
        let (_tmp, mut app) = test_app();
        app.maybe_hint_capslock(&key_with_state(KeyEventState::CAPS_LOCK));
        app.maybe_hint_capslock(&key_with_state(KeyEventState::empty()));
        app.maybe_hint_capslock(&key_with_state(KeyEventState::CAPS_LOCK));
        assert_eq!(app.toasts.visible().len(), 2);
    }

    #[test]
    fn keypress_without_reported_capslock_state_is_silent() {
        let (_tmp, mut app) = test_app();
        let unreported = KeyEvent::new(KeyCode::Char('K'), KeyModifiers::NONE);
        app.maybe_hint_capslock(&unreported);
        assert!(app.toasts.is_empty());
    }
}
