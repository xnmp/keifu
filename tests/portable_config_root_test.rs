use keifu::config::keifu_config_dir;

#[test]
fn explicit_keifu_config_root_is_platform_independent() {
    // This integration-test binary contains only this test, so its process
    // environment cannot race a sibling test.
    let isolated = tempfile::tempdir().unwrap();
    std::env::set_var("KEIFU_CONFIG_DIR", isolated.path());
    assert_eq!(keifu_config_dir().as_deref(), Some(isolated.path()));
    std::env::remove_var("KEIFU_CONFIG_DIR");
}
