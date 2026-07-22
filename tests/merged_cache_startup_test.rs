//! Startup merged-branch classification cache (#104).
//!
//! With `hide_merged_branches` on, keifu used to pay the full merged-branch
//! classification — O(branches × ancestry + bounded patch-id scans + three-way
//! merges) — *synchronously* before the first frame, so a branchy repo froze at
//! startup. The fix persists the last classification result under a signature of
//! its inputs and serves it at startup, revalidating in the background.
//!
//! These gates pin the behavior deterministically (immune to machine speed):
//!  - a cold/absent cache must NOT classify synchronously (the merged set is
//!    empty right after init, then fills in from the background poll);
//!  - a matching cache IS applied synchronously (the merged set is populated at
//!    init, no empty first frame / flash).
//!
//! Plus a generous wall-clock budget on a branchy fixture: cold-cache hide-mode
//! startup must stay far under the per-branch-git-scan regime, catching a
//! reintroduction of synchronous classification.
//!
//! All tests share an isolated config dir (via `XDG_CONFIG_HOME`) so they read
//! and write a throwaway cache, never the developer's real one. This is the only
//! test binary that overrides that env var, so there is no cross-test race.

use std::sync::Once;
use std::time::{Duration, Instant};

use git2::{Oid, Repository, Signature};
use tempfile::TempDir;

use keifu::app::App;
use keifu::config::UiState;
use keifu::git::GitRepository;

// ── Isolated cache directory ─────────────────────────────────────────

static CACHE_ENV: Once = Once::new();

/// Point `dirs::config_dir()` (which reads `$XDG_CONFIG_HOME` on Linux) at a
/// process-unique temp dir, once for the whole binary. Set to a fixed value and
/// never changed, so concurrent tests share one throwaway cache root without
/// racing on the variable. Each test uses a unique repo path, so their per-repo
/// cache files never collide.
fn isolate_cache_dir() {
    CACHE_ENV.call_once(|| {
        let dir = std::env::temp_dir().join(format!("keifu-merged-cache-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("XDG_CONFIG_HOME", &dir);
    });
}

// ── Fixtures ─────────────────────────────────────────────────────────

fn commit_on(
    repo: &Repository,
    refname: &str,
    parent: Option<Oid>,
    path: &str,
    contents: &str,
) -> Oid {
    let sig = Signature::now("Test User", "test@example.com").unwrap();
    let mut builder = match parent {
        Some(p) => {
            let tree = repo.find_commit(p).unwrap().tree().unwrap();
            repo.treebuilder(Some(&tree)).unwrap()
        }
        None => repo.treebuilder(None).unwrap(),
    };
    let blob = repo.blob(contents.as_bytes()).unwrap();
    builder.insert(path, blob, 0o100644).unwrap();
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let parents: Vec<git2::Commit> = parent.map(|p| repo.find_commit(p).unwrap()).into_iter().collect();
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
    repo.commit(Some(refname), &sig, &sig, path, &tree, &parent_refs)
        .unwrap()
}

/// A merge commit on `refname`: parents `[first, second]`, tree = first's tree
/// overlaid with second's entries — the shape of a branch landing on the trunk.
/// A landed branch's tip must hang OFF the trunk's first-parent line to count
/// as merged (a tip ON the line is merely behind since #112), so fixtures land
/// their branches through this instead of pointing them at trunk ancestors.
fn merge_on(repo: &Repository, refname: &str, first: Oid, second: Oid) -> Oid {
    let fc = repo.find_commit(first).unwrap();
    let sc = repo.find_commit(second).unwrap();
    let mut builder = repo.treebuilder(Some(&fc.tree().unwrap())).unwrap();
    for entry in sc.tree().unwrap().iter() {
        builder.insert(entry.name().unwrap(), entry.id(), entry.filemode()).unwrap();
    }
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let sig = Signature::now("Test User", "test@example.com").unwrap();
    repo.commit(Some(refname), &sig, &sig, "merge", &tree, &[&fc, &sc])
        .unwrap()
}

/// A repo whose `topic` branch landed on `main` via a merge commit
/// (unambiguously merged), plus an unmerged `gone` branch. `main` is HEAD.
fn repo_with_merged_topic() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    {
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test User").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
    }
    let a = commit_on(&repo, "refs/heads/main", None, "base.txt", "a");
    // topic: one own commit, landed on main by a merge commit → merged.
    repo.reference("refs/heads/topic", a, true, "topic").unwrap();
    let t = commit_on(&repo, "refs/heads/topic", Some(a), "t.txt", "t");
    let b = commit_on(&repo, "refs/heads/main", Some(a), "base.txt", "b");
    merge_on(&repo, "refs/heads/main", b, t);
    // gone: novel unlanded work → never merged.
    commit_on(&repo, "refs/heads/gone", Some(a), "gone.txt", "x");
    repo.set_head("refs/heads/main").unwrap();
    dir
}

/// A branchy repo: 30-commit `main`, `n` feature branches, half landed on main
/// via merge commits (merged) and half diverging with real unlanded diffs — the
/// shape whose synchronous classification is expensive. `main` is HEAD.
fn branchy_repo(n: usize) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    {
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test User").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
    }
    let mut tip = commit_on(&repo, "refs/heads/main", None, "base.txt", "0");
    let mut line = vec![tip];
    for i in 1..30 {
        tip = commit_on(&repo, "refs/heads/main", Some(tip), "base.txt", &format!("{i}"));
        line.push(tip);
    }
    repo.set_head("refs/heads/main").unwrap();
    for b in 0..n {
        let name = format!("refs/heads/feature-{b}");
        let base = line[b % line.len()];
        if b % 2 == 0 {
            let c = commit_on(&repo, &name, Some(base), &format!("m{b}.txt"), "landed");
            tip = merge_on(&repo, "refs/heads/main", tip, c);
        } else {
            let c1 = commit_on(&repo, &name, Some(base), &format!("f{b}.txt"), "one");
            commit_on(&repo, &name, Some(c1), &format!("f{b}.txt"), "two");
        }
    }
    dir
}

