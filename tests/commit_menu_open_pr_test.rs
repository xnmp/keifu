//! Browser-based Open PR action contract (issue #132).

use keifu::action::Action;
use keifu::app::{App, AppMode, CommitMenuItem, FocusedPanel};
use keifu::git::GitRepository;
use keifu::pr::{compare_create_url, parse_pr_list, parse_repo_info, PrRepoInfo};

mod common;
use common::{add_bare_origin, commit_file, git_cli, init_repo, Seed};

fn app_at(path: &str) -> App {
    let mut app = App::from_repo(GitRepository::open(path).unwrap()).unwrap();
    app.focused_panel = FocusedPanel::Graph;
    app.pr_repo_info = Some(PrRepoInfo {
        url: "https://github.com/example/project".to_string(),
        default_base: "main".to_string(),
    });
    app
}

fn menu_items_for_branch(app: &mut App, branch: &str) -> Vec<CommitMenuItem> {
    let pos = app
        .graph_nav
        .branch_positions
        .iter()
        .position(|(_, name)| name == branch)
        .unwrap_or_else(|| panic!("{branch} should be a graph ref"));
    let (node_idx, _) = app.graph_nav.branch_positions[pos];
    app.graph_nav.graph_list_state.select(Some(node_idx));
    app.graph_nav.selected_branch_position = Some(pos);

    app.handle_action(Action::OpenCommitMenu).unwrap();
    match &app.mode {
        AppMode::CommitMenu { items, .. } => items.clone(),
        other => panic!("expected a CommitMenu, got {other:?}"),
    }
}

fn repo_with_ahead_feature() -> (tempfile::TempDir, tempfile::TempDir, String) {
    let (working, repo) = init_repo(Seed::TrackedFile);
    let path = repo.path.clone();
    git_cli(&path, &["branch", "-m", "main"]);
    let origin = add_bare_origin(&path);
    git_cli(&path, &["checkout", "-b", "feature/browser-pr"]);
    commit_file(repo.repo(), "feature.txt", "new", "feature work");
    git_cli(&path, &["push", "origin", "feature/browser-pr"]);
    git_cli(&path, &["checkout", "main"]);
    (working, origin, path)
}

#[test]
fn ahead_branch_without_open_pr_displays_browser_action() {
    let (_working, _origin, path) = repo_with_ahead_feature();
    let mut app = app_at(&path);

    let items = menu_items_for_branch(&mut app, "feature/browser-pr");

    assert!(
        items.contains(&CommitMenuItem::OpenPrInBrowser),
        "an ahead branch without an open PR must offer the browser action, got {items:?}"
    );
    assert_eq!(
        CommitMenuItem::OpenPrInBrowser.label(),
        "Open PR in browser"
    );
}

#[test]
fn base_equal_and_open_pr_branches_do_not_display_browser_action() {
    let (_working, _origin, path) = repo_with_ahead_feature();
    git_cli(&path, &["branch", "equal-to-main", "main"]);
    let mut app = app_at(&path);

    for branch in ["main", "equal-to-main"] {
        let items = menu_items_for_branch(&mut app, branch);
        assert!(
            !items.contains(&CommitMenuItem::OpenPrInBrowser),
            "{branch} is not ahead of the default base, got {items:?}"
        );
        app.mode = AppMode::Normal;
    }

    app.open_prs = parse_pr_list(
        r#"[{"number":12,"url":"https://github.com/example/project/pull/12","headRefName":"feature/browser-pr","title":"Feature","state":"OPEN"}]"#,
    );
    let items = menu_items_for_branch(&mut app, "feature/browser-pr");
    assert!(
        !items.contains(&CommitMenuItem::OpenPrInBrowser),
        "a branch with an open PR must not offer another one, got {items:?}"
    );
}

#[test]
fn github_repo_metadata_supplies_authoritative_default_base() {
    let info = parse_repo_info(
        r#"{"url":"https://github.com/example/project","defaultBranchRef":{"name":"develop"}}"#,
    )
    .unwrap();

    assert_eq!(
        info,
        PrRepoInfo {
            url: "https://github.com/example/project".to_string(),
            default_base: "develop".to_string(),
        }
    );
}

#[test]
fn compare_url_preselects_default_base_and_slash_head() {
    assert_eq!(
        compare_create_url(
            "https://github.com/example/project/",
            "release/next",
            "feature/browser-pr",
        ),
        "https://github.com/example/project/compare/release/next...feature/browser-pr?expand=1"
    );
}
