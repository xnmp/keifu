//! Session undo ledger: an explicit record of reversible graph operations.
//!
//! Each mutating op keifu performs records an [`UndoEntry`] describing how to
//! reverse it (`plan`) and what the repo must currently look like for the
//! reversal to be safe (`check`). Ctrl+Z pops the newest entry, re-verifies it
//! against the live repo, and — only if it still matches — runs the inverse.
//! A mismatch (the ref moved since) drops the entry rather than guessing.
//!
//! This module is the pure core: the entry/plan/check types and the bounded
//! LIFO ledger. Verification and inverse execution (which touch git2) live on
//! the `App`. There is deliberately no redo stack.

use git2::Oid;

/// Maximum entries retained; older ones are dropped on overflow.
pub const UNDO_LEDGER_CAP: usize = 50;

/// The 7-char short form of an OID, for confirmation messages.
pub fn short_oid(oid: Oid) -> String {
    oid.to_string().chars().take(7).collect()
}

/// How to reverse an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoPlan {
    /// Recreate a deleted branch at `oid`.
    RecreateBranch { name: String, oid: Oid },
    /// Recreate a deleted tag as a lightweight tag at `oid`. `was_annotated`
    /// only drives the confirmation wording (the recreation is always
    /// lightweight — we don't reconstruct the original tag object/message).
    RecreateTag {
        name: String,
        oid: Oid,
        was_annotated: bool,
    },
    /// Hard-reset HEAD back to `to` (undo of a merge / fast-forward pull).
    ResetHard { to: Oid },
    /// Rename branch `from` back to `to` (undo of a rename).
    RenameBranch { from: String, to: String },
}

/// What the live repo must satisfy for the undo to be safe to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoCheck {
    /// The branch is still absent (nothing recreated it since the delete).
    BranchAbsent(String),
    /// The tag is still absent.
    TagAbsent(String),
    /// HEAD is still exactly at `oid` (the op's result) and the tree is clean —
    /// so the reset neither loses newer commits nor clobbers uncommitted work.
    HeadAtCleanTree(Oid),
    /// After a rename, `exists` is present and `absent` is gone.
    RenameConsistent { exists: String, absent: String },
}

/// One reversible operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoEntry {
    /// Past-tense op label, e.g. "Delete branch 'feature'".
    pub description: String,
    /// The full confirmation prompt shown before undoing, e.g.
    /// "Undo: delete branch 'feature' → recreate at abc1234?".
    pub confirm: String,
    pub plan: UndoPlan,
    pub check: UndoCheck,
}

/// A bounded LIFO stack of undo entries (newest last). Session-only.
#[derive(Debug, Default)]
pub struct UndoLedger {
    entries: Vec<UndoEntry>,
}

impl UndoLedger {
    /// Record a new entry as the newest, dropping the oldest past the cap.
    pub fn record(&mut self, entry: UndoEntry) {
        self.entries.push(entry);
        if self.entries.len() > UNDO_LEDGER_CAP {
            let overflow = self.entries.len() - UNDO_LEDGER_CAP;
            self.entries.drain(0..overflow);
        }
    }

    /// The newest entry without removing it.
    pub fn peek(&self) -> Option<&UndoEntry> {
        self.entries.last()
    }

    /// Remove and return the newest entry.
    pub fn pop(&mut self) -> Option<UndoEntry> {
        self.entries.pop()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(b: u8) -> Oid {
        Oid::from_bytes(&[b; 20]).unwrap()
    }

    fn entry(name: &str) -> UndoEntry {
        UndoEntry {
            description: format!("Delete branch '{name}'"),
            confirm: format!("Undo: delete branch '{name}'?"),
            plan: UndoPlan::RecreateBranch {
                name: name.to_string(),
                oid: oid(1),
            },
            check: UndoCheck::BranchAbsent(name.to_string()),
        }
    }

    #[test]
    fn ledger_is_lifo() {
        let mut led = UndoLedger::default();
        assert!(led.is_empty());
        led.record(entry("a"));
        led.record(entry("b"));
        assert_eq!(led.len(), 2);
        assert_eq!(led.peek().unwrap().description, "Delete branch 'b'");
        assert_eq!(led.pop().unwrap().description, "Delete branch 'b'");
        assert_eq!(led.pop().unwrap().description, "Delete branch 'a'");
        assert!(led.pop().is_none());
        assert!(led.is_empty());
    }

    #[test]
    fn ledger_caps_at_the_limit_dropping_oldest() {
        let mut led = UndoLedger::default();
        for i in 0..(UNDO_LEDGER_CAP + 10) {
            led.record(entry(&format!("b{i}")));
        }
        assert_eq!(led.len(), UNDO_LEDGER_CAP);
        // Newest retained.
        assert_eq!(
            led.peek().unwrap().description,
            format!("Delete branch 'b{}'", UNDO_LEDGER_CAP + 9)
        );
        // Oldest 10 dropped: the current oldest is entry #10.
        let mut led2 = UndoLedger::default();
        for i in 0..(UNDO_LEDGER_CAP + 10) {
            led2.record(entry(&format!("b{i}")));
        }
        while led2.len() > 1 {
            led2.pop();
        }
        assert_eq!(led2.pop().unwrap().description, "Delete branch 'b10'");
    }
}
