//! Unified-diff patch synthesis for hunk-level staging.
//!
//! # Model
//!
//! The FileDiff viewer for uncommitted changes shows the *combined*
//! `git diff HEAD` diff (`diff_tree_to_workdir_with_index`, see
//! [`crate::git::FileDiffContent::from_working_tree`]) — i.e. the net
//! difference between the HEAD tree and the working tree, taking the index into
//! account. A hunk in that view is therefore a change relative to HEAD,
//! regardless of whether it is currently staged.
//!
//! Hunk operations synthesise a minimal single-hunk unified diff for that hunk
//! and hand it to `git apply`:
//!
//! - **Stage hunk**  → `git apply --cached`      (apply the hunk to the index)
//! - **Unstage hunk**→ `git apply --cached -R`   (reverse-apply from the index)
//! - **Discard hunk**→ `git apply -R`            (reverse-apply in the worktree)
//!
//! Because the viewer shows the HEAD→worktree diff, the *direction* is chosen
//! explicitly by the user's key press, not inferred from the hunk. `git apply`
//! validates the patch against the target (index or worktree) and fails loudly
//! if the hunk does not apply cleanly (e.g. staging a hunk whose surrounding
//! region is already partially staged such that the index no longer matches
//! HEAD there), rather than silently corrupting state. In the common case —
//! a file with only unstaged changes, so index == HEAD — every direction
//! applies cleanly.
//!
//! The patch text is generated faithfully from libgit2's raw line bytes
//! (see [`extract_hunk_from_working_tree`]): each line's content already
//! carries its exact trailing newline (`\n` / `\r\n`, or none for the last
//! line of a file), so CRLF endings pass through unchanged and the
//! `\ No newline at end of file` marker is derived from a line whose content
//! lacks a trailing `\n`.

use std::path::Path;

use anyhow::Result;
use git2::{DiffLineType, DiffOptions, ErrorCode, Patch, Repository};

/// The role of a body line within a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchLineKind {
    Context,
    Addition,
    Deletion,
}

/// A single body line of a hunk.
///
/// `content` is the raw line text as it must appear *after* the leading
/// unified-diff marker, **including** any trailing newline (`\n` or `\r\n`).
/// A `content` that does not end in `\n` marks the final line of its side of
/// the file and triggers a `\ No newline at end of file` marker in the output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchLine {
    pub kind: PatchLineKind,
    pub content: String,
}

/// A single hunk to be rendered as a standalone one-hunk unified-diff patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkPatch {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<PatchLine>,
}

/// Render a minimal single-hunk unified-diff patch for `path`.
///
/// Pure function: the output depends only on the arguments. The resulting text
/// is accepted by `git apply` with the default `-p1` prefix stripping. Path
/// separators are normalised to `/` so patches apply on every platform.
pub fn render_hunk_patch(path: &str, hunk: &HunkPatch) -> String {
    let path = path.replace('\\', "/");
    let mut out = String::new();
    out.push_str("--- a/");
    out.push_str(&path);
    out.push('\n');
    out.push_str("+++ b/");
    out.push_str(&path);
    out.push('\n');
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
    ));
    for line in &hunk.lines {
        let marker = match line.kind {
            PatchLineKind::Context => ' ',
            PatchLineKind::Addition => '+',
            PatchLineKind::Deletion => '-',
        };
        out.push(marker);
        out.push_str(&line.content);
        if !line.content.ends_with('\n') {
            // Final line of this side of the file has no trailing newline.
            out.push_str("\n\\ No newline at end of file\n");
        }
    }
    out
}

