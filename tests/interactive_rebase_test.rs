//! Observable Git-history coverage for interactive rebase execution.

use std::fs;

use keifu::git::operations::{rebase_interactive, OpOutcome};

mod common;
use common::{commit_file, git_cli, init_repo, repo_path, Seed};

#[test]
fn interactive_rebase_executes_the_authored_order_and_actions() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let repo = git_repo.repo();
    let base = repo.head().unwrap().peel_to_commit().unwrap().id();
    let first = commit_file(repo, "first.txt", "first\n", "first");
    let second = commit_file(repo, "second.txt", "second\n", "second");
    let third = commit_file(repo, "third.txt", "third\n", "third");
    let dropped = commit_file(repo, "dropped.txt", "dropped\n", "dropped");

    let state_dir = repo.path().join("keifu-interactive-rebase-test");
    fs::create_dir_all(&state_dir).unwrap();
    let message_path = state_dir.join("message.txt");
    fs::write(&message_path, "renamed second\n").unwrap();
    let todo_path = state_dir.join("todo");
    fs::write(
        &todo_path,
        format!(
            "pick {second} second\nexec git commit --amend -F '{}'\nfixup {third} third\npick {first} first\n",
            message_path.display()
        ),
    )
    .unwrap();

    let outcome = rebase_interactive(repo_path(&git_repo), base, &todo_path).unwrap();

    assert_eq!(outcome, OpOutcome::Completed);
    let subjects = git_cli(
        repo_path(&git_repo),
        &["log", "--format=%s", &format!("{base}..HEAD")],
    );
    assert_eq!(
        subjects.lines().collect::<Vec<_>>(),
        ["first", "renamed second"]
    );
    assert!(!repo.workdir().unwrap().join("dropped.txt").exists());
    assert_ne!(repo.head().unwrap().target(), Some(dropped));
}
