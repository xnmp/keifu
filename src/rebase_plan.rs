//! Pure interactive-rebase plan types.
//!
//! The TUI edits these values, while the Git execution layer consumes them.
//! Keeping the plan independent of both layers makes history-rewrite intent
//! reviewable and testable before any command runs.

use git2::Oid;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    MissingRewordMessage,
    MissingMessageFile,
}

impl RebasePlan {
    /// Whether every combining action has an earlier, retained Git todo entry.
    pub fn is_valid(&self) -> bool {
        let mut has_previous = false;
        for entry in self.entries.iter().rev() {
            match entry.action {
                RebaseAction::Drop => {}
                RebaseAction::Squash | RebaseAction::Fixup if !has_previous => return false,
                _ => has_previous = true,
            }
        }
        true
    }

    pub fn set_action(&mut self, index: usize, action: RebaseAction) -> Result<(), PlanEditError> {
        let Some(entry) = self.entries.get_mut(index) else {
            return Err(PlanEditError::OutOfBounds);
        };
        let old_action = entry.action;
        let old_message = entry.reword_message.take();
        entry.action = action;
        if self.is_valid() {
            Ok(())
        } else {
            let entry = &mut self.entries[index];
            entry.action = old_action;
            entry.reword_message = old_message;
            Err(PlanEditError::NoPreviousCommit)
        }
    }

    pub fn set_reword_message(
        &mut self,
        index: usize,
        message: impl Into<String>,
    ) -> Result<(), PlanEditError> {
        let Some(entry) = self.entries.get_mut(index) else {
            return Err(PlanEditError::OutOfBounds);
        };
        let message = message.into();
        if message.trim().is_empty() {
            return Err(PlanEditError::MissingRewordMessage);
        }
        entry.action = RebaseAction::Reword;
        entry.reword_message = Some(message);
        Ok(())
    }

    pub fn move_up(&mut self, index: usize) -> Result<usize, PlanEditError> {
        if index == 0 || index >= self.entries.len() {
            return Err(PlanEditError::OutOfBounds);
        }
        self.swap_if_valid(index, index - 1)?;
        Ok(index - 1)
    }

    pub fn move_down(&mut self, index: usize) -> Result<usize, PlanEditError> {
        if index + 1 >= self.entries.len() {
            return Err(PlanEditError::OutOfBounds);
        }
        self.swap_if_valid(index, index + 1)?;
        Ok(index + 1)
    }

    fn swap_if_valid(&mut self, left: usize, right: usize) -> Result<(), PlanEditError> {
        self.entries.swap(left, right);
        if self.is_valid() {
            Ok(())
        } else {
            self.entries.swap(left, right);
            Err(PlanEditError::NoPreviousCommit)
        }
    }

    /// Serialize newest-first display entries into Git's oldest-first todo.
    pub fn to_git_todo(
        &self,
        message_paths: &HashMap<Oid, PathBuf>,
    ) -> Result<String, PlanEditError> {
        if !self.is_valid() {
            return Err(PlanEditError::NoPreviousCommit);
        }
        let mut todo = String::new();
        for entry in self.entries.iter().rev() {
            match entry.action {
                RebaseAction::Drop => continue,
                RebaseAction::Pick | RebaseAction::Reword => {
                    todo.push_str(&format!("pick {} {}\n", entry.oid, entry.subject));
                    if entry.action == RebaseAction::Reword {
                        if entry.reword_message.is_none() {
                            return Err(PlanEditError::MissingRewordMessage);
                        }
                        let path = message_paths
                            .get(&entry.oid)
                            .ok_or(PlanEditError::MissingMessageFile)?;
                        todo.push_str(&format!(
                            "exec git commit --amend -F {}\n",
                            shell_quote(path)
                        ));
                    }
                }
                RebaseAction::Squash | RebaseAction::Fixup => {
                    todo.push_str(&format!(
                        "{} {} {}\n",
                        entry.action.label(),
                        entry.oid,
                        entry.subject
                    ));
                }
            }
        }
        Ok(todo)
    }
}

/// Quote a path for the POSIX shell used by Git's editor/exec handling.
pub fn shell_quote(path: &Path) -> String {
    let text = path.to_string_lossy();
    format!("'{}'", text.replace('\'', "'\"'\"'"))
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
                "pick {} keep me\npick {} rename me\nexec git commit --amend -F '/tmp/rebase message.txt'\nsquash {} squash me\nfixup {} fixup me\ndrop {} drop me\n",
                oid(1),
                oid(2),
                oid(3),
                oid(4),
                oid(5)
            )
        );
    }

    #[test]
    fn rebase_plan_serializes_an_all_drop_plan_as_explicit_drop_commands() {
        let mut plan = plan();
        for index in 0..plan.entries.len() {
            plan.set_action(index, RebaseAction::Drop).unwrap();
        }

        let todo = plan.to_git_todo(&HashMap::new()).unwrap();

        assert_eq!(
            todo,
            format!(
                "drop {} first\ndrop {} second\ndrop {} third\n",
                oid(1),
                oid(2),
                oid(3)
            )
        );
    }
}
