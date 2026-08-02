//! Interactive-rebase range construction and plan editing.

use std::collections::HashMap;
use std::fs;

use anyhow::{bail, Context, Result};
use git2::Sort;

use super::*;

impl App {
    pub(crate) fn interactive_rebase_eligible(&self, base: Oid) -> bool {
        let Some(head) = self.repo.head_oid() else {
            return false;
        };
        !self.head_detached
            && head != base
            && self
                .repo
                .repo()
                .graph_descendant_of(head, base)
                .unwrap_or(false)
    }

    pub(crate) fn start_interactive_rebase(&mut self, base: Oid) {
        match self.build_interactive_rebase_plan(base) {
            Ok(plan) => {
                self.rebase_plan = Some(plan);
                self.mode = AppMode::RebasePlan { cursor: 0 };
            }
            Err(error) => {
                self.rebase_plan = None;
                self.mode = AppMode::Normal;
                self.show_error(error.to_string());
            }
        }
    }

    fn build_interactive_rebase_plan(&self, base: Oid) -> Result<RebasePlan> {
        if self.blocked_rebase_reason(base).is_some() {
            bail!(self.blocked_rebase_reason(base).unwrap());
        }
        let repo = self.repo.repo();
        if !crate::git::operations::is_working_tree_clean(repo)? {
            bail!("Commit or stash changes before rebasing");
        }
        let head = self.repo.head_oid().context("HEAD has no commit")?;
        let mut walk = repo.revwalk()?;
        walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
        walk.push(head)?;
        walk.hide(base)?;

        let mut entries = Vec::new();
        for oid in walk {
            let commit = repo.find_commit(oid?)?;
            if commit.parent_count() > 1 {
                bail!("Range contains a merge commit; not supported");
            }
            entries.push(crate::rebase_plan::PlanEntry::pick(
                commit.id(),
                commit.summary().unwrap_or(""),
            ));
        }
        if entries.is_empty() {
            bail!("There are no commits above the selected base");
        }
        Ok(RebasePlan {
            base_oid: base,
            entries,
        })
    }

    fn blocked_rebase_reason(&self, base: Oid) -> Option<&'static str> {
        if self.head_detached {
            return Some("Check out a branch to rebase");
        }
        let head = self.repo.head_oid()?;
        if head == base {
            return Some("There are no commits above the selected base");
        }
        if !self
            .repo
            .repo()
            .graph_descendant_of(head, base)
            .unwrap_or(false)
        {
            return Some("Selected commit is not an ancestor of HEAD");
        }
        None
    }

    pub(crate) fn handle_rebase_plan_action(&mut self, action: Action) -> Result<()> {
        let AppMode::RebasePlan { cursor } = self.mode else {
            return Ok(());
        };
        let len = self
            .rebase_plan
            .as_ref()
            .map_or(0, |plan| plan.entries.len());
        let mut next_cursor = cursor.min(len.saturating_sub(1));
        match action {
            Action::MoveUp => next_cursor = next_cursor.saturating_sub(1),
            Action::MoveDown => next_cursor = (next_cursor + 1).min(len.saturating_sub(1)),
            Action::RebaseMoveCommitUp => {
                if let Some(plan) = self.rebase_plan.as_mut() {
                    match plan.move_up(cursor) {
                        Ok(new_cursor) => next_cursor = new_cursor,
                        Err(error) => self.rebase_edit_error(error),
                    }
                }
            }
            Action::RebaseMoveCommitDown => {
                if let Some(plan) = self.rebase_plan.as_mut() {
                    match plan.move_down(cursor) {
                        Ok(new_cursor) => next_cursor = new_cursor,
                        Err(error) => self.rebase_edit_error(error),
                    }
                }
            }
            Action::RebasePick => self.set_rebase_action(cursor, RebaseAction::Pick),
            Action::RebaseSquash => self.set_rebase_action(cursor, RebaseAction::Squash),
            Action::RebaseFixup => self.set_rebase_action(cursor, RebaseAction::Fixup),
            Action::RebaseDrop => self.set_rebase_action(cursor, RebaseAction::Drop),
            Action::RebaseReword => {
                if let Some(entry) = self
                    .rebase_plan
                    .as_ref()
                    .and_then(|plan| plan.entries.get(cursor))
                {
                    self.mode = AppMode::Input {
                        title: "New commit message".to_string(),
                        input: entry
                            .reword_message
                            .clone()
                            .unwrap_or_else(|| entry.subject.clone()),
                        action: InputAction::RebaseReword { index: cursor },
                    };
                    return Ok(());
                }
            }
            Action::Confirm => {
                if let Some(plan) = self.rebase_plan.clone() {
                    let summary = rebase_summary(&plan);
                    self.mode = AppMode::Confirm {
                        message: summary,
                        action: ConfirmAction::RunInteractiveRebase(plan),
                    };
                    return Ok(());
                }
            }
            Action::Cancel | Action::Quit => {
                self.rebase_plan = None;
                self.mode = AppMode::Normal;
                return Ok(());
            }
            _ => {}
        }
        self.mode = AppMode::RebasePlan {
            cursor: next_cursor,
        };
        Ok(())
    }

    fn set_rebase_action(&mut self, cursor: usize, action: RebaseAction) {
        let result = self
            .rebase_plan
            .as_mut()
            .map(|plan| plan.set_action(cursor, action));
        if let Some(Err(error)) = result {
            self.rebase_edit_error(error);
        }
    }

    fn rebase_edit_error(&mut self, error: crate::rebase_plan::PlanEditError) {
        let message = match error {
            crate::rebase_plan::PlanEditError::NoPreviousCommit => {
                "Squash/fixup needs an older retained commit"
            }
            crate::rebase_plan::PlanEditError::OutOfBounds => "Cannot move this commit further",
            _ => "Invalid rebase plan",
        };
        self.toast(crate::toast::ToastKind::Info, message);
    }

    pub(crate) fn run_interactive_rebase_plan(&mut self, plan: RebasePlan) -> Result<OpOutcome> {
        let state_dir = self.repo.repo().path().join("keifu-interactive-rebase");
        fs::create_dir_all(&state_dir).context("Create interactive rebase state")?;
        let mut paths = HashMap::new();
        for (index, entry) in plan.entries.iter().enumerate() {
            if entry.action != RebaseAction::Reword {
                continue;
            }
            let path = state_dir.join(format!("message-{index}-{}", entry.oid));
            fs::write(
                &path,
                entry
                    .reword_message
                    .as_deref()
                    .context("Reword action has no message")?,
            )?;
            paths.insert(entry.oid, path);
        }
        let todo = plan
            .to_git_todo(&paths)
            .map_err(|error| anyhow::anyhow!("Invalid rebase plan: {error:?}"))?;
        let todo_path = state_dir.join("todo");
        fs::write(&todo_path, todo)?;
        let result = rebase_interactive(&self.repo_path, plan.base_oid, &todo_path);
        self.interactive_rebase_in_progress = self
            .repo
            .repo()
            .path()
            .join("rebase-merge/interactive")
            .exists();
        let outcome = result?;
        if outcome == OpOutcome::Completed {
            let _ = fs::remove_dir_all(state_dir);
        }
        Ok(outcome)
    }

    pub(crate) fn cleanup_interactive_rebase_state(&self) {
        let _ = fs::remove_dir_all(self.repo.repo().path().join("keifu-interactive-rebase"));
    }
}

