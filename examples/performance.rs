//! Reproducible release benchmarks. See docs/performance.md for methodology.
//! `cargo run --release --example performance -- [metric-substring]`
use std::{hint::black_box, time::Instant};

use chrono::{Local, TimeZone};
use git2::{Oid, Repository, Signature};
use keifu::{
    app::App,
    git::{
        graph::{build_graph, lineage_oids},
        CommitDiffInfo, CommitInfo, GitRepository,
    },
    ui::{
        graph_pixels::{build_row_spec, rasterize_row},
        graph_view::GraphViewWidget,
        theme::Theme,
    },
};

fn measure<T>(name: &str, mut f: impl FnMut() -> T) {
    if std::env::args()
        .nth(1)
        .is_some_and(|filter| !name.contains(&filter))
    {
        return;
    }
    drop(black_box(f())); // warm caches; fixture construction is never timed
    let mut samples = Vec::new();
    let count = if name.starts_with("branches.classify") {
        3
    } else {
        11
    };
    for _ in 0..count {
        let start = Instant::now();
        drop(black_box(f()));
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "{}",
        serde_json::json!({"metric":name,"median_ms":samples[count/2],"p95_ms":samples[count-1],"samples_ms":samples})
    );
}

fn oid(n: usize) -> Oid {
    let mut bytes = [0; 20];
    bytes[..8].copy_from_slice(&(n as u64).to_be_bytes());
    Oid::from_bytes(&bytes).unwrap()
}

fn history(n: usize) -> Vec<CommitInfo> {
    (1..=n)
        .rev()
        .map(|i| CommitInfo {
            oid: oid(i),
            short_id: format!("{i:07x}"),
            author_name: "Performance Author".into(),
            author_email: "perf@example.com".into(),
            timestamp: Local.timestamp_opt(1_700_000_000 + i as i64, 0).unwrap(),
            message: format!("Commit {i}: update parser"),
            full_message: format!("Commit {i}: update parser\n\nDetailed explanation."),
            parent_oids: if i > 1 { vec![oid(i - 1)] } else { vec![] },
        })
        .collect()
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let sig = Signature::new(
        "Perf",
        "perf@example.com",
        &git2::Time::new(1_700_000_000, 0),
    )
    .unwrap();
    let blob = repo.blob(b"fn main() { println!(\"hello\"); }\n").unwrap();
    let mut builder = repo.treebuilder(None).unwrap();
    for i in 0..5000 {
        builder
            .insert(format!("file-{i:05}.rs"), blob, 0o100644)
            .unwrap();
    }
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let mut tip = repo
        .commit(Some("HEAD"), &sig, &sig, "Initial import", &tree, &[])
        .unwrap();
    for i in 1..1000 {
        let parent = repo.find_commit(tip).unwrap();
        tip = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                &format!("Commit {i}"),
                &tree,
                &[&parent],
            )
            .unwrap();
    }
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    dir
}

