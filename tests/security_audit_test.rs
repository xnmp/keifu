use std::process::Command;

#[test]
fn resolved_dependencies_have_no_reported_security_advisories() {
    let output = Command::new("cargo")
        .args(["audit", "--json"])
        .output()
        .expect("cargo-audit must be installed to run the security audit");

    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo audit must emit a JSON report");
    let report_text = report.to_string();

    for advisory in [
        "RUSTSEC-2026-0194",
        "RUSTSEC-2026-0195",
        "RUSTSEC-2026-0183",
        "RUSTSEC-2026-0184",
        "RUSTSEC-2026-0008",
    ] {
        assert!(
            !report_text.contains(advisory),
            "cargo audit reported {advisory}: {report_text}"
        );
    }
}
