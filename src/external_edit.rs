//! External-editor pop-out for the compose editors.
//!
//! In the PR- and issue-compose modes, Ctrl+E hands the current buffer to the
//! user's terminal editor (`$VISUAL`, then `$EDITOR`, falling back to `vi`).
//! This module is pure/infrastructure: it resolves the editor command, writes
//! the text to a temp file, spawns the editor inheriting stdio, waits, and reads
//! the result back. It never touches the TUI terminal — `main.rs` owns the
//! suspend/restore around the call, since it is the only place that controls raw
//! mode and the alternate screen.

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

/// Which compose buffer a pending external-edit request targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalEditTarget {
    /// `App.pr_editor` (PR create / review compose).
    Pr,
    /// `App.issue_editor` (new issue / comment compose).
    Issue,
}

/// A resolved editor invocation: the program plus any leading arguments (e.g.
/// `code -w` → program `code`, args `["-w"]`). The temp-file path is appended
/// by the caller as the final argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// Resolve the editor command from the `VISUAL`/`EDITOR` environment values.
///
/// Precedence follows git's convention: `VISUAL` wins over `EDITOR`, and both
/// fall back to `vi`. Empty (or whitespace-only) values are treated as unset.
/// The value is split on whitespace so simple forms carrying flags work
/// (`"code -w"`, `"emacs -nw"`). Shell quoting is **not** supported — matching
/// git's own simple splitting behavior is intentional.
pub fn resolve_editor(visual: Option<&str>, editor: Option<&str>) -> EditorCommand {
    let chosen = first_nonempty(visual)
        .or_else(|| first_nonempty(editor))
        .unwrap_or("vi");
    parse_command(chosen)
}

/// The trimmed value if it is present and not empty/whitespace-only.
fn first_nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// Split a command string into program + args on whitespace.
fn parse_command(spec: &str) -> EditorCommand {
    let mut parts = spec.split_whitespace().map(str::to_string);
    let program = parts.next().unwrap_or_else(|| "vi".to_string());
    let args = parts.collect();
    EditorCommand { program, args }
}

/// Normalize text coming back from an editor: convert CRLF to LF and drop
/// trailing blank lines (editors commonly append a trailing newline). Interior
/// content is left untouched.
pub fn normalize_edited_text(text: &str) -> String {
    let unix = text.replace("\r\n", "\n").replace('\r', "\n");
    unix.trim_end_matches('\n').to_string()
}

/// Edit `text` in the user's terminal editor, returning the edited (normalized)
/// text. The caller must have suspended the TUI first. Errors on editor spawn
/// failure or a non-zero exit (interpreted as "abort, keep original").
pub fn edit_text(text: &str) -> Result<String> {
    let visual = std::env::var("VISUAL").ok();
    let editor = std::env::var("EDITOR").ok();
    let cmd = resolve_editor(visual.as_deref(), editor.as_deref());
    edit_text_with(&cmd, text)
}

/// Core of [`edit_text`], parameterized by the resolved command for testing.
fn edit_text_with(cmd: &EditorCommand, text: &str) -> Result<String> {
    let path = temp_file_path();
    std::fs::write(&path, text)
        .with_context(|| format!("failed to write temp file {}", path.display()))?;
    // Read back (and clean up) regardless of how the editor exits.
    let result = run_editor(cmd, &path);
    let _ = std::fs::remove_file(&path);
    result
}

/// Spawn the editor (inheriting stdin/stdout/stderr), wait, and read the file.
fn run_editor(cmd: &EditorCommand, path: &std::path::Path) -> Result<String> {
    let status = Command::new(&cmd.program)
        .args(&cmd.args)
        .arg(path)
        .status()
        .with_context(|| format!("failed to launch editor '{}'", cmd.program))?;
    if !status.success() {
        bail!("editor exited without saving");
    }
    let edited = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read temp file {}", path.display()))?;
    Ok(normalize_edited_text(&edited))
}

