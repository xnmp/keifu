use keifu::{
    app::App,
    debug_server::{handle_request, DebugRequest},
    git::repository::GitRepository,
};
use std::fs;

fn test_app() -> (tempfile::TempDir, App) {
    let tempdir = tempfile::tempdir().unwrap();
    let raw_repo = git2::Repository::init(tempdir.path()).unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    let mut index = raw_repo.index().unwrap();
    fs::write(tempdir.path().join("first.txt"), "first").unwrap();
    index.add_path(std::path::Path::new("first.txt")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = raw_repo.find_tree(tree_id).unwrap();
    raw_repo
        .commit(Some("HEAD"), &signature, &signature, "first", &tree, &[])
        .unwrap();
    fs::write(tempdir.path().join("second.txt"), "second").unwrap();
    index.add_path(std::path::Path::new("second.txt")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = raw_repo.find_tree(tree_id).unwrap();
    let parent = raw_repo.head().unwrap().peel_to_commit().unwrap();
    raw_repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "second",
            &tree,
            &[&parent],
        )
        .unwrap();
    let repo = GitRepository::open(tempdir.path()).unwrap();
    let app = App::from_repo(repo).unwrap();
    (tempdir, app)
}

#[test]
fn capslock_key_drives_the_visible_toast_and_its_normal_binding() {
    let (_tmp, mut app) = test_app();
    let before = handle_request(&mut app, 120, 35, DebugRequest::State);

    assert_eq!(
        handle_request(
            &mut app,
            120,
            35,
            DebugRequest::Keys {
                keys: "<caps-down>".into()
            }
        )["ok"],
        true
    );
    let after = handle_request(&mut app, 120, 35, DebugRequest::State);
    let dump = handle_request(
        &mut app,
        120,
        35,
        DebugRequest::Dump {
            width: Some(120),
            height: Some(35),
        },
    );

    assert_ne!(
        after["selected_index"], before["selected_index"],
        "the Caps Lock-reported down key must still run its normal navigation binding"
    );
    assert!(dump["screen"].as_str().unwrap().contains("Caps Lock is on"));
}

#[test]
fn capslock_warning_is_once_per_reported_session_and_rearms_when_inactive() {
    let (_tmp, mut app) = test_app();

    handle_request(
        &mut app,
        120,
        35,
        DebugRequest::Keys {
            keys: "<caps-k> <caps-k>".into(),
        },
    );
    assert_eq!(app.toasts.visible().len(), 1, "one session warns once");

    handle_request(
        &mut app,
        120,
        35,
        DebugRequest::Keys {
            keys: "k <caps-k>".into(),
        },
    );
    assert_eq!(
        app.toasts.visible().len(),
        2,
        "an inactive key re-arms the next reported session"
    );
}

#[test]
fn absent_capslock_state_does_not_infer_a_warning_from_uppercase_text() {
    let (_tmp, mut app) = test_app();
    handle_request(&mut app, 120, 35, DebugRequest::Keys { keys: "K".into() });

    assert!(app.toasts.is_empty());
}
