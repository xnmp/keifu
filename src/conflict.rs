//! Pure conflict-marker logic: detecting git conflict-marker lines and cycling
//! through conflict positions with wrap-around. Kept free of TUI/git types so it
//! can be unit-tested in isolation and reused by both the files pane (conflicted
//! file jumping) and the diff viewer (conflict-block jumping + highlighting).

/// The four characters git uses to open conflict-marker lines. Each marker line
/// is exactly seven of one of these, line-initial, followed by a space (before a
/// label) or the end of the line.
const MARKER_CHARS: [u8; 4] = [b'<', b'=', b'>', b'|'];

/// Length of a git conflict marker run (`<<<<<<<` etc. — always exactly seven).
const MARKER_LEN: usize = 7;

/// True if `line` is a git conflict-marker line.
///
/// Git writes conflict markers as exactly seven identical marker characters at
/// the very start of the line, optionally followed by a space and a label:
/// `<<<<<<< HEAD`, `||||||| base`, `=======`, `>>>>>>> branch`. This matches
/// that shape precisely:
///
/// - Line-initial only (a marker mid-line is not a real marker).
/// - Exactly seven marker chars — six (`<<<<<<`) or eight (`<<<<<<<<`) do not
///   count.
/// - The eighth column must be a space or the end of the line, so `======= foo`
///   and a bare `=======` both match but `=======x` does not.
///
/// A trailing `\r`/`\n` (if the caller hasn't stripped it) is ignored so the
/// bare separator `=======\n` still matches.
pub fn is_conflict_marker(line: &str) -> bool {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let bytes = line.as_bytes();
    if bytes.len() < MARKER_LEN {
        return false;
    }
    let marker = bytes[0];
    if !MARKER_CHARS.contains(&marker) {
        return false;
    }
    if !bytes[..MARKER_LEN].iter().all(|&b| b == marker) {
        return false;
    }
    match bytes.get(MARKER_LEN) {
        None => true,        // exactly seven chars, e.g. bare `=======`
        Some(&b' ') => true, // seven chars + labelled, e.g. `<<<<<<< HEAD`
        Some(_) => false,    // an eighth marker char or any other trailing byte
    }
}

