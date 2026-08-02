//! Interactive-rebase range construction and plan editing.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::path::Path;

use anyhow::{bail, Context, Result};
use fs2::FileExt;
use git2::Sort;
use serde::{Deserialize, Serialize};

use super::*;

const INTERACTIVE_REBASE_UNDO_FILE: &str = "pending-undo.json";

#[derive(Debug, Serialize, Deserialize)]
struct PendingInteractiveRebaseUndo {
    pre_head: String,
    base: String,
    commit_count: usize,
}

pub(super) struct InteractiveRebaseStateOwner(File);

impl Drop for InteractiveRebaseStateOwner {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub(super) fn acquire_interactive_rebase_state(
    git_dir: &Path,
) -> Result<InteractiveRebaseStateOwner> {
    let lock_path = git_dir.join("keifu-interactive-rebase.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .context("Open interactive rebase state lock")?;
    lock.try_lock_exclusive().with_context(|| {
        format!(
            "Another interactive rebase is starting ({})",
            lock_path.display()
        )
    })?;
    Ok(InteractiveRebaseStateOwner(lock))
}

pub(super) fn reconcile_interactive_rebase_state(git_dir: &Path) {
    let Ok(lock) = acquire_interactive_rebase_state(git_dir) else {
        // A live starter owns the pre-marker state. It will reconcile after Git
        // returns; another app instance must not remove files out from under it.
        return;
    };
    if !git_dir.join("rebase-merge/interactive").exists() {
        let _ = fs::remove_dir_all(git_dir.join("keifu-interactive-rebase"));
    }
    drop(lock);
}

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
        let head_ref = repo.head().context("Read current branch")?;
        let source_branch_ref = head_ref
            .name()
            .context("Current branch has no reference name")?
            .to_owned();
        let head = head_ref.target().context("HEAD has no commit")?;
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
            source_branch_ref,
            source_head_oid: head,
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
        let git_dir = self.repo.repo().path().to_path_buf();
        let _state_owner = acquire_interactive_rebase_state(&git_dir)?;
        if git_dir.join("rebase-merge/interactive").exists() {
            bail!("An interactive rebase is already in progress");
        }
        let state_dir = git_dir.join("keifu-interactive-rebase");
        let result = (|| {
            if state_dir.exists() {
                fs::remove_dir_all(&state_dir).context("Remove stale interactive rebase state")?;
            }
            fs::create_dir_all(&state_dir).context("Create interactive rebase state")?;
            let pre_head = self.repo.head_oid().context("HEAD has no commit")?;
            let pending_undo = PendingInteractiveRebaseUndo {
                pre_head: pre_head.to_string(),
                base: plan.base_oid.to_string(),
                commit_count: plan.entries.len(),
            };
            fs::write(
                state_dir.join(INTERACTIVE_REBASE_UNDO_FILE),
                serde_json::to_vec(&pending_undo)?,
            )
            .context("Write interactive rebase undo state")?;
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
            rebase_interactive(&self.repo_path, plan.base_oid, &todo_path)
        })();
        self.interactive_rebase_in_progress = self
            .repo
            .repo()
            .path()
            .join("rebase-merge/interactive")
            .exists();
        if matches!(&result, Ok(OpOutcome::Completed)) {
            self.record_completed_interactive_rebase_undo();
        }
        if !self.interactive_rebase_in_progress {
            let _ = fs::remove_dir_all(&state_dir);
        }
        result
    }

