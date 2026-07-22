//! Transient toast notifications for background-operation outcomes.
//!
//! `ToastQueue` is a pure state machine — all time is injected as a parameter,
//! so expiry, overflow, and stacking are unit-testable without a clock. The
//! render loop calls `evict(now)` each iteration and drives redraws off
//! `next_expiry()`; the widget renders `visible()`.

use std::time::{Duration, Instant};

/// Most toasts shown at once; a newer one over the cap drops the oldest.
const MAX_VISIBLE: usize = 3;
/// Time-to-live for info/success toasts.
const DEFAULT_TTL: Duration = Duration::from_secs(4);
/// Errors linger much longer so they're not missed — since #116 they are the
/// ONLY surface for one-shot errors (no blocking modal), so they must survive
/// a glance away. Esc dismisses them early (`dismiss_errors`).
const ERROR_TTL: Duration = Duration::from_secs(12);

/// Toast severity, driving color and TTL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

impl ToastKind {
    fn ttl(self) -> Duration {
        match self {
            ToastKind::Error => ERROR_TTL,
            _ => DEFAULT_TTL,
        }
    }
}

/// One notification.
#[derive(Debug, Clone)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    expires_at: Instant,
}

impl Toast {
    #[cfg(test)]
    fn expires_at(&self) -> Instant {
        self.expires_at
    }
}

/// A bounded, expiry-driven queue of toasts. Ordered oldest → newest.
#[derive(Debug, Default)]
pub struct ToastQueue {
    toasts: Vec<Toast>,
}

impl ToastQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a toast at time `now`. Expired toasts are dropped first, then the
    /// queue is capped to `MAX_VISIBLE` by removing the oldest.
    pub fn push(&mut self, kind: ToastKind, text: impl Into<String>, now: Instant) {
        self.evict(now);
        self.toasts.push(Toast {
            kind,
            text: text.into(),
            expires_at: now + kind.ttl(),
        });
        while self.toasts.len() > MAX_VISIBLE {
            self.toasts.remove(0);
        }
    }

    /// Drop toasts whose TTL has elapsed by `now`. Returns true if any were
    /// removed (the caller should redraw).
    pub fn evict(&mut self, now: Instant) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.expires_at > now);
        self.toasts.len() != before
    }

    /// Drop every ERROR toast immediately (#116): Esc dismisses lingering
    /// errors without waiting out their TTL. Info/success toasts are left to
    /// expire on their own — they are short-lived, and letting them swallow an
    /// Esc would make quit/cancel feel unreliable. Returns whether anything
    /// was dismissed (the caller then consumes the key and redraws).
    pub fn dismiss_errors(&mut self) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.kind != ToastKind::Error);
        self.toasts.len() != before
    }

    /// Currently live toasts, oldest → newest. The renderer stacks the newest
    /// on top. Call `evict(now)` first so nothing expired is shown.
    pub fn visible(&self) -> &[Toast] {
        &self.toasts
    }

    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    /// Earliest expiry among live toasts — the render loop's next deadline.
    pub fn next_expiry(&self) -> Option<Instant> {
        self.toasts.iter().map(|t| t.expires_at).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn push_and_visible_in_order() {
        let t0 = Instant::now();
        let mut q = ToastQueue::new();
        q.push(ToastKind::Info, "first", t0);
        q.push(ToastKind::Success, "second", at(t0, 1));
        let v = q.visible();
        assert_eq!(v.len(), 2);
        // Oldest first.
        assert_eq!(v[0].text, "first");
        assert_eq!(v[1].text, "second");
    }

    #[test]
    fn overflow_drops_oldest_keeping_three_newest() {
        let t0 = Instant::now();
        let mut q = ToastQueue::new();
        for (i, name) in ["a", "b", "c", "d"].iter().enumerate() {
            q.push(ToastKind::Info, *name, at(t0, i as u64));
        }
        let texts: Vec<&str> = q.visible().iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["b", "c", "d"], "oldest 'a' dropped");
    }

    #[test]
    fn info_and_error_ttls_differ() {
        let t0 = Instant::now();
        let mut q = ToastQueue::new();
        q.push(ToastKind::Info, "i", t0);
        q.push(ToastKind::Error, "e", t0);
        let info = q.visible().iter().find(|t| t.text == "i").unwrap();
        let err = q.visible().iter().find(|t| t.text == "e").unwrap();
        assert_eq!(info.expires_at(), t0 + DEFAULT_TTL);
        assert_eq!(err.expires_at(), t0 + ERROR_TTL);
    }

    #[test]
    fn evict_at_boundary_times() {
        let t0 = Instant::now();
        let mut q = ToastQueue::new();
        q.push(ToastKind::Info, "i", t0); // expires at t0+4
        // Just before expiry: still visible, evict is a no-op.
        assert!(!q.evict(at(t0, 3)));
        assert_eq!(q.visible().len(), 1);
        // Exactly at expiry (expires_at > now is false) → evicted.
        assert!(q.evict(at(t0, 4)));
        assert!(q.visible().is_empty());
    }

    #[test]
    fn error_outlives_info() {
        let t0 = Instant::now();
        let mut q = ToastQueue::new();
        q.push(ToastKind::Info, "i", t0);
        q.push(ToastKind::Error, "e", t0);
        // At t0+5: info (ttl 4) gone, error (ttl 8) remains.
        q.evict(at(t0, 5));
        let texts: Vec<&str> = q.visible().iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["e"]);
    }

    #[test]
    fn next_expiry_is_the_earliest() {
        let t0 = Instant::now();
        let mut q = ToastQueue::new();
        assert_eq!(q.next_expiry(), None);
        q.push(ToastKind::Error, "e", t0); // expires t0+8
        q.push(ToastKind::Info, "i", at(t0, 1)); // expires t0+5
        // Earliest of {t0+8, t0+5} is t0+5.
        assert_eq!(q.next_expiry(), Some(at(t0, 1) + DEFAULT_TTL));
    }

    #[test]
    fn dismiss_errors_removes_only_error_toasts() {
        let t0 = Instant::now();
        let mut q = ToastQueue::new();
        q.push(ToastKind::Info, "i", t0);
        q.push(ToastKind::Error, "e1", t0);
        q.push(ToastKind::Error, "e2", t0);
        assert!(q.dismiss_errors(), "errors were present and dismissed");
        let texts: Vec<&str> = q.visible().iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["i"], "info survives, errors are gone");
        assert!(!q.dismiss_errors(), "nothing left to dismiss");
    }

    #[test]
    fn push_evicts_expired_before_capping() {
        let t0 = Instant::now();
        let mut q = ToastQueue::new();
        q.push(ToastKind::Info, "old", t0); // expires t0+4
        // Push three more well after the first expired: 'old' is gone, so we
        // never exceed the cap by keeping a dead toast.
        q.push(ToastKind::Info, "a", at(t0, 10));
        q.push(ToastKind::Info, "b", at(t0, 10));
        q.push(ToastKind::Info, "c", at(t0, 10));
        let texts: Vec<&str> = q.visible().iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["a", "b", "c"]);
    }
}
