use std::{env, fs, path::Path};

use git2::{Repository, Signature, Status};
use keifu::git::operations::stage_file;

#[test]
fn stage_file_does_not_depend_on_git_cli() {
    let repo_dir = tempfile::tempdir().expect("create repository directory");
    let repo = Repository::init(repo_dir.path()).expect("initialize repository");
    fs::write(repo_dir.path().join("tracked.txt"), "staged contents\n")
        .expect("write file to stage");

    let original_path = env::var_os("PATH");
    let empty_path = tempfile::tempdir().expect("create empty PATH directory");
    env::set_var("PATH", empty_path.path());
    let result = stage_file(
        repo_dir.path().to_str().expect("UTF-8 repository path"),
        "tracked.txt",
    );
    match original_path {
        Some(path) => env::set_var("PATH", path),
        None => env::remove_var("PATH"),
    }

    result.expect("stage file without invoking the git executable");
    let mut index = repo.index().expect("open repository index");
    index.read(true).expect("refresh repository index");
    let entry = index
        .get_path(Path::new("tracked.txt"), 0)
        .expect("staged index entry");
    let blob = repo.find_blob(entry.id).expect("read staged blob");
    assert_eq!(blob.content(), b"staged contents\n");
}

#[test]
fn stage_file_records_a_tracked_file_deletion() {
    let repo_dir = tempfile::tempdir().expect("create repository directory");
    let repo = Repository::init(repo_dir.path()).expect("initialize repository");
    let tracked_path = repo_dir.path().join("tracked.txt");
    fs::write(&tracked_path, "committed contents\n").expect("write tracked file");

    let mut index = repo.index().expect("open repository index");
    index
        .add_path(Path::new("tracked.txt"))
        .expect("add initial file");
    index.write().expect("write initial index");
    let tree_id = index.write_tree().expect("write initial tree");
    let tree = repo.find_tree(tree_id).expect("read initial tree");
    let signature = Signature::now("Keifu Test", "test@keifu.invalid").expect("signature");
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "initial",
        &tree,
        &[],
    )
    .expect("create initial commit");
    drop(tree);
    fs::remove_file(tracked_path).expect("delete tracked file");

    stage_file(
        repo_dir.path().to_str().expect("UTF-8 repository path"),
        "tracked.txt",
    )
    .expect("stage tracked-file deletion");

    assert_eq!(
        repo.status_file(Path::new("tracked.txt"))
            .expect("read staged status"),
        Status::INDEX_DELETED
    );
}
