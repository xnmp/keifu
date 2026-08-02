//! Pure interactive-rebase plan types.
//!
//! The TUI edits these values, while the Git execution layer consumes them.
//! Keeping the plan independent of both layers makes history-rewrite intent
//! reviewable and testable before any command runs.

use git2::Oid;

/// An action applied to one commit in an interactive rebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseAction {
    Pick,
    Reword,
    Squash,
    Fixup,
    Drop,
}

impl RebaseAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Reword => "reword",
            Self::Squash => "squash",
            Self::Fixup => "fixup",
            Self::Drop => "drop",
        }
    }
}

/// One displayed row in a rebase plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub oid: Oid,
    pub subject: String,
    pub action: RebaseAction,
    /// Replacement full commit message, present only for `Reword`.
    pub reword_message: Option<String>,
}

impl PlanEntry {
    pub fn pick(oid: Oid, subject: impl Into<String>) -> Self {
        Self {
            oid,
            subject: subject.into(),
            action: RebaseAction::Pick,
            reword_message: None,
        }
    }
}

/// An editable plan. Entries are stored in display order: newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebasePlan {
    pub base_oid: Oid,
    pub entries: Vec<PlanEntry>,
}

/// Why a requested plan edit was not applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanEditError {
    OutOfBounds,
    NoPreviousCommit,
}