fn hide_state() -> UiState {
    UiState {
        hide_merged_branches: true,
        ..UiState::default()
    }
}

fn build_hide_app(dir: &TempDir) -> App {
    App::from_repo_with_ui_state(GitRepository::open(dir.path()).unwrap(), hide_state()).unwrap()
}

/// Poll the background classifier until it delivers (bounded), so the test never
/// hangs if something regresses.
fn poll_until_classified(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if app.update_merged_classification() {
            return;
        }
        assert!(Instant::now() < deadline, "background classification never delivered");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ── Deterministic gates ──────────────────────────────────────────────

/// Cold cache + hide mode: init must NOT classify synchronously. `topic` is
/// unambiguously merged, so a synchronous classification would populate the
/// merged set at init — asserting it is empty right after init proves the work
/// was deferred. The background poll then delivers the real set.
#[test]
fn hide_mode_cold_cache_defers_classification() {
    isolate_cache_dir();
    let dir = branchy_repo(20); // a repo whose sync classification would be costly
    // Ensure no warm cache from a prior run of this exact temp path.
    let mut app = build_hide_app(&dir);
    assert!(
        app.merged.branches.is_empty(),
        "hide-mode init on a cold cache must not classify synchronously: {:?}",
        app.merged.branches
    );
    // The classifier was kicked at init; it fills in shortly after.
    poll_until_classified(&mut app);
    assert!(
        !app.merged.branches.is_empty(),
        "background classification should find the merged feature branches"
    );
}

/// A matching cache is applied synchronously at init — the merged set is
/// populated immediately (no empty first frame), and no background reconcile is
/// needed. First run warms the cache; the second run on the unchanged repo hits
/// it.
#[test]
fn hide_mode_matching_cache_applied_synchronously() {
    isolate_cache_dir();
    let dir = repo_with_merged_topic();

    // Run 1: cold cache → deferred; the delivered result is persisted.
    {
        let mut app = build_hide_app(&dir);
        assert!(app.merged.branches.is_empty(), "run 1 starts unclassified");
        poll_until_classified(&mut app);
        assert!(app.merged.branches.contains("topic"), "run 1 classifies topic as merged");
    }

    // Run 2: unchanged repo → the cache signature matches → applied at init.
    let app2 = build_hide_app(&dir);
    assert!(
        app2.merged.branches.contains("topic"),
        "matching cache must be applied synchronously (no flash): {:?}",
        app2.merged.branches
    );
    assert!(
        !app2.merged.branches.contains("gone"),
        "the cached result is the real classification, not a blanket hide"
    );
}

/// A stale cache (repo changed since it was written) must not be trusted
/// wholesale nor block: init serves the stale result for the first frame, then
/// the background reconcile corrects it. Here a branch that was merged when the
/// cache was written becomes unmerged (new novel commit), and the reconcile
/// drops it from the merged set.
#[test]
fn hide_mode_stale_cache_reconciles_in_background() {
    isolate_cache_dir();
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    {
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test User").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
    }
    let a = commit_on(&repo, "refs/heads/main", None, "base.txt", "a");
    repo.reference("refs/heads/topic", a, true, "topic").unwrap();
    let t = commit_on(&repo, "refs/heads/topic", Some(a), "t.txt", "t");
    let b = commit_on(&repo, "refs/heads/main", Some(a), "base.txt", "b");
    merge_on(&repo, "refs/heads/main", b, t);
    repo.set_head("refs/heads/main").unwrap();

    // Warm the cache with `topic` classified merged.
    {
        let mut app = build_hide_app(&dir);
        poll_until_classified(&mut app);
        assert!(app.merged.branches.contains("topic"));
    }

    // Now move `topic` forward with novel, unlanded work → no longer merged.
    let topic_tip = repo.find_reference("refs/heads/topic").unwrap().target().unwrap();
    commit_on(&repo, "refs/heads/topic", Some(topic_tip), "novel.txt", "z");

    // Init on the changed repo: the signature no longer matches, so the seed is
    // stale. It must not block, and the background reconcile drops `topic`.
    let mut app2 = build_hide_app(&dir);
    poll_until_classified(&mut app2);
    assert!(
        !app2.merged.branches.contains("topic"),
        "stale cache reconciled: topic with new unlanded work is no longer merged: {:?}",
        app2.merged.branches
    );
}

// ── Wall-clock budget (algorithmic-blowup guard) ─────────────────────

/// Cold-cache hide-mode startup on a branchy fixture must stay far under the
/// per-branch-git-scan regime, because classification is deferred to the
/// background rather than paid inline. If synchronous classification is
/// reintroduced on the hide path, this trips.
///
/// Budget is deliberately generous (~10-30× a debug-build measurement; git
/// tempdir I/O is the flakiest thing here) — it exists to catch an algorithmic
/// regression, not a few-percent drift.
#[test]
fn hide_mode_cold_startup_budget_on_a_branchy_repo() {
    isolate_cache_dir();
    let dir = branchy_repo(40);
    let started = Instant::now();
    let app = build_hide_app(&dir);
    let elapsed = started.elapsed();
    println!("hide-mode cold startup on 40-branch repo: {elapsed:?}");
    assert!(!app.graph_layout.nodes.is_empty());
    assert!(
        elapsed < Duration::from_millis(300),
        "hide-mode App init took {elapsed:?} on a 40-branch fixture — startup regained \
         synchronous per-branch merged classification (budget 300ms; classification \
         must stay on the background thread, seeded from the #104 cache)"
    );
}