/// A unique temp-file path under the system temp dir, with an `.md` extension so
/// editors enable Markdown highlighting for the compose body.
fn temp_file_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut path = std::env::temp_dir();
    path.push(format!("keifu-compose-{}-{}.md", std::process::id(), nanos));
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_takes_precedence_over_editor() {
        let cmd = resolve_editor(Some("micro"), Some("vim"));
        assert_eq!(cmd.program, "micro");
        assert!(cmd.args.is_empty());
    }

    #[test]
    fn falls_back_to_editor_when_visual_unset() {
        let cmd = resolve_editor(None, Some("nano"));
        assert_eq!(cmd.program, "nano");
        assert!(cmd.args.is_empty());
    }

    #[test]
    fn falls_back_to_vi_when_both_unset() {
        assert_eq!(
            resolve_editor(None, None),
            EditorCommand {
                program: "vi".to_string(),
                args: vec![]
            }
        );
    }

    #[test]
    fn empty_and_whitespace_values_are_treated_as_unset() {
        // Empty VISUAL falls through to EDITOR.
        assert_eq!(resolve_editor(Some(""), Some("nano")).program, "nano");
        // Whitespace-only VISUAL falls through to EDITOR.
        assert_eq!(resolve_editor(Some("   "), Some("nano")).program, "nano");
        // Both empty → vi.
        assert_eq!(resolve_editor(Some(""), Some("")).program, "vi");
        assert_eq!(resolve_editor(Some("  "), None).program, "vi");
    }

    #[test]
    fn multi_word_values_split_into_program_and_args() {
        let cmd = resolve_editor(Some("code -w"), None);
        assert_eq!(cmd.program, "code");
        assert_eq!(cmd.args, vec!["-w".to_string()]);

        let cmd = resolve_editor(Some("emacs -nw -q"), None);
        assert_eq!(cmd.program, "emacs");
        assert_eq!(cmd.args, vec!["-nw".to_string(), "-q".to_string()]);
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_before_splitting() {
        let cmd = resolve_editor(Some("  code -w  "), None);
        assert_eq!(cmd.program, "code");
        assert_eq!(cmd.args, vec!["-w".to_string()]);
    }

    #[test]
    fn normalize_strips_trailing_newlines() {
        assert_eq!(normalize_edited_text("hello\n"), "hello");
        assert_eq!(normalize_edited_text("hello\n\n\n"), "hello");
        // Interior newlines survive; only trailing ones are dropped.
        assert_eq!(normalize_edited_text("a\n\nb\n"), "a\n\nb");
    }

    #[test]
    fn normalize_converts_crlf_to_lf() {
        assert_eq!(normalize_edited_text("a\r\nb\r\n"), "a\nb");
        assert_eq!(normalize_edited_text("lone\rcr"), "lone\ncr");
    }

    #[test]
    fn normalize_handles_empty_and_blank_input() {
        assert_eq!(normalize_edited_text(""), "");
        assert_eq!(normalize_edited_text("\n\n"), "");
    }

    #[test]
    fn edit_text_with_reads_back_editor_output() {
        // Use `cp` as a fake "editor": it copies a fixture over the temp file,
        // exercising the write → spawn → read-back → cleanup path without an
        // interactive editor.
        let dir = std::env::temp_dir();
        let fixture = dir.join(format!("keifu-fixture-{}.txt", std::process::id()));
        std::fs::write(&fixture, "edited body\n").unwrap();
        // `cp <fixture> <tempfile>` — tempfile is appended as the final arg.
        let cmd = EditorCommand {
            program: "cp".to_string(),
            args: vec![fixture.to_string_lossy().into_owned()],
        };
        let out = edit_text_with(&cmd, "original").unwrap();
        assert_eq!(out, "edited body");
        let _ = std::fs::remove_file(&fixture);
    }

    #[test]
    fn edit_text_with_errors_on_nonzero_exit() {
        // `false` exits non-zero without writing → treated as abort.
        let cmd = EditorCommand {
            program: "false".to_string(),
            args: vec![],
        };
        assert!(edit_text_with(&cmd, "keep me").is_err());
    }

    #[test]
    fn edit_text_with_errors_on_spawn_failure() {
        let cmd = EditorCommand {
            program: "keifu-no-such-editor-binary-xyz".to_string(),
            args: vec![],
        };
        assert!(edit_text_with(&cmd, "keep me").is_err());
    }
}
