//! Incremental commit loading: chunk-continuation correctness and a 10k-commit
//! rebuild profile (the latter #[ignore]d — run with `--ignored --nocapture`).

use chrono::Local;
use git2::{Oid, Repository, Signature, Time};
use keifu::app::App;
use keifu::git::{build_graph, BranchInfo, CommitInfo, GitRepository};
use tempfile::TempDir;

/// The OID currently selected in `app`, if any.
fn selected_oid(app: &App) -> Option<Oid> {
    app.graph_nav
        .graph_list_state
        .selected()
        .and_then(|i| app.graph_layout.nodes.get(i))
        .and_then(|n| n.commit.as_ref())
        .map(|c| c.oid)
}

/// Row index of the node carrying `oid`.
fn row_of(app: &App, oid: Oid) -> usize {
    app.graph_layout
        .nodes
        .iter()
        .position(|n| n.commit.as_ref().map(|c| c.oid) == Some(oid))
        .unwrap()
}

/// A deterministic fake OID from an index.
fn oid(i: u32) -> Oid {
    let mut bytes = [0u8; 20];
    bytes[0..4].copy_from_slice(&i.to_be_bytes());
    Oid::from_bytes(&bytes).unwrap()
}

fn synthetic_commit(i: u32, parents: Vec<u32>) -> CommitInfo {
    CommitInfo {
        oid: oid(i),
        short_id: format!("{i:07}"),
        author_name: "Dev".into(),
        author_email: "dev@example.com".into(),
        timestamp: Local::now(),
        message: format!("commit {i}"),
        full_message: format!("commit {i}"),
        parent_oids: parents.into_iter().map(oid).collect(),
    }
}

/// A branchy 10k-commit history: a linear main line with a short feature branch
/// forking and merging every 20 commits.
fn synthetic_history(n: u32) -> (Vec<CommitInfo>, Vec<BranchInfo>) {
    let mut commits = Vec::new();
    // Newest-first ordering (as the revwalk yields).
    for i in (1..=n).rev() {
        let parents = if i == 1 {
            vec![]
        } else if i % 20 == 0 {
            // A merge commit pulling in a sibling (i-1 and i-2).
            vec![i - 1, i - 2]
        } else {
            vec![i - 1]
        };
        commits.push(synthetic_commit(i, parents));
    }
    let branches = vec![BranchInfo {
        name: "main".into(),
        tip_oid: oid(n),
        is_head: true,
        is_remote: false,
        upstream: None,
        ahead: 0,
        behind: 0,
    }];
    (commits, branches)
}

#[test]
#[ignore = "profiling: run with --ignored --nocapture"]
fn profile_10k_rebuild() {
    let (commits, branches) = synthetic_history(10_000);
    let start = std::time::Instant::now();
    let layout = build_graph(&commits, &branches, &[], &[], None, None, None);
    let elapsed = start.elapsed();
    println!(
        "build_graph over {} commits: {:?} (max_lane {})",
        layout.nodes.len(),
        elapsed,
        layout.max_lane
    );
    assert!(layout.nodes.len() >= 10_000);
}

// ── chunk continuation against a real repo ─────────────────────────────

fn commit(repo: &Repository, refname: &str, parent: Option<Oid>, secs: i64, content: &str) -> Oid {
    let blob = repo.blob(content.as_bytes()).unwrap();
    let mut tb = repo.treebuilder(None).unwrap();
    tb.insert("f", blob, 0o100644).unwrap();
    let tree = repo.find_tree(tb.write().unwrap()).unwrap();
    let sig = Signature::new("T", "t@e", &Time::new(secs, 0)).unwrap();
    let parents: Vec<_> = parent.iter().map(|p| repo.find_commit(*p).unwrap()).collect();
    let refs: Vec<&git2::Commit> = parents.iter().collect();
    repo.commit(Some(refname), &sig, &sig, "m", &tree, &refs).unwrap()
}

/// A linear repo of `n` commits on main.
fn linear_repo(n: i64) -> (TempDir, GitRepository, Vec<Oid>) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let mut oids = Vec::new();
    let mut parent = None;
    for i in 0..n {
        let o = commit(&repo, "refs/heads/main", parent, 1000 + i, &format!("c{i}"));
        oids.push(o);
        parent = Some(o);
    }
    repo.set_head("refs/heads/main").unwrap();
    let git = GitRepository::open(dir.path()).unwrap();
    (dir, git, oids)
}

#[test]
fn chunk_continuation_has_no_gaps_or_duplicates() {
    // 30 commits; loading in chunks of 10 must equal the full walk prefix.
    let (_dir, git, _oids) = linear_repo(30);
    let branches = git.get_branches().unwrap();

    let full = git.get_commits(30, &branches, &[]).unwrap();
    let full_oids: Vec<Oid> = full.iter().map(|c| c.oid).collect();

    for limit in [10usize, 20, 30, 40] {
        let chunk = git.get_commits(limit, &branches, &[]).unwrap();
        let chunk_oids: Vec<Oid> = chunk.iter().map(|c| c.oid).collect();
        let expected = &full_oids[..limit.min(full_oids.len())];
        assert_eq!(
            chunk_oids, expected,
            "limit {limit}: chunk must be the full-walk prefix (no gaps/dupes)"
        );
    }
}

#[test]
fn extension_adds_commits_bumps_generation_and_preserves_selection() {
    let (_dir, git, _oids) = linear_repo(30);
    let mut app = App::from_repo(git).unwrap();

    // Simulate a partial initial load by capping to 5 and re-walking.
    app.commit_load_limit = 5;
    app.all_commits_loaded = false;
    app.refresh(false).unwrap();
    assert_eq!(app.commits.len(), 5);
    assert!(!app.all_commits_loaded);

    // Select the 3rd loaded commit (stays loaded after extension).
    let sel = app.commits[2].oid;
    app.graph_nav.graph_list_state.select(Some(row_of(&app, sel)));
    let gen_before = app.graph_generation;

    app.load_more_commits(false); // limit 5 -> 505, walks the full 30

    assert_eq!(app.commits.len(), 30, "the rest of the history loaded");
    assert!(app.all_commits_loaded, "walk exhausted (< limit)");
    assert_ne!(app.graph_generation, gen_before, "generation bumped");
    assert_eq!(selected_oid(&app), Some(sel), "selection preserved by OID");
}

#[test]
fn extension_respects_the_branch_filter() {
    // main: a<-b<-c ; feature forks off b with an exclusive commit f.
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let a = commit(&repo, "refs/heads/main", None, 1000, "a");
    let b = commit(&repo, "refs/heads/main", Some(a), 2000, "b");
    let _c = commit(&repo, "refs/heads/main", Some(b), 4000, "c");
    let f = commit(&repo, "refs/heads/feature", Some(b), 3000, "f"); // exclusive to feature
    repo.set_head("refs/heads/main").unwrap();
    let git = GitRepository::open(dir.path()).unwrap();

    let mut app = App::from_repo(git).unwrap();
    // Hide the feature branch, then load in a capped-then-extended fashion.
    app.hidden_branches.insert("feature".to_string());
    app.commit_load_limit = 2;
    app.all_commits_loaded = false;
    app.refresh(false).unwrap();
    assert!(!app.commits.iter().any(|c| c.oid == f), "hidden pre-extension");

    app.load_more_commits(false);
    assert!(
        !app.commits.iter().any(|c| c.oid == f),
        "the filter still excludes the hidden branch's exclusive commit after extension"
    );
    assert!(app.commits.iter().any(|c| c.oid == a), "main history present");
}
