use keifu::{
    app::{App, LaunchMode},
    ui,
};
use ratatui::{backend::TestBackend, Terminal};

mod common;

fn rendered_screen(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| ui::draw(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(120)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn bare_launch_renders_only_an_untitled_graph_without_a_status_bar() {
    let mut app = App::test_fixture();
    app.launch_mode = LaunchMode::Bare;

    let screen = rendered_screen(&mut app);

    assert!(screen.lines().next().unwrap().contains('╭'));
    assert!(
        !screen.lines().next().unwrap().contains("Commit"),
        "bare graph must not have a commit title: {screen}"
    );
    for absent in ["Changed Files", "Commit Detail", "help"] {
        assert!(
            !screen.contains(absent),
            "bare launch must not render {absent:?}: {screen}"
        );
    }
}

#[test]
fn scm_launch_renders_only_untitled_files_and_commit_detail_without_a_status_bar() {
    let (_repo_dir, repo) = common::init_repo(common::Seed::TrackedFile);
    let mut app = App::from_repo(repo).unwrap();
    app.launch_mode = LaunchMode::Scm;

    let screen = rendered_screen(&mut app);

    assert!(
        screen.contains("empty commit"),
        "SCM files pane is missing: {screen}"
    );
    assert!(
        screen.contains("Author:"),
        "SCM commit-detail pane is missing: {screen}"
    );
    for absent in ["Commits", "Changed Files", "Commit Detail", "help"] {
        assert!(
            !screen.contains(absent),
            "SCM launch must not render {absent:?}: {screen}"
        );
    }
}

#[test]
fn full_launch_keeps_the_existing_complete_layout() {
    let mut app = App::test_fixture();
    let screen = rendered_screen(&mut app);

    for expected in ["Commits", "Changed Files", "Commit Detail", "help"] {
        assert!(
            screen.contains(expected),
            "full launch must retain {expected:?}: {screen}"
        );
    }
}
