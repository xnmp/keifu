//! Integration tests for hunk-level staging: patch synthesis
//! (`extract_hunk_from_working_tree` + `render_hunk_patch`) round-tripped
//! through the real `git apply` invocations in `git::operations`.
//!
//! These assert the git-observable outcome (`git diff` / `git diff --cached`),
//! not intermediate state — the whole point is that our synthesised patches are
//! accepted by git and move exactly one hunk.

mod common;

use std::fs;
use std::path::Path;

use common::{git_cli, init_repo, repo_path, Seed};
use keifu::git::operations::{
    apply_patch_cached, apply_patch_cached_reverse, apply_patch_worktree_reverse, stage_all,
    unstage_all,
};
use keifu::git::{extract_hunk_from_working_tree, render_hunk_patch};

/// Synthesise the patch for the `hunk_index`-th hunk of `rel_path` from the
/// current working tree and return its text.
fn hunk_patch(git_repo: &keifu::git::GitRepository, rel_path: &str, hunk_index: usize) -> String {
    let hunk = extract_hunk_from_working_tree(git_repo.repo(), Path::new(rel_path), hunk_index)
        .expect("extract must succeed")
        .expect("hunk must exist");
    render_hunk_patch(rel_path, &hunk)
}

/// A 20-line file. Editing line 2 (TOP) and line 19 (BOTTOM) leaves 16
/// unchanged lines between them — far more than 2×context(3), so git produces
/// two distinct hunks rather than merging them.
fn base() -> String {
    (1..=20).map(|i| format!("l{i}\n")).collect()
}
fn two_edits() -> String {
    (1..=20)
        .map(|i| match i {
            2 => "TOP\n".to_string(),
            19 => "BOTTOM\n".to_string(),
            _ => format!("l{i}\n"),
        })
        .collect()
}

fn write(repo: &Path, rel: &str, contents: &str) {
    fs::write(repo.join(rel), contents).unwrap();
}

#[test]
fn staging_one_of_two_hunks_stages_only_that_hunk() {
    let (tempdir, git_repo) = init_repo(Seed::Empty);
    let rp = repo_path(&git_repo).to_string();
    write(tempdir.path(), "f.txt", &base());
    git_cli(&rp, &["add", "f.txt"]);
    git_cli(&rp, &["commit", "-m", "base"]);
    write(tempdir.path(), "f.txt", &two_edits());

    // Stage only the first hunk (the TOP edit).
    let patch = hunk_patch(&git_repo, "f.txt", 0);
    apply_patch_cached(&rp, &patch).unwrap();

    let staged = git_cli(&rp, &["diff", "--cached"]);
    assert!(staged.contains("+TOP"), "index should contain TOP:\n{staged}");
    assert!(
        !staged.contains("+BOTTOM"),
        "index must NOT contain BOTTOM:\n{staged}"
    );

    // The worktree still carries the second, unstaged hunk.
    let unstaged = git_cli(&rp, &["diff"]);
    assert!(
        unstaged.contains("+BOTTOM"),
        "worktree should retain BOTTOM:\n{unstaged}"
    );
    assert!(
        !unstaged.contains("+TOP"),
        "TOP is fully staged, so `git diff` must not show it:\n{unstaged}"
    );
}

#[test]
fn unstaging_a_staged_hunk_clears_the_index() {
    let (tempdir, git_repo) = init_repo(Seed::Empty);
    let rp = repo_path(&git_repo).to_string();
    write(tempdir.path(), "f.txt", &base());
    git_cli(&rp, &["add", "f.txt"]);
    git_cli(&rp, &["commit", "-m", "base"]);
    write(tempdir.path(), "f.txt", &two_edits());

    apply_patch_cached(&rp, &hunk_patch(&git_repo, "f.txt", 0)).unwrap();
    assert!(git_cli(&rp, &["diff", "--cached"]).contains("+TOP"));

    // Re-derive the hunk from the (unchanged) worktree and reverse-apply it.
    let patch = hunk_patch(&git_repo, "f.txt", 0);
    apply_patch_cached_reverse(&rp, &patch).unwrap();

    let staged = git_cli(&rp, &["diff", "--cached"]);
    assert!(staged.trim().is_empty(), "index should be clean:\n{staged}");
    // Both edits remain in the worktree.
    let unstaged = git_cli(&rp, &["diff"]);
    assert!(unstaged.contains("+TOP") && unstaged.contains("+BOTTOM"));
}