    pub(crate) fn record_completed_interactive_rebase_undo(&mut self) {
        let path = self
            .repo
            .repo()
            .path()
            .join("keifu-interactive-rebase")
            .join(INTERACTIVE_REBASE_UNDO_FILE);
        if !path.exists() {
            return;
        }
        let entry = (|| -> Result<Option<crate::undo::UndoEntry>> {
            let pending: PendingInteractiveRebaseUndo =
                serde_json::from_slice(&fs::read(&path).context("Read rebase undo state")?)
                    .context("Parse rebase undo state")?;
            let pre = Oid::from_str(&pending.pre_head).context("Parse pre-rebase HEAD")?;
            let base = Oid::from_str(&pending.base).context("Parse rebase base")?;
            let post = self.repo.head_oid().context("HEAD has no commit")?;
            if pre == post {
                return Ok(None);
            }
            Ok(Some(crate::undo::UndoEntry {
                description: format!(
                    "Interactive rebase ({} commits onto {})",
                    pending.commit_count,
                    short_hash(base)
                ),
                confirm: format!("Undo: rebase → reset to {}?", short_hash(pre)),
                plan: crate::undo::UndoPlan::ResetHard { to: pre },
                check: crate::undo::UndoCheck::HeadAtCleanTree(post),
            }))
        })();
        match entry {
            Ok(Some(entry)) => self.record_undo(entry),
            Ok(None) => {}
            Err(error) => self.show_error(format!(
                "Rebase completed, but undo is unavailable: {error}"
            )),
        }
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
        "Rebase {} commits onto {}?\nActions: reword {}, squash {}, fixup {}, drop {}\nWARNING: this rewrites commit history.",
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

    fn app_with_conflicting_range() -> (tempfile::TempDir, App, Oid, Oid) {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        git(tmp.path(), &["config", "user.name", "Test User"]);
        git(tmp.path(), &["config", "user.email", "test@example.com"]);
        std::fs::write(tmp.path().join("f.txt"), "base\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-q", "-m", "base"]);
        let repo = git2::Repository::open(tmp.path()).unwrap();
        let base = repo.head().unwrap().target().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "one\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-q", "-m", "one"]);
        std::fs::write(tmp.path().join("f.txt"), "two\n").unwrap();
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

    #[test]
    fn confirmed_plan_refreshes_history_and_toasts_success() {
        let (tmp, mut app, base, _head) = app_with_range();
        app.start_interactive_rebase(base);
        app.rebase_plan
            .as_mut()
            .unwrap()
            .set_reword_message(0, "renamed two")
            .unwrap();
        let plan = app.rebase_plan.clone().unwrap();
        app.mode = AppMode::Confirm {
            message: rebase_summary(&plan),
            action: ConfirmAction::RunInteractiveRebase(plan),
        };

        app.handle_action(Action::Confirm).unwrap();

        assert!(matches!(app.mode, AppMode::Normal));
        assert_eq!(
            app.repo
                .repo()
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .summary(),
            Some("renamed two")
        );
        assert!(app.toasts.visible().iter().any(|toast| {
            toast.kind == crate::toast::ToastKind::Success
                && toast.text.contains("Rebase completed")
        }));
        assert!(
            !tmp.path().join(".git/keifu-interactive-rebase").exists(),
            "completed rebase must remove durable execution state"
        );
    }

    #[test]
    fn confirmed_plan_refuses_an_external_commit_after_review() {
        let (tmp, mut app, base, _reviewed_head) = app_with_range();
        app.start_interactive_rebase(base);
        let plan = app.rebase_plan.clone().unwrap();
        app.mode = AppMode::Confirm {
            message: rebase_summary(&plan),
            action: ConfirmAction::RunInteractiveRebase(plan),
        };
        std::fs::write(tmp.path().join("external.txt"), "external\n").unwrap();
        git(tmp.path(), &["add", "external.txt"]);
        git(tmp.path(), &["commit", "-q", "-m", "external"]);
        let external_head = git2::Repository::open(tmp.path())
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap();

        app.handle_action(Action::Confirm).unwrap();

        let live_repo = git2::Repository::open(tmp.path()).unwrap();
        assert_eq!(live_repo.head().unwrap().target(), Some(external_head));
        assert!(matches!(app.mode, AppMode::Normal));
        assert!(app.toasts.visible().iter().any(|toast| {
            toast.kind == crate::toast::ToastKind::Error
                && toast.text.contains("HEAD changed since the plan was reviewed")
        }));
        assert!(!tmp.path().join(".git/keifu-interactive-rebase").exists());
    }

    #[test]
    fn confirmed_plan_refuses_a_branch_switch_after_review() {
        let (tmp, mut app, base, reviewed_head) = app_with_range();
        app.start_interactive_rebase(base);
        let plan = app.rebase_plan.clone().unwrap();
        app.mode = AppMode::Confirm {
            message: rebase_summary(&plan),
            action: ConfirmAction::RunInteractiveRebase(plan),
        };
        git(tmp.path(), &["checkout", "-q", "-b", "other"]);

        app.handle_action(Action::Confirm).unwrap();

        let live_repo = git2::Repository::open(tmp.path()).unwrap();
        assert_eq!(live_repo.head().unwrap().name(), Some("refs/heads/other"));
        assert_eq!(live_repo.head().unwrap().target(), Some(reviewed_head));
        assert!(matches!(app.mode, AppMode::Normal));
        assert!(app.toasts.visible().iter().any(|toast| {
            toast.kind == crate::toast::ToastKind::Error
                && toast
                    .text
                    .contains("Branch changed since the plan was reviewed")
        }));
        assert!(!tmp.path().join(".git/keifu-interactive-rebase").exists());
    }

    #[test]
    fn startup_removes_orphaned_interactive_rebase_state() {
        let (tmp, app, _base, _head) = app_with_range();
        drop(app);
        let state_dir = tmp.path().join(".git/keifu-interactive-rebase");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join("message-0"), "secret rewrite message").unwrap();

        let app = App::from_repo(GitRepository::open(tmp.path()).unwrap()).unwrap();

        assert!(!app.interactive_rebase_in_progress);
        assert!(
            !state_dir.exists(),
            "startup must reconcile durable state when Git has no interactive rebase"
        );
    }

    #[test]
    fn refresh_switches_from_cli_to_libgit2_rebase_recovery_in_one_session() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        git(tmp.path(), &["config", "user.name", "Test User"]);
        git(tmp.path(), &["config", "user.email", "test@example.com"]);
        std::fs::write(tmp.path().join("f.txt"), "base\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-q", "-m", "base"]);
        git(tmp.path(), &["branch", "upstream"]);
        let repo = git2::Repository::open(tmp.path()).unwrap();
        let base = repo.head().unwrap().target().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "main\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-q", "-m", "main"]);
        let main_head = repo.head().unwrap().target().unwrap();
        let todo = repo.path().join("pause-todo");
        std::fs::write(
            &todo,
            format!(
                "pick {main_head} main\nexec git rev-parse --verify refs/heads/does-not-exist\n"
            ),
        )
        .unwrap();
        let mut app = App::from_repo(GitRepository::open(tmp.path()).unwrap()).unwrap();

        assert_eq!(
            crate::git::operations::rebase_interactive(&app.repo_path, base, &todo).unwrap(),
            OpOutcome::Paused
        );
        app.refresh(true).unwrap();
        assert!(app.interactive_rebase_in_progress);
        let state_dir = tmp.path().join(".git/keifu-interactive-rebase");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join("message-0"), "durable message").unwrap();
        crate::git::operations::abort_interactive_rebase(&app.repo_path).unwrap();
        app.refresh(true).unwrap();
        assert!(!app.interactive_rebase_in_progress);
        assert!(
            !state_dir.exists(),
            "refresh after an external abort must remove orphaned execution state"
        );

        git(tmp.path(), &["checkout", "-q", "upstream"]);
        std::fs::write(tmp.path().join("f.txt"), "upstream\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-q", "-m", "upstream"]);
        git(tmp.path(), &["checkout", "-q", "main"]);
        app.refresh(true).unwrap();
        assert!(matches!(
            rebase_branch(app.repo.repo(), "upstream", git2::BranchType::Local).unwrap(),
            OpOutcome::Conflicts { .. }
        ));
        app.refresh(true).unwrap();
        assert_eq!(app.op_state, OperationState::Rebase);
        assert!(!app.interactive_rebase_in_progress);
        app.mode = AppMode::Confirm {
            message: "Abort rebase?".to_string(),
            action: ConfirmAction::AbortOperation(OperationState::Rebase),
        };

        app.handle_action(Action::Confirm).unwrap();

        assert_eq!(app.repo.head_oid(), Some(main_head));
        assert_eq!(app.op_state, OperationState::Clean);
    }

    #[test]
    fn confirmed_conflicting_plan_enters_the_existing_conflict_workflow() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        git(tmp.path(), &["config", "user.name", "Test User"]);
        git(tmp.path(), &["config", "user.email", "test@example.com"]);
        std::fs::write(tmp.path().join("f.txt"), "base\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-q", "-m", "base"]);
        let repo = git2::Repository::open(tmp.path()).unwrap();
        let base = repo.head().unwrap().target().unwrap();
        std::fs::write(tmp.path().join("f.txt"), "one\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-q", "-m", "one"]);
        std::fs::write(tmp.path().join("f.txt"), "two\n").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-q", "-m", "two"]);
        drop(repo);
        let mut app = App::from_repo(GitRepository::open(tmp.path()).unwrap()).unwrap();
        app.start_interactive_rebase(base);
        app.rebase_plan.as_mut().unwrap().move_down(0).unwrap();
        let plan = app.rebase_plan.clone().unwrap();
        app.mode = AppMode::Confirm {
            message: rebase_summary(&plan),
            action: ConfirmAction::RunInteractiveRebase(plan),
        };

        app.handle_action(Action::Confirm).unwrap();

        assert_eq!(app.op_state, OperationState::Rebase);
        assert!(app.interactive_rebase_in_progress);
        assert!(
            tmp.path().join(".git/keifu-interactive-rebase").exists(),
            "a paused CLI rebase must retain its todo and reword state"
        );
        assert_eq!(app.conflict_count, 1);
        assert_eq!(app.focused_panel, FocusedPanel::Files);
        assert!(app.get_message().unwrap().contains("Conflicts in 1 file"));
        app.mode = AppMode::Confirm {
            message: "Abort rebase?".to_string(),
            action: ConfirmAction::AbortOperation(OperationState::Rebase),
        };
        app.handle_action(Action::Confirm).unwrap();
        assert!(
            !tmp.path().join(".git/keifu-interactive-rebase").exists(),
            "app-driven abort must remove durable reword/todo state"
        );
    }

    #[test]
    fn continued_interactive_rebase_after_restart_can_be_undone() {
        let (tmp, mut app, base, original_head) = app_with_conflicting_range();
        app.start_interactive_rebase(base);
        app.rebase_plan.as_mut().unwrap().move_down(0).unwrap();
        let plan = app.rebase_plan.clone().unwrap();
        app.mode = AppMode::Confirm {
            message: rebase_summary(&plan),
            action: ConfirmAction::RunInteractiveRebase(plan),
        };
        app.handle_action(Action::Confirm).unwrap();
        assert!(app.interactive_rebase_in_progress);
        drop(app);

        std::fs::write(tmp.path().join("f.txt"), "one\n").unwrap();
        git(tmp.path(), &["add", "f.txt"]);
        let mut resumed = App::from_repo(GitRepository::open(tmp.path()).unwrap()).unwrap();
        assert!(tmp
            .path()
            .join(".git/keifu-interactive-rebase/pending-undo.json")
            .exists());
        resumed.focused_panel = FocusedPanel::Files;
        resumed.handle_action(Action::ContinueOperation).unwrap();
        if resumed.interactive_rebase_in_progress {
            resumed.handle_action(Action::ContinueOperation).unwrap();
        }
        let rebased_head = resumed.repo.head_oid().unwrap();
        assert_ne!(rebased_head, original_head);
        assert_eq!(
            resumed.undo_ledger.len(),
            1,
            "toasts: {:?}",
            resumed
                .toasts
                .visible()
                .iter()
                .map(|toast| toast.text.as_str())
                .collect::<Vec<_>>()
        );

        resumed.focused_panel = FocusedPanel::Graph;
        resumed.handle_action(Action::UndoLastOp).unwrap();
        let AppMode::Confirm { message, .. } = &resumed.mode else {
            panic!("completed Continue must make the rebase undoable")
        };
        assert!(message.contains(&short_hash(original_head)));
        resumed.handle_action(Action::Confirm).unwrap();

        assert_eq!(resumed.repo.head_oid(), Some(original_head));
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
            "two\n"
        );
    }

    #[test]
    fn second_instance_cannot_overwrite_pending_reword_of_paused_rebase() {
        let (tmp, mut first_app, base, _original_head) = app_with_conflicting_range();
        first_app.start_interactive_rebase(base);
        first_app
            .rebase_plan
            .as_mut()
            .unwrap()
            .move_down(0)
            .unwrap();
        first_app
            .rebase_plan
            .as_mut()
            .unwrap()
            .set_reword_message(0, "original pending message")
            .unwrap();
        let first_plan = first_app.rebase_plan.clone().unwrap();
        let mut second_plan = first_plan.clone();
        second_plan
            .set_reword_message(0, "overwritten by second instance")
            .unwrap();
        first_app.mode = AppMode::Confirm {
            message: rebase_summary(&first_plan),
            action: ConfirmAction::RunInteractiveRebase(first_plan),
        };
        first_app.handle_action(Action::Confirm).unwrap();
        assert!(first_app.interactive_rebase_in_progress);

        let state_dir = tmp.path().join(".git/keifu-interactive-rebase");
        let message_path = std::fs::read_dir(&state_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("message-")
            })
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&message_path).unwrap(),
            "original pending message"
        );
        let mut second_app = App::from_repo(GitRepository::open(tmp.path()).unwrap()).unwrap();

        let second_start = second_app.run_interactive_rebase_plan(second_plan);

        assert!(second_start
            .unwrap_err()
            .to_string()
            .contains("already in progress"));
        assert_eq!(
            std::fs::read_to_string(&message_path).unwrap(),
            "original pending message"
        );
        std::fs::write(tmp.path().join("f.txt"), "one\n").unwrap();
        git(tmp.path(), &["add", "f.txt"]);
        first_app.handle_action(Action::ContinueOperation).unwrap();
        if first_app.interactive_rebase_in_progress {
            first_app.handle_action(Action::ContinueOperation).unwrap();
        }
        assert_eq!(
            first_app
                .repo
                .repo()
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .summary(),
            Some("original pending message")
        );
    }

    #[cfg(unix)]
    #[test]
    fn completion_handoff_blocks_a_new_plan_until_undo_and_cleanup_finish() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::mpsc;
        use std::time::Duration;

        let (tmp, mut first_app, base, original_head) = app_with_conflicting_range();
        first_app.start_interactive_rebase(base);
        first_app
            .rebase_plan
            .as_mut()
            .unwrap()
            .move_down(0)
            .unwrap();
        let plan = first_app.rebase_plan.clone().unwrap();
        first_app.mode = AppMode::Confirm {
            message: rebase_summary(&plan),
            action: ConfirmAction::RunInteractiveRebase(plan.clone()),
        };
        first_app.handle_action(Action::Confirm).unwrap();
        assert!(first_app.interactive_rebase_in_progress);
        drop(first_app);

        let state_dir = tmp.path().join(".git/keifu-interactive-rebase");
        let undo_path = state_dir.join(INTERACTIVE_REBASE_UNDO_FILE);
        let undo_seed = std::fs::read(&undo_path).unwrap();
        let original_todo = std::fs::read(state_dir.join("todo")).unwrap();
        std::fs::remove_file(&undo_path).unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(&undo_path)
            .status()
            .unwrap()
            .success());
        std::fs::write(tmp.path().join("f.txt"), "one\n").unwrap();
        git(tmp.path(), &["add", "f.txt"]);
        let hook = tmp.path().join(".git/hooks/pre-rebase");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook, permissions).unwrap();

        let mut competing_app = App::from_repo(GitRepository::open(tmp.path()).unwrap()).unwrap();
        let (reader_open_tx, reader_open_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let mut fifo = OpenOptions::new().write(true).open(undo_path).unwrap();
            reader_open_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            fifo.write_all(&undo_seed).unwrap();
        });
        let repo_path = tmp.path().to_path_buf();
        let worker = std::thread::spawn(move || {
            let mut resumed = App::from_repo(GitRepository::open(&repo_path).unwrap()).unwrap();
            resumed.focused_panel = FocusedPanel::Files;
            for _ in 0..3 {
                resumed.handle_action(Action::ContinueOperation).unwrap();
                if !resumed.interactive_rebase_in_progress {
                    break;
                }
            }
            (resumed.undo_ledger.len(), resumed.repo.head_oid())
        });
        let marker = tmp.path().join(".git/rebase-merge/interactive");
        reader_open_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        let marker_removed_during_handoff = !marker.exists();
        let _startup_app = App::from_repo(GitRepository::open(tmp.path()).unwrap()).unwrap();
        let todo_after_startup = std::fs::read(state_dir.join("todo")).ok();

        let competing_start = competing_app.run_interactive_rebase_plan(plan);
        release_tx.send(()).unwrap();
        writer.join().unwrap();
        let (undo_count, completed_head) = worker.join().unwrap();

        assert!(marker_removed_during_handoff);
        assert_eq!(
            todo_after_startup.as_deref(),
            Some(original_todo.as_slice())
        );
        assert!(
            competing_start
                .unwrap_err()
                .to_string()
                .contains("Another interactive rebase is starting"),
            "the completion owner must retain the state lock through cleanup"
        );
        assert_eq!(undo_count, 1);
        assert_ne!(completed_head, Some(original_head));
        assert!(!state_dir.exists());
    }

    #[test]
    fn interactive_abort_acquires_state_ownership_before_git_and_cleanup() {
        let (tmp, mut app, base, original_head) = app_with_conflicting_range();
        app.start_interactive_rebase(base);
        app.rebase_plan.as_mut().unwrap().move_down(0).unwrap();
        let plan = app.rebase_plan.clone().unwrap();
        app.mode = AppMode::Confirm {
            message: rebase_summary(&plan),
            action: ConfirmAction::RunInteractiveRebase(plan),
        };
        app.handle_action(Action::Confirm).unwrap();
        assert!(app.interactive_rebase_in_progress);
        let state_dir = tmp.path().join(".git/keifu-interactive-rebase");
        let marker = tmp.path().join(".git/rebase-merge/interactive");
        let state_owner = acquire_interactive_rebase_state(app.repo.repo().path()).unwrap();
        app.mode = AppMode::Confirm {
            message: "Abort rebase?".to_string(),
            action: ConfirmAction::AbortOperation(OperationState::Rebase),
        };

        let blocked_abort = app.handle_action(Action::Confirm);

        assert!(blocked_abort
            .unwrap_err()
            .to_string()
            .contains("Another interactive rebase is starting"));
        assert!(marker.exists());
        assert!(state_dir.exists());
        drop(state_owner);

        app.handle_action(Action::Confirm).unwrap();

        assert_eq!(app.repo.head_oid(), Some(original_head));
        assert!(!marker.exists());
        assert!(!state_dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn failed_confirmed_plan_is_a_non_blocking_error_toast() {
        use std::os::unix::fs::PermissionsExt;

        let (tmp, mut app, base, head) = app_with_range();
        let hook = tmp.path().join(".git/hooks/pre-rebase");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook, permissions).unwrap();
        app.start_interactive_rebase(base);
        let plan = app.rebase_plan.clone().unwrap();
        app.mode = AppMode::Confirm {
            message: rebase_summary(&plan),
            action: ConfirmAction::RunInteractiveRebase(plan),
        };

        app.handle_action(Action::Confirm).unwrap();

        assert!(matches!(app.mode, AppMode::Normal));
        assert_eq!(app.repo.head_oid(), Some(head));
        assert!(app.toasts.visible().iter().any(|toast| {
            toast.kind == crate::toast::ToastKind::Error
                && toast.text.contains("Interactive rebase failed")
        }));
        assert!(
            !tmp.path().join(".git/keifu-interactive-rebase").exists(),
            "failed start must not retain todo or commit-message content"
        );
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_startup_does_not_delete_state_while_a_rebase_is_starting() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};

        let (tmp, app, base, _head) = app_with_range();
        drop(app);
        let started = tmp.path().join("hook-started");
        let release = tmp.path().join("hook-release");
        let hook = tmp.path().join(".git/hooks/pre-rebase");
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\ntouch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nexit 1\n",
                started.display(),
                release.display()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook, permissions).unwrap();
        let repo_path = tmp.path().to_path_buf();
        let worker = std::thread::spawn(move || {
            let mut app = App::from_repo(GitRepository::open(&repo_path).unwrap()).unwrap();
            app.start_interactive_rebase(base);
            let plan = app.rebase_plan.clone().unwrap();
            app.run_interactive_rebase_plan(plan)
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !started.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(started.exists(), "pre-rebase hook did not start");
        let state_dir = tmp.path().join(".git/keifu-interactive-rebase");
        assert!(state_dir.join("todo").exists());

        let _second_app = App::from_repo(GitRepository::open(tmp.path()).unwrap()).unwrap();
        let retained_during_start = state_dir.join("todo").exists();
        std::fs::write(&release, "release\n").unwrap();
        let outcome = worker.join().unwrap();

        assert!(
            retained_during_start,
            "another app instance must not delete state owned by an in-flight start"
        );
        assert!(outcome.is_err());
        assert!(!state_dir.exists());
    }
}