fn main() {
    let dir = fixture();
    let repo = GitRepository::open(dir.path()).unwrap();
    let branches = repo.get_branches().unwrap();
    let commits = repo.get_commits(2000, &branches, &[], false).unwrap();
    let root = commits.last().unwrap().oid;
    measure("git.load.1000", || {
        repo.get_commits(2000, &branches, &[], false).unwrap()
    });
    measure("git.status.5000_files", || {
        repo.get_working_tree_status().unwrap()
    });
    measure("git.paths.5000_files", || {
        repo.changed_paths_by_commit(&commits[commits.len() - 1..])
    });
    measure("diff.quick.5000_files", || {
        CommitDiffInfo::quick_file_list_for_commit(repo.repo(), root).unwrap()
    });
    measure("diff.full.5000_files", || {
        CommitDiffInfo::from_commit(repo.repo(), root).unwrap()
    });
    measure("app.startup.1000", || {
        App::from_repo(GitRepository::open(dir.path()).unwrap()).unwrap()
    });
    let mut app = App::from_repo(repo).unwrap();
    measure("app.refresh.1000", || app.refresh(false).unwrap());
    let theme = Theme::dark();
    // A ref-heavy fixture exposes alias attribution and repeated squash scans.
    let raw = Repository::open(dir.path()).unwrap();
    let tip = commits[0].oid;
    let fork = commits[commits.len() - 1].oid;
    let sig = Signature::new(
        "Feature",
        "feature@example.com",
        &git2::Time::new(1_700_002_000, 0),
    )
    .unwrap();
    for i in 0..60 {
        let parent = raw.find_commit(fork).unwrap();
        let tree = parent.tree().unwrap();
        let mut builder = raw.treebuilder(Some(&tree)).unwrap();
        let blob = raw.blob(format!("feature {i}\n").as_bytes()).unwrap();
        builder
            .insert(format!("feature-{i}"), blob, 0o100644)
            .unwrap();
        let tree = raw.find_tree(builder.write().unwrap()).unwrap();
        let oid = raw
            .commit(
                Some(&format!("refs/heads/feature-{i}")),
                &sig,
                &sig,
                &format!("Feature {i}"),
                &tree,
                &[&parent],
            )
            .unwrap();
        raw.reference(
            &format!("refs/remotes/origin/feature-{i}"),
            oid,
            true,
            "fixture",
        )
        .unwrap();
    }
    let branchy = keifu::git::BranchInfo::list_all(&raw).unwrap();
    measure("branches.authors.121", || {
        keifu::git::branch_authors(&raw, &branchy)
    });
    let classification_branches: Vec<_> = branchy
        .iter()
        .filter(|b| {
            b.name == "main"
                || (0..6).any(|i| {
                    b.name == format!("feature-{i}") || b.name == format!("origin/feature-{i}")
                })
        })
        .cloned()
        .collect();
    assert_eq!(classification_branches.len(), 13);
    measure("branches.classify.13", || {
        keifu::git::merged::classify_merged_branches_with_targets(
            &raw,
            &classification_branches,
            tip,
            "main",
            &Default::default(),
        )
    });
    let content =
        keifu::git::FileDiffContent::from_commit(&raw, root, std::path::Path::new("file-00000.rs"))
            .unwrap();
    let mut large = content.clone();
    large.hunks[0].lines = (0..5000)
        .map(|_| content.hunks[0].lines[0].clone())
        .collect();
    measure("diff.highlight.5000_lines", || {
        keifu::ui::file_diff_view::build_highlighted_lines(&large, &theme)
    });
    let (highlighted, hunks) = keifu::ui::file_diff_view::build_highlighted_lines(&large, &theme);
    measure("diff.layout.5000_lines", || {
        keifu::ui::file_diff_view::layout_diff_rows(&highlighted, &hunks, true, 100)
    });
    for n in [2000, 10000, 20000] {
        let commits = history(n);
        measure(&format!("graph.build.tip.{n}"), || {
            build_graph(&commits, &[], &[], &[], None, Some(oid(n)), &[])
        });
        measure(&format!("graph.build.old_head.{n}"), || {
            build_graph(&commits, &[], &[], &[], None, Some(oid(1)), &[])
        });
        let layout = build_graph(&commits, &[], &[], &[], None, Some(oid(n)), &[]);
        measure(&format!("graph.lineage.bottom.{n}"), || {
            lineage_oids(&layout, n - 1)
        });
        app.graph_layout = layout;
        app.commits = commits;
        app.recompute_visible_commits();
        app.graph_nav.graph_list_state.select(Some(n - 1));
        *app.graph_nav.graph_list_state.offset_mut() = n - 30;
        for pixel in [false, true] {
            measure(&format!("render.widget.bottom.pixel_{pixel}.{n}"), || {
                GraphViewWidget::new(&app, 120, &theme, pixel, 30, true)
            });
        }
        *app.graph_nav.graph_list_state.offset_mut() = 0;
        measure(&format!("render.widget.jump.{n}"), || {
            GraphViewWidget::new(&app, 120, &theme, false, 30, true)
        });
        app.commit_filter = "author=PERFORMANCE; message=parser".into();
        measure(&format!("filter.message_author.{n}"), || {
            app.recompute_visible_commits()
        });
        app.commit_filter.clear();
        let spec = build_row_spec(
            None,
            &app.graph_layout.nodes[0],
            None,
            &[],
            None,
            None,
            &theme,
        );
        if n == 2000 {
            measure("raster.100_rows", || {
                (0..100)
                    .map(|_| rasterize_row(&spec, 10, 20))
                    .collect::<Vec<_>>()
            });
        }
    }
}
