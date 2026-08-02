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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;

    fn oid(n: u8) -> Oid {
        Oid::from_bytes(&[n; 20]).unwrap()
    }

    fn plan() -> RebasePlan {
        RebasePlan {
            base_oid: oid(9),
            entries: vec![
                PlanEntry::pick(oid(3), "third"),
                PlanEntry::pick(oid(2), "second"),
                PlanEntry::pick(oid(1), "first"),
            ],
        }
    }

    #[test]
    fn rebase_plan_reorders_displayed_commits_without_losing_actions() {
        let mut plan = plan();
        plan.set_action(0, RebaseAction::Squash).unwrap();

        plan.move_down(0).unwrap();

        assert_eq!(plan.entries[0].subject, "second");
        assert_eq!(plan.entries[1].subject, "third");
        assert_eq!(plan.entries[1].action, RebaseAction::Squash);
        assert!(plan.is_valid());
    }

    #[test]
    fn rebase_plan_rejects_actions_and_edits_that_remove_the_squash_target() {
        let mut plan = plan();
        assert_eq!(
            plan.set_action(2, RebaseAction::Fixup),
            Err(PlanEditError::NoPreviousCommit)
        );
        assert_eq!(plan.entries[2].action, RebaseAction::Pick);

        plan.set_action(1, RebaseAction::Squash).unwrap();
        assert_eq!(
            plan.set_action(2, RebaseAction::Drop),
            Err(PlanEditError::NoPreviousCommit)
        );
        assert_eq!(plan.entries[2].action, RebaseAction::Pick);
        assert!(plan.is_valid());
    }

    #[test]
    fn rebase_plan_serializes_the_displayed_actions_in_git_replay_order() {
        let mut plan = RebasePlan {
            base_oid: oid(9),
            entries: vec![
                PlanEntry::pick(oid(5), "drop me"),
                PlanEntry::pick(oid(4), "fixup me"),
                PlanEntry::pick(oid(3), "squash me"),
                PlanEntry::pick(oid(2), "rename me"),
                PlanEntry::pick(oid(1), "keep me"),
            ],
        };
        plan.set_action(0, RebaseAction::Drop).unwrap();
        plan.set_action(1, RebaseAction::Fixup).unwrap();
        plan.set_action(2, RebaseAction::Squash).unwrap();
        plan.set_reword_message(3, "replacement\n\nbody").unwrap();
        let message_path = PathBuf::from("/tmp/rebase message.txt");
        let paths = HashMap::from([(oid(2), message_path)]);

        let todo = plan.to_git_todo(&paths).unwrap();

        assert_eq!(
            todo,
            format!(
                "pick {} keep me\npick {} rename me\nexec git commit --amend -F '/tmp/rebase message.txt'\nsquash {} squash me\nfixup {} fixup me\n",
                oid(1),
                oid(2),
                oid(3),
                oid(4)
            )
        );
    }
}
