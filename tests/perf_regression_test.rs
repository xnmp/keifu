//! Perf regression gates.
//!
//! Philosophy: wall-clock budgets here are deliberately ~10× the measured
//! debug-build cost — they exist to catch *algorithmic* blowups (an O(n²)
//! sneaking into the raster pipeline, a per-branch git scan landing on the
//! startup path), not few-percent drift. CI noise of 2-3× must never trip
//! them. Absolute regressions with a deterministic signature are gated by
//! counter-based tests instead (e.g. `app_behavior_test::
//! startup_defers_merged_classification_to_the_background` pins that startup
//! does zero synchronous merged-branch classification in dim-only mode —
//! that's the #78 gate, immune to machine speed).
//!
//! When a budget trips: first re-measure locally (`cargo test --test
//! perf_regression_test -- --nocapture` prints the measured times), then
//! either fix the regression or, if the cost is a justified feature,
//! re-calibrate the budget in the same commit that justifies it.

use std::time::{Duration, Instant};

use git2::{Oid, Repository, Signature};
use tempfile::TempDir;

use keifu::app::App;
use keifu::git::graph::build_graph;
use keifu::git::GitRepository;
use keifu::ui::graph_pixels::{build_row_spec, rasterize_row, NeighborRow};
use keifu::ui::theme::Theme;

// ── Fixture: a branchy repo (the shape that made startup regress) ────

fn commit_to(
    repo: &Repository,
    refname: &str,
    parent: Option<Oid>,
    path: &str,
    contents: &str,
) -> Oid {
    let sig = Signature::now("Perf Test", "perf@example.com").unwrap();
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

/// `n_branches` feature branches (2 commits each) hanging off a 30-commit
/// main line; half the branches point at main ancestors (already merged).
fn branchy_repo(n_branches: usize) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let mut cfg = repo.config().unwrap();
    cfg.set_str("user.name", "Perf Test").unwrap();
    cfg.set_str("user.email", "perf@example.com").unwrap();
    drop(cfg);

    let mut main_tip = commit_to(&repo, "refs/heads/main", None, "base.txt", "0");
    let mut main_line = vec![main_tip];
    for i in 1..30 {
        main_tip = commit_to(
            &repo,
            "refs/heads/main",
            Some(main_tip),
            "base.txt",
            &format!("{i}"),
        );
        main_line.push(main_tip);
    }
    repo.set_head("refs/heads/main").unwrap();

    for b in 0..n_branches {
        let name = format!("refs/heads/feature-{b}");
        if b % 2 == 0 {
            // Merged: the branch ref sits on a main ancestor.
            repo.reference(&name, main_line[b % main_line.len()], true, "perf")
                .unwrap();
        } else {
            let base = main_line[b % main_line.len()];
            let c1 = commit_to(&repo, &name, Some(base), &format!("f{b}.txt"), "one");
            commit_to(&repo, &name, Some(c1), &format!("f{b}.txt"), "two");
        }
    }
    dir
}

// ── Gates ────────────────────────────────────────────────────────────

/// Startup on a branchy repo must stay far under the per-branch-git-scan
/// regime: App::from_repo does no synchronous merged classification (dim-only
/// default), so its cost is one commit walk + one graph build.
/// Measured (debug, 2026-07-21): ~8ms on 40 branches. Budget: ~30×
/// (git tempdir I/O is the flakiest thing this suite touches).
#[test]
fn startup_budget_on_a_branchy_repo() {
    let dir = branchy_repo(40);
    let started = Instant::now();
    let app = App::from_repo(GitRepository::open(dir.path()).unwrap()).unwrap();
    let elapsed = started.elapsed();
    println!("startup on 40-branch repo: {elapsed:?}");
    assert!(!app.graph_layout.nodes.is_empty());
    assert!(
        elapsed < Duration::from_millis(250),
        "App::from_repo took {elapsed:?} on a 40-branch fixture — startup gained \
         per-branch or per-history work (budget 250ms ≈ 30× measured; see module doc)"
    );
}

/// Rasterizing a window of rows (the per-frame encode cost with a cold cache)
/// must stay linear in rows. Builds specs exactly like production
/// (`build_row_spec` with real neighbours) and rasterizes every row.
/// Measured (debug, 2026-07-21): ~46ms for the 84-row fixture graph at 10×20.
/// Budget: ~10×.
#[test]
fn rasterize_window_budget() {
    let dir = branchy_repo(40);
    let mut repo = GitRepository::open(dir.path()).unwrap();
    let branches = repo.get_branches().unwrap();
    let stashes = repo.get_stashes();
    let tags = repo.get_tags();
    let commits = repo.get_commits(200, &branches, &stashes, false).unwrap();
    let layout = build_graph(&commits, &branches, &tags, &stashes, None, None, &[]);
    let theme = Theme::dark();
    let n = layout.nodes.len();
    assert!(n >= 50, "fixture should produce a real window of rows, got {n}");

    let started = Instant::now();
    let neighbor = |i: usize| NeighborRow {
        underlay: &[],
        cells: &layout.nodes[i].cells,
    };
    let mut px_total = 0usize;
    for i in 0..n {
        let above = i.checked_sub(1).map(|j| layout.nodes[j].cells.as_slice());
        let below = (i + 1 < n).then(|| layout.nodes[i + 1].cells.as_slice());
        let spec = build_row_spec(
            above,
            &layout.nodes[i],
            below,
            &[],
            i.checked_sub(1).map(neighbor),
            (i + 1 < n).then(|| neighbor(i + 1)),
            &theme,
        );
        let img = rasterize_row(&spec, 10, 20);
        px_total += img.pixels().len();
    }
    let elapsed = started.elapsed();
    println!("rasterized {n} rows ({px_total} px) in {elapsed:?}");
    assert!(
        elapsed < Duration::from_millis(500),
        "rasterizing {n} rows took {elapsed:?} — the spec/raster pipeline gained \
         superlinear work (budget 500ms ≈ 10× measured; see module doc)"
    );
}

/// Startup instrumentation contract: App construction records its phase
/// timings (`startup.*`) so slow-startup reports carry their own breakdown.
#[test]
fn startup_phases_are_recorded() {
    let dir = branchy_repo(4);
    let app = App::from_repo(GitRepository::open(dir.path()).unwrap()).unwrap();
    let recorded: Vec<&str> = app.perf.ops().map(|(name, _)| name).collect();
    for phase in ["startup.commits", "startup.build_graph", "startup.app_new_total"] {
        assert!(
            app.perf
                .ops()
                .any(|(name, agg)| name == phase && agg.count == 1),
            "missing startup phase {phase} in perf summary: {recorded:?}"
        );
    }
}
