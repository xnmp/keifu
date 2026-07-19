//! In-process credential handling for HTTPS git operations.
//!
//! When a push/fetch/pull fails because git can't read a username/password from
//! the (disabled) terminal, keifu prompts the user in-TUI and retries the same
//! command with the entered credentials supplied through `GIT_ASKPASS`.
//!
//! Credentials never touch argv, the remote URL, or disk. They're passed to the
//! child git process as environment variables (`KEIFU_ASKPASS_USER` /
//! `KEIFU_ASKPASS_PASS`), read by a tiny shim script that git invokes for each
//! prompt. The shim itself contains no secrets — it only echoes env vars — so it
//! can live harmlessly in the system temp dir.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Context, Result};

/// A username/password (or personal-access-token) pair for one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// The shim script git runs for every credential prompt. `$1` is the prompt
/// text ("Username for '…'" / "Password for '…'"); we branch on it and echo the
/// matching env var with no trailing newline.
const ASKPASS_SCRIPT: &str = "#!/bin/sh\ncase \"$1\" in\n*[Uu]sername*) printf '%s' \"$KEIFU_ASKPASS_USER\" ;;\n*) printf '%s' \"$KEIFU_ASKPASS_PASS\" ;;\nesac\n";

/// Environment variable names the shim reads. Public so callers set them on the
/// child process alongside `GIT_ASKPASS`.
pub const ENV_USER: &str = "KEIFU_ASKPASS_USER";
pub const ENV_PASS: &str = "KEIFU_ASKPASS_PASS";

static SHIM_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Write (once per process) the askpass shim to the temp dir and return its
/// path, ready to hand to git via `GIT_ASKPASS`. The script is credential-free,
/// so a persisted copy is harmless; it's created mode 0700 on Unix.
pub fn ensure_askpass_shim() -> Result<PathBuf> {
    if let Some(path) = SHIM_PATH.get() {
        return Ok(path.clone());
    }
    let path = std::env::temp_dir().join("keifu-askpass.sh");
    write_shim(&path).context("Failed to write askpass shim")?;
    // Ignore a race where another thread won the OnceLock; both wrote identical
    // content, so either path is correct.
    let _ = SHIM_PATH.set(path.clone());
    Ok(SHIM_PATH.get().cloned().unwrap_or(path))
}

#[cfg(unix)]
fn write_shim(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o700)
        .open(path)?;
    file.write_all(ASKPASS_SCRIPT.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_shim(path: &std::path::Path) -> Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(ASKPASS_SCRIPT.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_branches_on_username_vs_password() {
        // The script must route a "Username" prompt to the user var and
        // everything else (the password prompt) to the pass var.
        assert!(ASKPASS_SCRIPT.contains("[Uu]sername*) printf '%s' \"$KEIFU_ASKPASS_USER\""));
        assert!(ASKPASS_SCRIPT.contains("*) printf '%s' \"$KEIFU_ASKPASS_PASS\""));
    }

    #[test]
    fn ensure_shim_is_stable_and_executable() {
        let a = ensure_askpass_shim().unwrap();
        let b = ensure_askpass_shim().unwrap();
        assert_eq!(a, b, "shim path must be stable across calls");
        assert!(a.exists());
        let contents = std::fs::read_to_string(&a).unwrap();
        assert_eq!(contents, ASKPASS_SCRIPT);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&a).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }
}
