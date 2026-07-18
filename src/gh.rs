//! Small shared helper for running the `gh` CLI on a background thread with a
//! timeout, capturing output. Used by the CI-checks and PR-thread features.

use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Captured result of a `gh` invocation.
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run `gh` with `args` in `repo_path`, with a timeout. `Err` only when gh
/// can't be run at all (missing binary, timeout, spawn failure); a non-zero
/// exit is returned in `Output` so the caller can decide (some `gh` subcommands
/// exit non-zero while still printing useful output — e.g. `gh pr checks`).
pub fn run(repo_path: &str, args: &[&str], timeout: Duration) -> Result<Output, String> {
    let mut child = Command::new("gh")
        .args(args)
        .current_dir(repo_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("gh not available: {e}"))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("gh timed out".to_string());
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("gh failed: {e}")),
        }
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    Ok(Output {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}
