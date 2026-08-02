//! Observable Git-history coverage for interactive rebase execution.

use std::fs;

use keifu::git::operations::{abort_interactive_rebase, rebase_interactive, OpOutcome};

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

#[test]
fn interactive_rebase_squash_combines_the_commits_and_messages() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let repo = git_repo.repo();
    let base = repo.head().unwrap().peel_to_commit().unwrap().id();
    let first = commit_file(repo, "first.txt", "first\n", "first subject");
    let second = commit_file(repo, "second.txt", "second\n", "second subject");
    let todo_path = repo.path().join("squash-todo");
    fs::write(
        &todo_path,
        format!("pick {first} first subject\nsquash {second} second subject\n"),
    )
    .unwrap();

    let outcome = rebase_interactive(repo_path(&git_repo), base, &todo_path).unwrap();

    assert_eq!(outcome, OpOutcome::Completed);
    let messages = git_cli(
        repo_path(&git_repo),
        &["log", "--format=%B%x00", &format!("{base}..HEAD")],
    );
    assert_eq!(messages.matches('\0').count(), 1);
    assert!(messages.contains("first subject"));
    assert!(messages.contains("second subject"));
}

#[test]
fn conflicted_interactive_rebase_can_be_aborted_back_to_the_original_head() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let repo = git_repo.repo();
    let base = repo.head().unwrap().peel_to_commit().unwrap().id();
    fs::write(repo.workdir().unwrap().join("tracked.txt"), "one\n").unwrap();
    git_cli(repo_path(&git_repo), &["add", "tracked.txt"]);
    git_cli(repo_path(&git_repo), &["commit", "-m", "one"]);
    let first = repo.head().unwrap().target().unwrap();
    fs::write(repo.workdir().unwrap().join("tracked.txt"), "two\n").unwrap();
    git_cli(repo_path(&git_repo), &["add", "tracked.txt"]);
    git_cli(repo_path(&git_repo), &["commit", "-m", "two"]);
    let second = repo.head().unwrap().target().unwrap();
    let original_head = second;
    let todo_path = repo.path().join("conflict-todo");
    fs::write(&todo_path, format!("pick {second} two\npick {first} one\n")).unwrap();

    let outcome = rebase_interactive(repo_path(&git_repo), base, &todo_path).unwrap();

    assert!(matches!(outcome, OpOutcome::Conflicts { count: 1 }));
    assert!(repo.path().join("rebase-merge/interactive").exists());
    abort_interactive_rebase(repo_path(&git_repo)).unwrap();
    assert_eq!(repo.head().unwrap().target(), Some(original_head));
    assert_eq!(
        fs::read_to_string(repo.workdir().unwrap().join("tracked.txt")).unwrap(),
        "two\n"
    );
}

#[test]
fn non_conflict_interactive_stop_remains_recoverable() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let repo = git_repo.repo();
    let base = repo.head().unwrap().target().unwrap();
    let first = commit_file(repo, "first.txt", "first\n", "first");
    let todo_path = repo.path().join("paused-todo");
    fs::write(
        &todo_path,
        format!("pick {first} first\nexec git rev-parse --verify refs/heads/does-not-exist\n"),
    )
    .unwrap();

    let outcome = rebase_interactive(repo_path(&git_repo), base, &todo_path).unwrap();

    assert_eq!(outcome, OpOutcome::Paused);
    assert!(repo.path().join("rebase-merge/interactive").exists());
    abort_interactive_rebase(repo_path(&git_repo)).unwrap();
    assert_eq!(repo.head().unwrap().target(), Some(first));
}

#[test]
fn reword_message_file_survives_a_conflict_until_continue_reaches_it() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let repo = git_repo.repo();
    let base = repo.head().unwrap().target().unwrap();
    fs::write(repo.workdir().unwrap().join("tracked.txt"), "one\n").unwrap();
    git_cli(repo_path(&git_repo), &["add", "tracked.txt"]);
    git_cli(repo_path(&git_repo), &["commit", "-m", "one"]);
    fs::write(repo.workdir().unwrap().join("tracked.txt"), "two\n").unwrap();
    git_cli(repo_path(&git_repo), &["add", "tracked.txt"]);
    git_cli(repo_path(&git_repo), &["commit", "-m", "two"]);
    let second = repo.head().unwrap().target().unwrap();
    let third = commit_file(repo, "third.txt", "third\n", "third");
    let state_dir = repo.path().join("keifu-interactive-rebase");
    fs::create_dir_all(&state_dir).unwrap();
    let message_path = state_dir.join("message-third");
    fs::write(&message_path, "renamed after conflict\n").unwrap();
    let todo_path = state_dir.join("todo");
    fs::write(
        &todo_path,
        format!(
            "pick {second} two\npick {third} third\nexec git commit --amend -F '{}'\n",
            message_path.display()
        ),
    )
    .unwrap();

    assert!(matches!(
        rebase_interactive(repo_path(&git_repo), base, &todo_path).unwrap(),
        OpOutcome::Conflicts { count: 1 }
    ));
    assert_eq!(
        fs::read_to_string(&message_path).unwrap(),
        "renamed after conflict\n"
    );
    // Simulate a later session resolving the conflict and reaching the pending
    // reword step; the operations layer carries no in-memory todo state.
    fs::write(repo.workdir().unwrap().join("tracked.txt"), "two\n").unwrap();
    git_cli(repo_path(&git_repo), &["add", "tracked.txt"]);

    let outcome =
        keifu::git::operations::continue_interactive_rebase(repo_path(&git_repo)).unwrap();

    assert_eq!(outcome, OpOutcome::Completed);
    let reopened = git2::Repository::open(repo.workdir().unwrap()).unwrap();
    assert_eq!(
        reopened.head().unwrap().peel_to_commit().unwrap().summary(),
        Some("renamed after conflict")
    );
}