/// Extract the `hunk_index`-th hunk of `file_path` from the combined
/// HEAD→worktree diff (the exact diff the FileDiff viewer renders), preserving
/// raw line bytes so the synthesised patch is byte-faithful.
///
/// Returns `Ok(None)` when the file has no textual diff or `hunk_index` is out
/// of range (e.g. the hunk was already applied/removed by a concurrent edit).
pub fn extract_hunk_from_working_tree(
    repo: &Repository,
    file_path: &Path,
    hunk_index: usize,
) -> Result<Option<HunkPatch>> {
    let head_tree = match repo.head() {
        Ok(head) => Some(head.peel_to_tree()?),
        Err(err)
            if err.code() == ErrorCode::UnbornBranch || err.code() == ErrorCode::NotFound =>
        {
            None
        }
        Err(err) => return Err(err.into()),
    };

    // Must mirror `FileDiffContent::from_working_tree` so hunk indices line up
    // with what the viewer displays.
    let mut opts = DiffOptions::new();
    opts.ignore_submodules(true);
    opts.context_lines(3);
    opts.pathspec(file_path);
    opts.disable_pathspec_match(true);
    opts.include_untracked(true);
    opts.recurse_untracked_dirs(true);
    opts.show_untracked_content(true);

    let diff = repo.diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))?;
    if diff.deltas().len() == 0 {
        return Ok(None);
    }
    let Some(patch) = Patch::from_diff(&diff, 0)? else {
        return Ok(None);
    };
    if hunk_index >= patch.num_hunks() {
        return Ok(None);
    }

    let (hunk, _) = patch.hunk(hunk_index)?;
    let num_lines = patch.num_lines_in_hunk(hunk_index)?;
    let mut lines = Vec::with_capacity(num_lines);
    for line_idx in 0..num_lines {
        let line = patch.line_in_hunk(hunk_index, line_idx)?;
        let kind = match line.origin_value() {
            DiffLineType::Context => PatchLineKind::Context,
            DiffLineType::Addition => PatchLineKind::Addition,
            DiffLineType::Deletion => PatchLineKind::Deletion,
            // The EOFNL pseudo-lines carry no body of their own; the preceding
            // real line's missing trailing `\n` already encodes the condition.
            _ => continue,
        };
        let content = String::from_utf8_lossy(line.content()).into_owned();
        lines.push(PatchLine { kind, content });
    }

    Ok(Some(HunkPatch {
        old_start: hunk.old_start(),
        old_lines: hunk.old_lines(),
        new_start: hunk.new_start(),
        new_lines: hunk.new_lines(),
        lines,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(s: &str) -> PatchLine {
        PatchLine {
            kind: PatchLineKind::Context,
            content: s.to_string(),
        }
    }
    fn add(s: &str) -> PatchLine {
        PatchLine {
            kind: PatchLineKind::Addition,
            content: s.to_string(),
        }
    }
    fn del(s: &str) -> PatchLine {
        PatchLine {
            kind: PatchLineKind::Deletion,
            content: s.to_string(),
        }
    }

    #[test]
    fn renders_headers_and_body_for_a_simple_hunk() {
        let hunk = HunkPatch {
            old_start: 1,
            old_lines: 3,
            new_start: 1,
            new_lines: 3,
            lines: vec![ctx("l1\n"), del("l2\n"), add("CHG\n"), ctx("l3\n")],
        };
        let patch = render_hunk_patch("src/foo.rs", &hunk);
        assert_eq!(
            patch,
            "--- a/src/foo.rs\n\
             +++ b/src/foo.rs\n\
             @@ -1,3 +1,3 @@\n\
             \x20l1\n\
             -l2\n\
             +CHG\n\
             \x20l3\n"
        );
    }

    #[test]
    fn renders_a_hunk_taken_from_the_middle_of_a_file() {
        // A hunk whose start positions are well past line 1 (i.e. the second
        // hunk of a multi-hunk file) must carry those exact positions.
        let hunk = HunkPatch {
            old_start: 40,
            old_lines: 6,
            new_start: 42,
            new_lines: 7,
            lines: vec![
                ctx("a\n"),
                ctx("b\n"),
                ctx("c\n"),
                del("old\n"),
                add("new1\n"),
                add("new2\n"),
                ctx("d\n"),
                ctx("e\n"),
                ctx("f\n"),
            ],
        };
        let patch = render_hunk_patch("x.txt", &hunk);
        assert!(patch.contains("@@ -40,6 +42,7 @@\n"));
        assert!(patch.starts_with("--- a/x.txt\n+++ b/x.txt\n"));
    }

    #[test]
    fn renders_pure_addition_hunk_at_file_start() {
        // Inserting at the very top of a file: old side is empty (0 lines).
        let hunk = HunkPatch {
            old_start: 0,
            old_lines: 0,
            new_start: 1,
            new_lines: 2,
            lines: vec![add("first\n"), add("second\n")],
        };
        let patch = render_hunk_patch("a", &hunk);
        assert_eq!(
            patch,
            "--- a/a\n+++ b/a\n@@ -0,0 +1,2 @@\n+first\n+second\n"
        );
    }

    #[test]
    fn no_newline_on_both_sides_marks_the_context_line() {
        // Context line at EOF with no trailing newline (both sides).
        let hunk = HunkPatch {
            old_start: 1,
            old_lines: 3,
            new_start: 1,
            new_lines: 3,
            lines: vec![ctx("l1\n"), del("l2\n"), add("CHG\n"), ctx("l3")],
        };
        let patch = render_hunk_patch("f.txt", &hunk);
        assert!(patch.ends_with(" l3\n\\ No newline at end of file\n"));
    }

    #[test]
    fn no_newline_on_new_side_only() {
        // Old side had a trailing newline; new side does not (LF removed).
        let hunk = HunkPatch {
            old_start: 1,
            old_lines: 3,
            new_start: 1,
            new_lines: 3,
            lines: vec![ctx("a\n"), ctx("b\n"), del("c\n"), add("c")],
        };
        let patch = render_hunk_patch("f.txt", &hunk);
        assert_eq!(
            patch,
            "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n a\n b\n-c\n+c\n\\ No newline at end of file\n"
        );
    }

    #[test]
    fn no_newline_on_old_side_only() {
        // Old side lacked a trailing newline; new side adds one (LF added).
        let hunk = HunkPatch {
            old_start: 1,
            old_lines: 3,
            new_start: 1,
            new_lines: 3,
            lines: vec![ctx("x\n"), ctx("y\n"), del("z"), add("z\n")],
        };
        let patch = render_hunk_patch("f.txt", &hunk);
        assert_eq!(
            patch,
            "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n x\n y\n-z\n\\ No newline at end of file\n+z\n"
        );
    }

    #[test]
    fn crlf_line_endings_pass_through_unchanged() {
        let hunk = HunkPatch {
            old_start: 1,
            old_lines: 3,
            new_start: 1,
            new_lines: 3,
            lines: vec![ctx("p\r\n"), del("q\r\n"), add("Q\r\n"), ctx("r\r\n")],
        };
        let patch = render_hunk_patch("h.txt", &hunk);
        assert_eq!(
            patch,
            "--- a/h.txt\n+++ b/h.txt\n@@ -1,3 +1,3 @@\n p\r\n-q\r\n+Q\r\n r\r\n"
        );
        // No spurious no-newline markers for CRLF lines.
        assert!(!patch.contains("No newline"));
    }

    #[test]
    fn context_only_change_is_rendered_verbatim() {
        // Windows path separators are normalised to forward slashes.
        let hunk = HunkPatch {
            old_start: 5,
            old_lines: 2,
            new_start: 5,
            new_lines: 3,
            lines: vec![ctx("keep\n"), add("inserted\n"), ctx("tail\n")],
        };
        let patch = render_hunk_patch("dir\\sub\\file.rs", &hunk);
        assert!(patch.starts_with("--- a/dir/sub/file.rs\n+++ b/dir/sub/file.rs\n"));
        assert!(patch.contains(" keep\n+inserted\n tail\n"));
    }
}
