//! Behavioral contracts for performance caching and background delivery.
mod common;
use common::{commit_file, init_repo, Seed};
use keifu::{
    action::Action,
    app::{App, AppMode},
    git::{CommitDiffInfo, GitRepository},
};
use std::{
    path::Path,
    time::{Duration, Instant},
};

fn settle(app: &mut App) {
    let until = Instant::now() + Duration::from_secs(10);
    while app.file_diff_is_loading() || app.commit_paths_are_loading() {
        app.poll_background_work();
        assert!(Instant::now() < until, "background work did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn quiet_refresh_reuses_layout_but_refs_tags_worktree_and_filters_update() {
    let (dir, repo) = init_repo(Seed::TrackedFile);
    let mut app = App::from_repo(repo).unwrap();
    app.refresh(false).unwrap();
    let generation = app.graph_generation;
    let selected = app.graph_nav.selected_index();
    app.refresh(false).unwrap();
    assert_eq!(app.graph_generation, generation);
    assert_eq!(app.graph_nav.selected_index(), selected);
    std::fs::write(dir.path().join("tracked.txt"), "modified\n").unwrap();
    app.refresh(false).unwrap();
    assert!(app.graph_layout.nodes.iter().any(|n| n.is_uncommitted));
    let raw = git2::Repository::open(dir.path()).unwrap();
    let oid = commit_file(&raw, "tracked.txt", "committed\n", "new content");
    app.refresh(false).unwrap();
    assert!(app
        .graph_layout
        .nodes
        .iter()
        .any(|n| n.commit.as_ref().is_some_and(|c| c.oid == oid)));
    assert!(!app.graph_layout.nodes.iter().any(|n| n.is_uncommitted));
    raw.tag_lightweight("release", raw.find_commit(oid).unwrap().as_object(), false)
        .unwrap();
    app.refresh(false).unwrap();
    assert!(app
        .graph_layout
        .nodes
        .iter()
        .any(|n| n.tag_names.iter().any(|t| t == "release")));
    app.commit_filter = "message=initial".into();
    app.refresh(false).unwrap();
    assert_eq!(
        app.graph_layout
            .nodes
            .iter()
            .filter(|n| n.commit.is_some())
            .count(),
        1
    );
    app.commit_filter.clear();
    app.refresh(false).unwrap();
    assert_eq!(
        app.graph_layout
            .nodes
            .iter()
            .filter(|n| n.commit.is_some())
            .count(),
        2
    );
}

#[test]
fn asynchronous_file_diff_delivers_actual_content_and_cannot_reopen_after_cancel() {
    let (_dir, repo) = init_repo(Seed::TrackedFile);
    let mut app = App::from_repo(repo).unwrap();
    app.background.threaded = true;
    app.update_diff_cache();
    app.sync_file_list_cache();
    app.handle_action(Action::OpenFileDiff).unwrap();
    assert!(matches!(app.mode, AppMode::FileDiff { .. }));
    settle(&mut app);
    let AppMode::FileDiff {
        content,
        rendered_lines,
        ..
    } = &app.mode
    else {
        panic!("diff closed");
    };
    assert_eq!(content.path, Path::new("tracked.txt"));
    assert!(rendered_lines
        .iter()
        .any(|line| line.to_string().contains("tracked")));
    app.handle_action(Action::NextFile).unwrap();
    app.handle_action(Action::Cancel).unwrap();
    for _ in 0..20 {
        app.poll_background_work();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(app.mode, AppMode::Normal));
}

#[test]
fn background_path_index_applies_the_latest_query_and_keeps_merge_ancestry() {
    let (_dir, repo) = init_repo(Seed::TrackedFile);
    commit_file(repo.repo(), "other.rs", "other\n", "other file");
    let wanted = commit_file(repo.repo(), "tracked.txt", "updated\n", "wanted");
    let mut app = App::from_repo(repo).unwrap();
    app.background.threaded = true;
    app.commit_filter = "file=missing".into();
    app.refresh(false).unwrap();
    app.commit_filter = "file=tracked.txt; message=wanted".into();
    app.refresh(false).unwrap();
    settle(&mut app);
    assert_eq!(app.commit_filter_matches.len(), 1);
    assert!(app.commit_filter_matches.contains(&wanted));
    assert_eq!(
        app.graph_layout
            .nodes
            .iter()
            .filter(|n| n.commit.is_some())
            .count(),
        3
    );
}

#[test]
fn worktree_cache_identity_changes_on_staging_without_worktree_edits() {
    let (dir, repo) = init_repo(Seed::TrackedFile);
    std::fs::write(dir.path().join("tracked.txt"), "changed\n").unwrap();
    let before = repo.get_working_tree_status().unwrap().unwrap();
    let mut index = repo.repo().index().unwrap();
    index.add_path(Path::new("tracked.txt")).unwrap();
    index.write().unwrap();
    let after = repo.get_working_tree_status().unwrap().unwrap();
    assert_ne!(before, after);
    assert_eq!(before.file_paths, after.file_paths);
    let reopened = GitRepository::open(dir.path()).unwrap();
    let diff = CommitDiffInfo::from_working_tree(reopened.repo()).unwrap();
    assert_eq!(diff.staged_files.len(), 1);
    assert!(diff.unstaged_files.is_empty());
}

#[test]
fn resize_during_async_preparation_delivers_the_current_wrap_width() {
    let (_dir, repo) = init_repo(Seed::TrackedFile);
    commit_file(repo.repo(), "wide.txt", &"long text ".repeat(100), "wide");
    let mut app = App::from_repo(repo).unwrap();
    app.background.threaded = true;
    app.diff_word_wrap = true;
    app.diff_viewport_width = 100;
    app.update_diff_cache();
    app.sync_file_list_cache();
    app.handle_action(Action::OpenFileDiff).unwrap();
    app.diff_viewport_width = 20;
    settle(&mut app);
    let AppMode::FileDiff {
        rendered_lines,
        content,
        ..
    } = &app.mode
    else {
        panic!("diff closed")
    };
    assert_eq!(content.path, Path::new("wide.txt"));
    assert!(rendered_lines.len() > 40);
    assert!(rendered_lines.iter().skip(1).all(|line| line.width() <= 20));
}

#[test]
fn threaded_wrap_toggle_resets_pan_and_preserves_line_beginnings() {
    let (_dir, repo) = init_repo(Seed::TrackedFile);
    commit_file(
        repo.repo(),
        "wide.txt",
        &format!("BEGIN {}", "word ".repeat(100)),
        "wide",
    );
    let mut app = App::from_repo(repo).unwrap();
    app.background.threaded = true;
    app.diff_word_wrap = false;
    app.update_diff_cache();
    app.sync_file_list_cache();
    app.handle_action(Action::OpenFileDiff).unwrap();
    settle(&mut app);
    if let AppMode::FileDiff {
        horizontal_offset, ..
    } = &mut app.mode
    {
        *horizontal_offset = 30;
    }
    app.handle_action(Action::ToggleDiffWrap).unwrap();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 25)).unwrap();
    terminal
        .draw(|frame| keifu::ui::draw(frame, &mut app))
        .unwrap();
    settle(&mut app);
    let AppMode::FileDiff {
        horizontal_offset,
        rendered_lines,
        ..
    } = &app.mode
    else {
        panic!("diff closed")
    };
    assert_eq!(*horizontal_offset, 0);
    assert!(rendered_lines
        .iter()
        .any(|line| line.to_string().contains("BEGIN")));
}

