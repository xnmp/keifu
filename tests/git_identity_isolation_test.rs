//! Regression coverage for the friendly missing-Git-identity error.
//!
//! This is deliberately its own integration-test binary: the environment
//! overrides below are process-global, but this binary has no sibling tests.

use std::fs;
use std::path::Path;

use keifu::git::operations::{commit_with_message, stage_file};

mod common;
use common::{commit_file, git_cli, init_repo, repo_path, Seed};

#[test]
fn identity_regression_is_not_in_parallel_git_operations_binary() {
    let git_operations_source = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/git_operations_test.rs"),
    )
    .unwrap();

    assert!(
        !git_operations_source.contains("fn commit_without_configured_user_maps_to_friendly_error"),
        "the identity regression must stay out of the parallel git_operations_test binary"
    );
}

#[test]
fn commit_without_configured_user_maps_to_friendly_error() {
    let (td, git_repo) = init_repo(Seed::Empty);
    let repo = git_repo.repo();
    let path = repo_path(&git_repo);
    commit_file(repo, "a.txt", "a", "initial");

    // Blank the identity so git refuses to author a commit ("Please tell me who
    // you are"). Empty local values override any global identity.
    git_cli(path, &["config", "user.name", ""]);
    git_cli(path, &["config", "user.email", ""]);

    fs::write(repo.workdir().unwrap().join("a.txt"), "changed").unwrap();
    stage_file(path, "a.txt").unwrap();

    // The operation deliberately honors a user's global identity in production.
    // Isolate this dedicated test process from host configuration instead.
    let empty_global = td.path().join("empty-global-gitconfig");
    fs::write(&empty_global, "").unwrap();
    let inherited_identity = [
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
    ]
    .into_iter()
    .map(|key| (key, std::env::var_os(key)))
    .collect::<Vec<_>>();
    std::env::set_var("GIT_CONFIG_GLOBAL", &empty_global);
    std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    for key in [
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
    ] {
        std::env::remove_var(key);
    }
    let result = commit_with_message(path, "a message");
    for (key, value) in inherited_identity {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("Git user not configured"),
        "unconfigured identity should map to a friendly error, got: {err}"
    );
}