#[test]
fn discarding_a_hunk_reverts_only_that_hunk_in_the_worktree() {
    let (tempdir, git_repo) = init_repo(Seed::Empty);
    let rp = repo_path(&git_repo).to_string();
    write(tempdir.path(), "f.txt", &base());
    git_cli(&rp, &["add", "f.txt"]);
    git_cli(&rp, &["commit", "-m", "base"]);
    write(tempdir.path(), "f.txt", &two_edits());

    // Discard the first hunk (TOP) from the working tree.
    let patch = hunk_patch(&git_repo, "f.txt", 0);
    apply_patch_worktree_reverse(&rp, &patch).unwrap();

    let contents = fs::read_to_string(tempdir.path().join("f.txt")).unwrap();
    assert!(
        contents.contains("l2\n") && !contents.contains("TOP"),
        "TOP must be reverted:\n{contents}"
    );
    assert!(
        contents.contains("BOTTOM"),
        "BOTTOM must survive:\n{contents}"
    );
}

#[test]
fn stage_all_then_unstage_all_round_trips_including_untracked() {
    let (tempdir, git_repo) = init_repo(Seed::TrackedFile);
    let rp = repo_path(&git_repo).to_string();
    // Modify the tracked file and add an untracked one.
    write(tempdir.path(), "tracked.txt", "tracked-modified\n");
    write(tempdir.path(), "brand_new.txt", "hello\n");

    stage_all(&rp).unwrap();
    let staged = git_cli(&rp, &["diff", "--cached", "--name-only"]);
    assert!(
        staged.contains("tracked.txt") && staged.contains("brand_new.txt"),
        "stage_all must stage tracked + untracked:\n{staged}"
    );
    // Nothing left unstaged in the tracked file, and no untracked remain.
    assert!(git_cli(&rp, &["diff", "--name-only"]).trim().is_empty());
    assert!(git_cli(&rp, &["ls-files", "--others", "--exclude-standard"])
        .trim()
        .is_empty());

    unstage_all(&rp).unwrap();
    assert!(
        git_cli(&rp, &["diff", "--cached", "--name-only"]).trim().is_empty(),
        "unstage_all must clear the index"
    );
    // Files themselves are untouched: modification + untracked file still there.
    assert!(git_cli(&rp, &["diff", "--name-only"]).contains("tracked.txt"));
    assert!(git_cli(&rp, &["ls-files", "--others", "--exclude-standard"]).contains("brand_new.txt"));
}

#[test]
fn synthesized_patch_applies_for_a_file_without_trailing_newline() {
    let (tempdir, git_repo) = init_repo(Seed::Empty);
    let rp = repo_path(&git_repo).to_string();
    // Commit a file that ends without a newline, then change its last line
    // (still no trailing newline) — exercises the `\ No newline` marker.
    write(tempdir.path(), "n.txt", "a\nb\nc");
    git_cli(&rp, &["add", "n.txt"]);
    git_cli(&rp, &["commit", "-m", "base"]);
    write(tempdir.path(), "n.txt", "a\nb\nCHANGED");

    let patch = hunk_patch(&git_repo, "n.txt", 0);
    assert!(patch.contains("\\ No newline at end of file"));
    apply_patch_cached(&rp, &patch).unwrap();

    let staged = git_cli(&rp, &["diff", "--cached"]);
    assert!(staged.contains("+CHANGED"), "staged:\n{staged}");
    assert!(git_cli(&rp, &["diff"]).trim().is_empty(), "fully staged");
}

#[test]
fn synthesized_patch_applies_for_a_crlf_file() {
    let (tempdir, git_repo) = init_repo(Seed::Empty);
    let rp = repo_path(&git_repo).to_string();
    // Keep CRLF bytes verbatim in the repo.
    git_cli(&rp, &["config", "core.autocrlf", "false"]);
    write(tempdir.path(), "w.txt", "p\r\nq\r\nr\r\n");
    git_cli(&rp, &["add", "w.txt"]);
    git_cli(&rp, &["commit", "-m", "base"]);
    write(tempdir.path(), "w.txt", "p\r\nQ\r\nr\r\n");

    let patch = hunk_patch(&git_repo, "w.txt", 0);
    assert!(patch.contains("-q\r\n") && patch.contains("+Q\r\n"));
    apply_patch_cached(&rp, &patch).unwrap();

    assert!(git_cli(&rp, &["diff", "--cached"]).contains("+Q"));
    // Blob in the index preserved CRLF.
    let staged_blob = git_cli(&rp, &["show", ":w.txt"]);
    assert!(staged_blob.contains("Q\r"), "index blob keeps CRLF");
}
