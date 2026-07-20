//! `SignatureGuarded<T>`: a value paired with a cheap fingerprint of the inputs
//! it was derived from, so a frequent recompute does no work when nothing
//! relevant changed.
//!
//! This is the reusable form of the hand-rolled "compute a `DefaultHasher`
//! signature, bail if unchanged, else recompute" guard that several derived
//! caches share. The value and its signature live together, so a caller can't
//! accidentally update one without the other (the failure mode of keeping them
//! as two separate fields).

use std::hash::{Hash, Hasher};

/// Hash a set of inputs into a single `u64` fingerprint. Inputs whose iteration
/// order is stable can be hashed directly; order-independent inputs should be
/// folded (e.g. sorted or XOR-accumulated) by the caller before hashing.
pub fn signature(inputs: impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    inputs.hash(&mut h);
    h.finish()
}

/// A derived `value` guarded by the signature of the inputs it was computed
/// from. `recompute_if_changed` runs the (potentially expensive) closure only
/// when the signature differs from the last one seen.
#[derive(Debug, Clone)]
pub struct SignatureGuarded<T> {
    sig: Option<u64>,
    value: T,
}

impl<T: Default> Default for SignatureGuarded<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> SignatureGuarded<T> {
    /// A guard holding `value` with no signature yet, so the first
    /// `recompute_if_changed` always runs.
    pub fn new(value: T) -> Self {
        Self { sig: None, value }
    }

    /// The current derived value.
    pub fn value(&self) -> &T {
        &self.value
    }

    /// Recompute `value` via `f` only when `inputs` hash to a signature that
    /// differs from the last one seen; otherwise leave `value` untouched.
    /// Returns `true` when it recomputed.
    pub fn recompute_if_changed(&mut self, inputs: impl Hash, f: impl FnOnce() -> T) -> bool {
        let sig = signature(inputs);
        if self.sig == Some(sig) {
            return false;
        }
        self.sig = Some(sig);
        self.value = f();
        true
    }

    /// Replace `value` and forget the signature, so the next
    /// `recompute_if_changed` always runs. Used when the inputs go away
    /// entirely (e.g. no base branch), where there's nothing to hash.
    pub fn reset(&mut self, value: T) {
        self.sig = None;
        self.value = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recomputes_on_first_call() {
        let mut g = SignatureGuarded::new(0u32);
        let ran = g.recompute_if_changed(1u32, || 42);
        assert!(ran);
        assert_eq!(*g.value(), 42);
    }

    #[test]
    fn skips_recompute_when_signature_unchanged() {
        let mut g = SignatureGuarded::new(0u32);
        g.recompute_if_changed(1u32, || 42);
        let mut calls = 0;
        let ran = g.recompute_if_changed(1u32, || {
            calls += 1;
            99
        });
        assert!(!ran);
        assert_eq!(calls, 0, "closure must not run when inputs unchanged");
        assert_eq!(*g.value(), 42, "value preserved when inputs unchanged");
    }

    #[test]
    fn recomputes_when_signature_changes() {
        let mut g = SignatureGuarded::new(0u32);
        g.recompute_if_changed(1u32, || 42);
        let ran = g.recompute_if_changed(2u32, || 99);
        assert!(ran);
        assert_eq!(*g.value(), 99);
    }

    #[test]
    fn reset_forgets_signature_so_next_recompute_runs() {
        let mut g = SignatureGuarded::new(0u32);
        g.recompute_if_changed(1u32, || 42);
        g.reset(7);
        assert_eq!(*g.value(), 7);
        // Same inputs as before the reset must still recompute.
        let ran = g.recompute_if_changed(1u32, || 100);
        assert!(ran);
        assert_eq!(*g.value(), 100);
    }

    #[test]
    fn default_starts_with_no_signature() {
        let mut g: SignatureGuarded<u32> = SignatureGuarded::default();
        assert_eq!(*g.value(), 0);
        assert!(g.recompute_if_changed(0u32, || 5));
    }
}
