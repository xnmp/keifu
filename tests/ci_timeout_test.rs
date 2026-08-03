use serde_yaml::Value;

fn ci_workflow() -> Value {
    serde_yaml::from_str(include_str!("../.github/workflows/ci.yaml"))
        .expect("CI workflow must remain valid YAML")
}

#[test]
fn matrix_test_job_settles_within_fifteen_minutes() {
    let workflow = ci_workflow();
    let test_job = &workflow["jobs"]["test"];

    assert_eq!(
        test_job["timeout-minutes"].as_u64(),
        Some(15),
        "every OS in the shared test matrix must settle within 15 minutes"
    );
}

#[test]
fn bounded_job_still_runs_the_cross_platform_test_suite() {
    let workflow = ci_workflow();
    let test_job = &workflow["jobs"]["test"];
    let operating_systems = test_job["strategy"]["matrix"]["os"]
        .as_sequence()
        .expect("test job must retain an OS matrix")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let commands = test_job["steps"]
        .as_sequence()
        .expect("test job must retain its steps")
        .iter()
        .filter_map(|step| step["run"].as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        operating_systems,
        ["ubuntu-latest", "macos-latest", "windows-latest"]
    );
    assert!(
        commands.contains(&"cargo test"),
        "the bounded job must still run the repository test suite"
    );
}