fn rebase_summary(plan: &RebasePlan) -> String {
    let count = |action| {
        plan.entries
            .iter()
            .filter(|entry| entry.action == action)
            .count()
    };
    format!(
        "Rebase {} commits onto {}?\n\nreword {}, squash {}, fixup {}, drop {}\n\nThis rewrites commit history.",
        plan.entries.len(),
        short_hash(plan.base_oid),
        count(RebaseAction::Reword),
        count(RebaseAction::Squash),
        count(RebaseAction::Fixup),
        count(RebaseAction::Drop),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GitRepository;
    use crate::test_support::git;

    fn app_with_range() -> (tempfile::TempDir, App, Oid, Oid) {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        git(tmp.path(), &["config", "user.name", "Test User"]);
        git(tmp.path(), &["config", "user.email", "test@example.com"]);
        std::fs::write(tmp.path().join("base.txt"), "base\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-q", "-m", "base"]);
        let repo = git2::Repository::open(tmp.path()).unwrap();
        let base = repo.head().unwrap().target().unwrap();
        std::fs::write(tmp.path().join("one.txt"), "one\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-q", "-m", "one"]);
        std::fs::write(tmp.path().join("two.txt"), "two\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-q", "-m", "two"]);
        let head = repo.head().unwrap().target().unwrap();
        drop(repo);
        let app = App::from_repo(GitRepository::open(tmp.path()).unwrap()).unwrap();
        (tmp, app, base, head)
    }

    #[test]
    fn plan_screen_lists_the_real_range_and_cancel_preserves_head() {
        let (_tmp, mut app, base, head) = app_with_range();

        app.start_interactive_rebase(base);

        assert!(matches!(app.mode, AppMode::RebasePlan { cursor: 0 }));
        let plan = app.rebase_plan.as_ref().unwrap();
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].subject, "two");
        assert_eq!(plan.entries[1].subject, "one");

        app.handle_action(Action::Cancel).unwrap();
        assert!(matches!(app.mode, AppMode::Normal));
        assert_eq!(app.repo.head_oid(), Some(head));
    }

    #[test]
    fn plan_actions_are_displayed_before_confirmation_and_cancel_does_not_execute() {
        let (_tmp, mut app, base, head) = app_with_range();
        app.start_interactive_rebase(base);

        app.handle_action(Action::RebaseDrop).unwrap();
        assert_eq!(
            app.rebase_plan.as_ref().unwrap().entries[0].action,
            RebaseAction::Drop
        );
        app.handle_action(Action::Confirm).unwrap();
        let AppMode::Confirm { message, .. } = &app.mode else {
            panic!("Enter must show a review confirmation")
        };
        assert!(
            message.contains("drop 1"),
            "review reflects the plan: {message}"
        );

        app.handle_action(Action::Cancel).unwrap();
        assert!(matches!(app.mode, AppMode::Normal));
        assert_eq!(app.repo.head_oid(), Some(head));
    }
}
