//! Regression checks for user-facing capability claims.

#[test]
fn readme_does_not_advertise_removed_commit_signature_status() {
    let readme = include_str!("../README.md");

    assert!(
        !readme.contains("GPG signature status"),
        "README must not advertise signature status after it was removed from commit details"
    );
}
