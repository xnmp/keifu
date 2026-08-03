use std::{env, fs, path::Path};

use git2::Repository;
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