/// Pick the next (or previous) position from `positions` relative to `current`,
/// wrapping around the ends. `positions` must be sorted ascending and hold no
/// duplicates (as produced by scanning items/rows in order).
///
/// - `forward`: the smallest position strictly greater than `current`, or the
///   first position if `current` is at or past the last one.
/// - `!forward`: the largest position strictly less than `current`, or the last
///   position if `current` is at or before the first one.
///
/// Returns `None` when there are no positions. With a single position it always
/// returns that position (a wrap onto itself), so "jump to next conflict" is a
/// harmless no-op move when only one conflict exists.
pub fn next_position(positions: &[usize], current: usize, forward: bool) -> Option<usize> {
    if positions.is_empty() {
        return None;
    }
    if forward {
        positions
            .iter()
            .copied()
            .find(|&p| p > current)
            .or_else(|| positions.first().copied())
    } else {
        positions
            .iter()
            .copied()
            .rev()
            .find(|&p| p < current)
            .or_else(|| positions.last().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── is_conflict_marker ──────────────────────────────────────────

    #[test]
    fn detects_all_four_labelled_markers() {
        assert!(is_conflict_marker("<<<<<<< HEAD"));
        assert!(is_conflict_marker("||||||| merged common ancestors"));
        assert!(is_conflict_marker(">>>>>>> feature-branch"));
        // The separator is conventionally bare, but a labelled one still counts.
        assert!(is_conflict_marker("======= whatever"));
    }

    #[test]
    fn detects_bare_separator_and_bare_markers() {
        assert!(is_conflict_marker("======="));
        assert!(is_conflict_marker("<<<<<<<"));
        assert!(is_conflict_marker(">>>>>>>"));
        assert!(is_conflict_marker("|||||||"));
    }

    #[test]
    fn ignores_trailing_line_endings() {
        assert!(is_conflict_marker("=======\n"));
        assert!(is_conflict_marker("=======\r\n"));
        assert!(is_conflict_marker("<<<<<<< HEAD\n"));
    }

    #[test]
    fn rejects_six_char_near_miss() {
        // Six markers is one short — not a git marker.
        assert!(!is_conflict_marker("<<<<<<"));
        assert!(!is_conflict_marker("======"));
        assert!(!is_conflict_marker(">>>>>>"));
    }

    #[test]
    fn rejects_eight_char_run() {
        // Eight identical marker chars: the eighth column is a marker char, not a
        // space/EOL, so it must be rejected.
        assert!(!is_conflict_marker("<<<<<<<<"));
        assert!(!is_conflict_marker("========"));
        assert!(!is_conflict_marker(">>>>>>>>"));
    }

    #[test]
    fn rejects_seven_chars_followed_by_non_space() {
        // Exactly seven but immediately followed by a non-space (no label space).
        assert!(!is_conflict_marker("=======x"));
        assert!(!is_conflict_marker("<<<<<<<HEAD"));
    }

    #[test]
    fn rejects_markers_that_are_not_line_initial() {
        // A marker embedded mid-line (e.g. inside code or a comment) is not a
        // real git conflict marker.
        assert!(!is_conflict_marker("  <<<<<<< HEAD"));
        assert!(!is_conflict_marker("let x = 1; // ======="));
        assert!(!is_conflict_marker("prefix >>>>>>> branch"));
    }

    #[test]
    fn rejects_mixed_and_empty_lines() {
        assert!(!is_conflict_marker("<<<<<=="));
        assert!(!is_conflict_marker(""));
        assert!(!is_conflict_marker("normal line of code"));
        assert!(!is_conflict_marker("<")); // far too short
    }

    // ─── next_position (cycling with wrap-around) ────────────────────

    #[test]
    fn next_position_none_when_empty() {
        assert_eq!(next_position(&[], 0, true), None);
        assert_eq!(next_position(&[], 5, false), None);
    }

    #[test]
    fn single_position_always_returns_itself() {
        // Whether we're before, on, or after it, and either direction.
        assert_eq!(next_position(&[3], 0, true), Some(3));
        assert_eq!(next_position(&[3], 3, true), Some(3));
        assert_eq!(next_position(&[3], 9, true), Some(3));
        assert_eq!(next_position(&[3], 3, false), Some(3));
        assert_eq!(next_position(&[3], 0, false), Some(3));
    }

    #[test]
    fn forward_finds_next_strictly_greater() {
        let p = [2, 5, 9];
        assert_eq!(next_position(&p, 0, true), Some(2));
        assert_eq!(next_position(&p, 2, true), Some(5));
        assert_eq!(next_position(&p, 4, true), Some(5));
        assert_eq!(next_position(&p, 5, true), Some(9));
    }

    #[test]
    fn forward_wraps_to_first_past_the_end() {
        let p = [2, 5, 9];
        assert_eq!(next_position(&p, 9, true), Some(2));
        assert_eq!(next_position(&p, 100, true), Some(2));
    }

    #[test]
    fn backward_finds_previous_strictly_less() {
        let p = [2, 5, 9];
        assert_eq!(next_position(&p, 9, false), Some(5));
        assert_eq!(next_position(&p, 6, false), Some(5));
        assert_eq!(next_position(&p, 5, false), Some(2));
    }

    #[test]
    fn backward_wraps_to_last_before_the_start() {
        let p = [2, 5, 9];
        assert_eq!(next_position(&p, 2, false), Some(9));
        assert_eq!(next_position(&p, 0, false), Some(9));
    }

    #[test]
    fn forward_then_backward_round_trips_between_neighbours() {
        let p = [1, 4, 8];
        let fwd = next_position(&p, 1, true).unwrap(); // -> 4
        assert_eq!(fwd, 4);
        assert_eq!(next_position(&p, fwd, false), Some(1)); // back to 1
    }
}
