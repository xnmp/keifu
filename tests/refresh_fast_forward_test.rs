//! Fast-forward-on-refresh (`refresh.fast_forward_on_refresh`, issue #84).
//!
//! Two layers of coverage:
//!   * the pure git operation `fast_forward_behind_branches`, against real temp
//!     repos backed by a bare "remote": behind-only branches move, diverged /
//!     ahead / up-to-date branches don't, and the checked-out branch moves only
//!     when the working tree is clean;
//!   * the App gate — a real F5 (`FullUpdate`) fetch-and-refresh cycle
//!     fast-forwards only when the setting is on.

use std::path::Path;
use std::time::{Duration, Instant};

use git2::Oid;

use keifu::action::Action;
use keifu::app::App;
use keifu::git::operations::fast_forward_behind_branches;
use keifu::git::GitRepository;

mod common;
use common::{add_bare_origin, commit_file, current_branch, git_cli, head_oid, init_repo, Seed};

fn open(path: &Path) -> git2::Repository {
    git2::Repository::open(path).unwrap()
}

fn branch_tip(repo: &git2::Repository, name: &str) -> Oid {
    repo.find_branch(name, git2::BranchType::Local)
        .unwrap()
        .get()
        .peel_to_commit()
        .unwrap()
        .id()
}

/// Advance `branch` on the bare repo at `origin_path` by one commit, made in a
/// throwaway clone — simulating "someone else pushed". Returns the new upstream
/// commit OID (now present in the bare origin).
fn advance_origin(origin_path: &Path, branch: &str, file: &str, contents: &str) -> Oid {
    let clone = common::clone_from(origin_path);
    let clone_path = clone.path().to_str().unwrap().to_string();
    // Land on `branch` (clone HEAD may be origin's default branch).
    git_cli(&clone_path, &["checkout", branch]);
    let repo = open(clone.path());
    let oid = commit_file(&repo, file, contents, "remote work");
    git_cli(&clone_path, &["push", "origin", branch]);
    oid
}

// ── Operations layer: fast_forward_behind_branches ──────────────────

#[test]
fn non_checked_out_branch_behind_upstream_is_fast_forwarded() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let default = current_branch(git_repo.repo());
    let origin = add_bare_origin(&path);

    // A `feature` branch, published with an upstream, while HEAD stays on the
    // default branch (feature is NOT checked out).
    let base = head_oid(git_repo.repo());
    git_cli(&path, &["branch", "feature", &default]);
    git_cli(&path, &["push", "-u", "origin", "feature"]);

    // origin/feature advances; the local `feature` stays put and, after a fetch,
    // is strictly behind its upstream.
    let remote_tip = advance_origin(origin.path(), "feature", "f.txt", "f");
    git_cli(&path, &["fetch", "origin"]);

    let repo = open(Path::new(&path));
    assert_eq!(branch_tip(&repo, "feature"), base, "precondition: feature is behind");

    let summary = fast_forward_behind_branches(&repo);

    assert_eq!(summary.moved, vec!["feature".to_string()]);
    assert!(summary.failed.is_empty());
    // Pure ref update moved the local branch to the upstream tip…
    assert_eq!(branch_tip(&repo, "feature"), remote_tip);
    // …and HEAD (the default branch) was untouched.
    assert_eq!(head_oid(&repo), base);
    assert_eq!(repo.head().unwrap().shorthand().unwrap(), default);
}

#[test]
fn checked_out_branch_behind_is_fast_forwarded_when_clean() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let default = current_branch(git_repo.repo());
    let origin = add_bare_origin(&path);
    let base = head_oid(git_repo.repo());

    git_cli(&path, &["push", "-u", "origin", &default]);
    let remote_tip = advance_origin(origin.path(), &default, "r.txt", "r");
    git_cli(&path, &["fetch", "origin"]);

    let repo = open(Path::new(&path));
    assert_eq!(head_oid(&repo), base, "precondition: checked-out branch is behind");

    let summary = fast_forward_behind_branches(&repo);

    assert_eq!(summary.moved, vec![default.clone()]);
    assert!(summary.failed.is_empty());
    // Ref advanced AND the working tree was checked out to the new tip.
    assert_eq!(head_oid(&repo), remote_tip);
    assert!(
        Path::new(&path).join("r.txt").exists(),
        "working tree must be updated to the fast-forwarded tree"
    );
}

#[test]
fn checked_out_branch_behind_is_skipped_when_working_tree_dirty() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let default = current_branch(git_repo.repo());
    let origin = add_bare_origin(&path);
    let base = head_oid(git_repo.repo());

    git_cli(&path, &["push", "-u", "origin", &default]);
    advance_origin(origin.path(), &default, "r.txt", "r");
    git_cli(&path, &["fetch", "origin"]);

    // Dirty a tracked file: the checked-out branch must be skipped silently.
    std::fs::write(Path::new(&path).join("tracked.txt"), "dirty edit\n").unwrap();

    let repo = open(Path::new(&path));
    let summary = fast_forward_behind_branches(&repo);

    assert!(summary.moved.is_empty(), "dirty checked-out branch must not move");
    assert!(summary.failed.is_empty(), "a dirty skip is not a failure");
    assert_eq!(head_oid(&repo), base, "HEAD must stay put on a dirty tree");
}

