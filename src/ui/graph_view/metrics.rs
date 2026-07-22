//! Text-metrics helpers: VS16-aware display width, width-bounded truncation,
//! and compact relative-date formatting for the graph view.

use chrono::{DateTime, Local};
use unicode_width::UnicodeWidthChar;

/// Fixed display width for the compact date field (fits "11mo", "now", "59m").
/// The full absolute date lives in the commit detail panel.
const DATE_FIELD_WIDTH: usize = 4;

/// Compact relative age of a commit: "now", "59m", "23h", "6d", "3w", "11mo",
/// "2y". Left-padded to `DATE_FIELD_WIDTH` so the column stays aligned.
/// `now` is passed in so it's computed once per render, not once per row.
pub(super) fn format_date_field(timestamp: DateTime<Local>, now: DateTime<Local>) -> String {
    let delta = now.signed_duration_since(timestamp);
    let secs = delta.num_seconds();
    let days = delta.num_days();

    let label = if secs < 60 {
        // Includes future timestamps (clock skew) — shown as "now".
        "now".to_string()
    } else if delta.num_minutes() < 60 {
        format!("{}m", delta.num_minutes())
    } else if delta.num_hours() < 24 {
        format!("{}h", delta.num_hours())
    } else if days < 7 {
        format!("{}d", days)
    } else if days < 30 {
        format!("{}w", days / 7)
    } else if days < 365 {
        format!("{}mo", days / 30)
    } else {
        format!("{}y", days / 365)
    };

    format!("{:<width$}", label, width = DATE_FIELD_WIDTH)
}

/// VS16 (U+FE0F) variation selector for emoji presentation
const VS16: char = '\u{FE0F}';

/// Calculate character width considering VS16 emoji presentation sequence.
/// If `next_char` is VS16, the character has emoji presentation width (2).
/// VS16 itself has no width.
fn char_width_with_vs16(c: char, next_char: Option<char>) -> usize {
    if next_char == Some(VS16) {
        2
    } else if c == VS16 {
        0
    } else {
        UnicodeWidthChar::width(c).unwrap_or(0)
    }
}

/// Calculate display width of a string.
/// Handles VS16 which changes preceding character to emoji presentation (width 2).
pub(crate) fn display_width(s: &str) -> usize {
    if s.is_ascii() {
        // No VS16 (non-ASCII) possible. Printable ASCII (0x20..=0x7E) is width
        // 1; control chars are width 0 per unicode-width, matching the
        // general-case per-char width function below.
        return s.bytes().filter(|b| (0x20..=0x7E).contains(b)).count();
    }
    let mut width = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let next_char = chars.peek().copied();
        width += char_width_with_vs16(c, next_char);
        // Skip next char if it was VS16 (already accounted for)
        if next_char == Some(VS16) {
            chars.next();
        }
    }
    width
}

/// Truncate a string to the specified display width.
/// Handles VS16 which changes preceding character to emoji presentation (width 2).
pub(super) fn truncate_to_width(s: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut current_width = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let next_char = chars.peek().copied();
        let ch_width = char_width_with_vs16(c, next_char);
        if current_width + ch_width > max_width {
            break;
        }
        result.push(c);
        current_width += ch_width;
        if next_char == Some(VS16) {
            result.push(VS16);
            chars.next();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `now` minus `secs_ago` seconds. Duration arithmetic on `DateTime<Tz>`
    /// operates on the underlying instant, so this is exact regardless of
    /// DST — safe to use with `Local::now()` as the anchor.
    fn ago(now: DateTime<Local>, secs_ago: i64) -> DateTime<Local> {
        now - chrono::Duration::seconds(secs_ago)
    }

    // ── format_date_field (compact relative age) ─────────────────────

    fn age(secs_ago: i64) -> String {
        let now = Local::now();
        format_date_field(ago(now, secs_ago), now).trim_end().to_string()
    }

    const MIN: i64 = 60;
    const HOUR: i64 = 3600;
    const DAY: i64 = 24 * HOUR;

    #[test]
    fn compact_date_covers_all_ranges() {
        assert_eq!(age(5), "now"); // just now
        assert_eq!(age(59), "now"); // still under a minute
        assert_eq!(age(60), "1m");
        assert_eq!(age(59 * MIN), "59m"); // 59 minutes
        assert_eq!(age(HOUR), "1h");
        assert_eq!(age(23 * HOUR), "23h"); // 23 hours
        assert_eq!(age(DAY), "1d");
        assert_eq!(age(6 * DAY), "6d");
        assert_eq!(age(7 * DAY), "1w"); // weeks start at 7d
        assert_eq!(age(21 * DAY), "3w"); // 3 weeks
        assert_eq!(age(30 * DAY), "1mo"); // months start at 30d
        assert_eq!(age(330 * DAY), "11mo"); // 11 months
        assert_eq!(age(365 * DAY), "1y"); // years start at 365d
        assert_eq!(age(2 * 365 * DAY), "2y");
    }

    #[test]
    fn future_timestamp_shows_now() {
        let now = Local::now();
        let future = now + chrono::Duration::seconds(300);
        assert_eq!(format_date_field(future, now).trim_end(), "now");
    }

    #[test]
    fn result_is_padded_to_fixed_width() {
        let now = Local::now();
        let recent = ago(now, 5);
        // "now" padded to DATE_FIELD_WIDTH; longer labels ("11mo") fill it exactly.
        assert_eq!(format_date_field(recent, now).len(), DATE_FIELD_WIDTH);
        let months = ago(now, 330 * DAY);
        assert_eq!(display_width(&format_date_field(months, now)), DATE_FIELD_WIDTH);
    }

    // ── display_width ────────────────────────────────────────────────

    #[test]
    fn ascii_string_width_equals_byte_len() {
        let s = "hello world";
        assert_eq!(display_width(s), s.len());
    }

    #[test]
    fn cjk_wide_chars_count_as_two() {
        assert_eq!(display_width("中文"), 4);
    }

    #[test]
    fn vs16_emoji_sequence_counts_as_two() {
        // U+2764 HEAVY BLACK HEART + U+FE0F VS16 => emoji presentation, width 2.
        let heart_emoji = "\u{2764}\u{FE0F}";
        assert_eq!(display_width(heart_emoji), 2);
    }

    #[test]
    fn combining_mark_contributes_no_width() {
        // 'e' + combining acute accent (U+0301): the accent is zero-width.
        let combining_e = "e\u{0301}";
        assert_eq!(display_width(combining_e), 1);
    }

    #[test]
    fn empty_string_has_zero_width() {
        assert_eq!(display_width(""), 0);
    }
}