#[test]
fn deepening_recomputes_boundary_paths_for_the_same_displayed_commits() {
    check_deepening_paths(false);
    check_deepening_paths(true);
}

fn check_deepening_paths(force: bool) {
    let (source, repo) = init_repo(Seed::TrackedFile);
    commit_file(repo.repo(), "middle.txt", "middle", "middle");
    let wanted = commit_file(repo.repo(), "boundary.txt", "boundary", "boundary");
    commit_file(repo.repo(), "tip.txt", "tip", "tip");
    let clone = tempfile::tempdir().unwrap();
    let output = std::process::Command::new("git")
        .args(["clone", "--depth=2", "--no-local"])
        .arg(source.path())
        .arg(clone.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut app = App::from_repo(GitRepository::open(clone.path()).unwrap()).unwrap();
    app.background.threaded = true;
    app.commit_load_limit = 2;
    app.commit_filter = "file=middle.txt".into();
    app.refresh(false).unwrap();
    settle(&mut app);
    assert!(app.commit_filter_matches.contains(&wanted));
    let boundary_index = app
        .graph_layout
        .nodes
        .iter()
        .position(|n| n.commit.as_ref().is_some_and(|c| c.oid == wanted))
        .unwrap();
    app.graph_nav.graph_list_state.select(Some(boundary_index));
    app.update_diff_cache();
    assert!(app
        .cached_diff_or_quick()
        .unwrap()
        .files
        .iter()
        .any(|f| f.path == Path::new("middle.txt")));
    let displayed: Vec<_> = app.commits.iter().map(|c| c.oid).collect();
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(clone.path())
        .args(["fetch", "--deepen=1"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    app.refresh(force).unwrap();
    settle(&mut app);
    assert_eq!(
        displayed,
        app.commits.iter().map(|c| c.oid).collect::<Vec<_>>()
    );
    assert!(!app.commit_filter_matches.contains(&wanted));
    app.commit_filter.clear();
    app.refresh(false).unwrap();
    let boundary_index = app
        .graph_layout
        .nodes
        .iter()
        .position(|n| n.commit.as_ref().is_some_and(|c| c.oid == wanted))
        .unwrap();
    app.graph_nav.graph_list_state.select(Some(boundary_index));
    app.update_diff_cache();
    let files = &app.cached_diff_or_quick().unwrap().files;
    assert!(files.iter().any(|f| f.path == Path::new("boundary.txt")));
    assert!(!files.iter().any(|f| f.path == Path::new("middle.txt")));
}

#[test]
fn deepening_invalidates_oldest_unique_branch_author() {
    fn author(repo: &git2::Repository, name: &str) {
        let signature = git2::Signature::now(name, "author@example.com").unwrap();
        repo.head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .amend(Some("HEAD"), Some(&signature), None, None, None, None)
            .unwrap();
    }
    fn remove_remote_aliases(repo: &git2::Repository) {
        let refs: Vec<_> = repo
            .references_glob("refs/remotes/*")
            .unwrap()
            .map(|r| r.unwrap().name().unwrap().to_string())
            .collect();
        for name in refs {
            repo.find_reference(&name).unwrap().delete().unwrap();
        }
    }
    let (source, repo) = init_repo(Seed::TrackedFile);
    commit_file(repo.repo(), "middle", "middle", "middle");
    author(repo.repo(), "Middle Author");
    commit_file(repo.repo(), "boundary", "boundary", "boundary");
    author(repo.repo(), "Boundary Author");
    commit_file(repo.repo(), "tip", "tip", "tip");
    let clone = tempfile::tempdir().unwrap();
    let output = std::process::Command::new("git")
        .args(["clone", "--depth=2", "--no-local"])
        .arg(source.path())
        .arg(clone.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let raw = git2::Repository::open(clone.path()).unwrap();
    remove_remote_aliases(&raw);
    let mut app = App::from_repo(GitRepository::open(clone.path()).unwrap()).unwrap();
    app.handle_action(Action::OpenBranchFilter).unwrap();
    assert!(app.branch_authors.values().any(|a| a == "Boundary Author"));
    app.handle_action(Action::Cancel).unwrap();
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(clone.path())
        .args(["fetch", "--deepen=1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    remove_remote_aliases(&raw);
    app.refresh(false).unwrap();
    app.handle_action(Action::OpenBranchFilter).unwrap();
    assert!(app.branch_authors.values().any(|a| a == "Middle Author"));
    assert!(!app.branch_authors.values().any(|a| a == "Boundary Author"));
}
