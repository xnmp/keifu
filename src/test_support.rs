//! Shared helpers for src-internal unit tests (the `#[cfg(test)] mod tests`
//! blocks inside `src/app/*.rs` and `src/git/*.rs`), as opposed to the
//! black-box tests under `tests/`.

use std::path::Path;
use std::process::Command;

/// Run a git command in `dir` with a fully isolated identity and config —
/// independent of the invoking user's global/system git config. Without this,
/// a global setting such as `commit.gpgsign = true` would make these tests
/// hang waiting on a GPG prompt instead of failing fast.
///
/// Deliberately does not assert the exit code: some callers intentionally run
/// a command expected to fail (e.g. a merge that leaves conflicts).
pub fn git(dir: &Path, args: &[&str]) {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .expect("run git");
}