#[test]
fn diverged_branch_is_left_untouched() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let default = current_branch(git_repo.repo());
    let origin = add_bare_origin(&path);

    git_cli(&path, &["branch", "feature", &default]);
    git_cli(&path, &["push", "-u", "origin", "feature"]);

    // Remote advances feature (behind by 1)…
    advance_origin(origin.path(), "feature", "remote.txt", "remote");
    git_cli(&path, &["fetch", "origin"]);
    // …and the local feature also gains its own commit (ahead by 1) → diverged.
    let repo = open(Path::new(&path));
    git_cli(&path, &["checkout", "feature"]);
    let local_tip = commit_file(&repo, "local.txt", "local", "local work");
    git_cli(&path, &["checkout", &default]);

    let repo = open(Path::new(&path));
    let summary = fast_forward_behind_branches(&repo);

    assert!(summary.moved.is_empty(), "a diverged branch must not fast-forward");
    assert!(summary.failed.is_empty());
    assert_eq!(branch_tip(&repo, "feature"), local_tip, "feature tip unchanged");
}

#[test]
fn up_to_date_and_ahead_branches_are_untouched() {
    let (_td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let default = current_branch(git_repo.repo());
    let _origin = add_bare_origin(&path);

    // `even` is exactly at its upstream; `ahead` is one commit past it.
    git_cli(&path, &["branch", "even", &default]);
    git_cli(&path, &["push", "-u", "origin", "even"]);
    git_cli(&path, &["branch", "ahead", &default]);
    git_cli(&path, &["push", "-u", "origin", "ahead"]);

    let repo = open(Path::new(&path));
    git_cli(&path, &["checkout", "ahead"]);
    let ahead_tip = commit_file(&repo, "ahead.txt", "ahead", "ahead work");
    git_cli(&path, &["checkout", &default]);

    let even_tip = branch_tip(&open(Path::new(&path)), "even");

    let repo = open(Path::new(&path));
    let summary = fast_forward_behind_branches(&repo);

    assert!(summary.moved.is_empty(), "neither up-to-date nor ahead branches move");
    assert!(summary.failed.is_empty());
    assert_eq!(branch_tip(&repo, "even"), even_tip);
    assert_eq!(branch_tip(&repo, "ahead"), ahead_tip);
}

// ── App gate: F5 (FullUpdate) honours the setting ───────────────────

/// Drive the background fetch kicked off by `FullUpdate` to completion, so the
/// fetch-completion handler (and its fast-forward gate) runs.
fn pump_fetch_to_completion(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !app.update_fetch_status() {
        assert!(Instant::now() < deadline, "background fetch never completed");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Repo whose checked-out default branch tracks origin and will be strictly
/// behind once the app fetches. Returns (tempdir, origin tempdir, path, branch,
/// base OID, upstream OID that a fetch will bring in).
fn behind_after_fetch_fixture() -> (tempfile::TempDir, tempfile::TempDir, String, String, Oid, Oid) {
    let (td, git_repo) = init_repo(Seed::TrackedFile);
    let path = git_repo.path.clone();
    let default = current_branch(git_repo.repo());
    let base = head_oid(git_repo.repo());
    let origin = add_bare_origin(&path);
    git_cli(&path, &["push", "-u", "origin", &default]);
    // origin advances; the primary has NOT fetched yet, so its remote-tracking
    // ref still points at `base`. The app's own fetch will surface the update.
    let upstream = advance_origin(origin.path(), &default, "r.txt", "r");
    (td, origin, path, default, base, upstream)
}

#[test]
fn full_update_fast_forwards_when_setting_enabled() {
    let (_td, _origin, path, _branch, base, upstream) = behind_after_fetch_fixture();

    let mut app = App::from_repo(GitRepository::open(&path).unwrap()).unwrap();
    app.config.refresh.fast_forward_on_refresh = true;

    app.handle_action(Action::FullUpdate).unwrap();
    pump_fetch_to_completion(&mut app);

    let repo = open(Path::new(&path));
    assert_eq!(head_oid(&repo), upstream, "F5 must fast-forward the behind branch");
    assert_ne!(head_oid(&repo), base);
}

#[test]
fn full_update_does_not_fast_forward_when_setting_disabled() {
    let (_td, _origin, path, _branch, base, upstream) = behind_after_fetch_fixture();

    let mut app = App::from_repo(GitRepository::open(&path).unwrap()).unwrap();
    app.config.refresh.fast_forward_on_refresh = false;

    app.handle_action(Action::FullUpdate).unwrap();
    pump_fetch_to_completion(&mut app);

    let repo = open(Path::new(&path));
    // The fetch still ran (remote-tracking ref advanced), but with the setting
    // off the local branch stays behind — nothing moved.
    assert_eq!(head_oid(&repo), base, "setting off must leave the branch behind");
    assert_ne!(head_oid(&repo), upstream);
}
