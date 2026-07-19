//! File diff view widget (full-screen)

use std::sync::LazyLock;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};
use similar::{ChangeTag, TextDiff};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style as SyntectStyle, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use unicode_width::UnicodeWidthChar;

use crate::git::{DiffLineContent, DiffLineOrigin, FileDiffContent};

use super::{render_placeholder_block, theme::Theme, MIN_WIDGET_HEIGHT, MIN_WIDGET_WIDTH};

/// Display width of the diff gutter produced by [`make_diff_line`]:
/// old-lineno (4) + space (1) + new-lineno (4) + space (1) + prefix (1).
/// Continuation rows of a soft-wrapped line pad this many blank columns so the
/// wrapped text stays aligned under the first row's content.
const GUTTER_COLS: usize = 11;

// Syntax highlighting resources (initialized once)
static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

// NOTE: The diff background colors and syntect theme name are now in Theme.
// These module-level constants are kept only as defaults for the standalone
// `make_diff_line` helper which doesn't take a Theme (called from
// `build_highlighted_lines` which does).

// --- Line grouping for word-level emphasis ---

enum LineGroup<'a> {
    Context(&'a DiffLineContent),
    Change {
        deletions: Vec<&'a DiffLineContent>,
        additions: Vec<&'a DiffLineContent>,
    },
    NoNewline,
}

fn group_diff_lines<'a>(lines: &'a [DiffLineContent]) -> Vec<LineGroup<'a>> {
    let mut groups: Vec<LineGroup<'a>> = Vec::new();
    let mut pending_dels: Vec<&'a DiffLineContent> = Vec::new();
    let mut pending_adds: Vec<&'a DiffLineContent> = Vec::new();

    let flush = |groups: &mut Vec<LineGroup<'a>>,
                 dels: &mut Vec<&'a DiffLineContent>,
                 adds: &mut Vec<&'a DiffLineContent>| {
        if !dels.is_empty() || !adds.is_empty() {
            groups.push(LineGroup::Change {
                deletions: std::mem::take(dels),
                additions: std::mem::take(adds),
            });
        }
    };

    for line in lines {
        match line.origin {
            DiffLineOrigin::Context | DiffLineOrigin::HunkHeader => {
                flush(&mut groups, &mut pending_dels, &mut pending_adds);
                groups.push(LineGroup::Context(line));
            }
            DiffLineOrigin::Deletion => {
                if !pending_adds.is_empty() {
                    flush(&mut groups, &mut pending_dels, &mut pending_adds);
                }
                pending_dels.push(line);
            }
            DiffLineOrigin::Addition => {
                pending_adds.push(line);
            }
            DiffLineOrigin::NoNewlineAtEof => {
                flush(&mut groups, &mut pending_dels, &mut pending_adds);
                groups.push(LineGroup::NoNewline);
            }
        }
    }
    flush(&mut groups, &mut pending_dels, &mut pending_adds);
    groups
}

// --- Word-level emphasis via `similar` ---

struct WordEmphasis {
    old_spans: Vec<Vec<(bool, String)>>,
    new_spans: Vec<Vec<(bool, String)>>,
}

fn compute_word_emphasis(
    deletions: &[&DiffLineContent],
    additions: &[&DiffLineContent],
) -> WordEmphasis {
    let mut old_spans = Vec::with_capacity(deletions.len());
    let mut new_spans = Vec::with_capacity(additions.len());

    let max_len = deletions.len().max(additions.len());

    for i in 0..max_len {
        match (deletions.get(i), additions.get(i)) {
            (Some(del), Some(add)) => {
                let diff = TextDiff::from_words(&del.content, &add.content);
                let mut old_s = Vec::new();
                let mut new_s = Vec::new();
                for change in diff.iter_all_changes() {
                    match change.tag() {
                        ChangeTag::Equal => {
                            let v = change.value().to_string();
                            old_s.push((false, v.clone()));
                            new_s.push((false, v));
                        }
                        ChangeTag::Delete => {
                            old_s.push((true, change.value().to_string()));
                        }
                        ChangeTag::Insert => {
                            new_s.push((true, change.value().to_string()));
                        }
                    }
                }
                old_spans.push(old_s);
                new_spans.push(new_s);
            }
            (Some(del), None) => {
                old_spans.push(vec![(false, del.content.clone())]);
            }
            (None, Some(add)) => {
                new_spans.push(vec![(false, add.content.clone())]);
            }
            (None, None) => {}
        }
    }

    WordEmphasis {
        old_spans,
        new_spans,
    }
}

// --- Syntax highlighting helpers ---

fn highlight_line_owned(hl: &mut HighlightLines, content: &str) -> Vec<(SyntectStyle, String)> {
    let line = format!("{}\n", content);
    match hl.highlight_line(&line, &SYNTAX_SET) {
        Ok(spans) => {
            let result: Vec<_> = spans
                .into_iter()
                .map(|(style, text)| (style, text.trim_end_matches('\n').to_string()))
                .filter(|(_, text)| !text.is_empty())
                .collect();
            // Keep at least one span so diff background color is applied to blank lines
            if result.is_empty() {
                vec![(SyntectStyle::default(), String::new())]
            } else {
                result
            }
        }
        Err(_) => vec![(SyntectStyle::default(), content.to_string())],
    }
}

/// Map syntect RGB to vivid terminal ANSI colors (like delta/bat's ansi theme).
/// This ensures colors are always vivid regardless of the syntect theme,
/// because the terminal's own color scheme defines the actual appearance.
fn syntect_fg(style: &SyntectStyle, use_dark: bool) -> Color {
    let (r, g, b) = (style.foreground.r, style.foreground.g, style.foreground.b);

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let sat = if max > 0 {
        (max - min) as f32 / max as f32
    } else {
        0.0
    };

    // Achromatic (grays / whites)
    if sat < 0.15 {
        return if use_dark {
            if max > 160 { Color::DarkGray } else { Color::Black }
        } else if max < 90 {
            Color::DarkGray
        } else {
            Color::White
        };
    }

    // Compute hue (0–360)
    let (rf, gf, bf) = (r as f32, g as f32, b as f32);
    let range = (max - min) as f32;
    let hue = if r == max {
        60.0 * ((gf - bf) / range).rem_euclid(6.0)
    } else if g == max {
        60.0 * ((bf - rf) / range + 2.0)
    } else {
        60.0 * ((rf - gf) / range + 4.0)
    };

    if use_dark {
        // Dark ANSI colors for light terminal backgrounds
        match hue as u16 {
            0..=20 | 346..=360 => Color::Red,
            21..=65 => Color::Rgb(160, 120, 0),  // dark yellow (ANSI yellow is often too bright)
            66..=155 => Color::Rgb(0, 130, 0),    // dark green
            156..=195 => Color::Rgb(0, 130, 130),  // dark cyan
            196..=265 => Color::Blue,
            _ => Color::Magenta,
        }
    } else {
        // Bright ANSI colors for dark terminal backgrounds
        match hue as u16 {
            0..=20 | 346..=360 => Color::LightRed,
            21..=65 => Color::LightYellow,
            66..=155 => Color::LightGreen,
            156..=195 => Color::LightCyan,
            196..=265 => Color::LightBlue,
            _ => Color::LightMagenta,
        }
    }
}

/// Convert syntax-highlighted spans to ratatui spans with optional background
fn syntax_to_ratatui(
    syn_spans: &[(SyntectStyle, String)],
    bg: Option<Color>,
    use_dark_fg: bool,
) -> Vec<Span<'static>> {
    syn_spans
        .iter()
        .map(|(style, text)| {
            let mut s = Style::default().fg(syntect_fg(style, use_dark_fg));
            if let Some(bg) = bg {
                s = s.bg(bg);
            }
            Span::styled(text.clone(), s)
        })
        .collect()
}

/// Merge syntax spans with word-level emphasis spans
fn merge_syntax_and_emphasis(
    syn_spans: &[(SyntectStyle, String)],
    emp_spans: &[(bool, String)],
    base_bg: Color,
    emph_bg: Color,
    use_dark_fg: bool,
) -> Vec<Span<'static>> {
    let mut result = Vec::new();

    let mut syn_idx = 0;
    let mut syn_off = 0usize;
    let mut emp_idx = 0;
    let mut emp_off = 0usize;

    loop {
        let syn = syn_spans.get(syn_idx);
        let emp = emp_spans.get(emp_idx);

        let (Some((syn_style, syn_text)), Some((emphasized, emp_text))) = (syn, emp) else {
            break;
        };

        let syn_rem = syn_text.len() - syn_off;
        let emp_rem = emp_text.len() - emp_off;

        if syn_rem == 0 {
            syn_idx += 1;
            syn_off = 0;
            continue;
        }
        if emp_rem == 0 {
            emp_idx += 1;
            emp_off = 0;
            continue;
        }

        let chunk_len = syn_rem.min(emp_rem);
        // Ensure we don't slice in the middle of a multi-byte UTF-8 character.
        // Both span lists cover the same text, but syntect and similar may
        // split at different token boundaries.
        let chunk_len = {
            let syn_remaining = &syn_text[syn_off..];
            let emp_remaining = &emp_text[emp_off..];
            let mut len = chunk_len;
            while len > 0
                && (!syn_remaining.is_char_boundary(len) || !emp_remaining.is_char_boundary(len))
            {
                len -= 1;
            }
            len
        };
        if chunk_len == 0 {
            // Cannot find a common char boundary; skip both spans to avoid infinite loop
            syn_idx += 1;
            syn_off = 0;
            emp_idx += 1;
            emp_off = 0;
            continue;
        }
        let text = &syn_text[syn_off..syn_off + chunk_len];
        let fg = syntect_fg(syn_style, use_dark_fg);
        let bg = if *emphasized { emph_bg } else { base_bg };

        result.push(Span::styled(
            text.to_string(),
            Style::default().fg(fg).bg(bg),
        ));

        syn_off += chunk_len;
        emp_off += chunk_len;

        if syn_off >= syn_text.len() {
            syn_idx += 1;
            syn_off = 0;
        }
        if emp_off >= emp_text.len() {
            emp_idx += 1;
            emp_off = 0;
        }
    }

    // Remaining syntax spans (no emphasis data)
    while let Some((syn_style, syn_text)) = syn_spans.get(syn_idx) {
        let text = &syn_text[syn_off..];
        if !text.is_empty() {
            result.push(Span::styled(
                text.to_string(),
                Style::default().fg(syntect_fg(syn_style, use_dark_fg)).bg(base_bg),
            ));
        }
        syn_idx += 1;
        syn_off = 0;
    }

    result
}

// --- Line rendering helpers ---

/// A single logical diff row, kept with its gutter and content spans separate so
/// it can be soft-wrapped on demand: the gutter (line numbers + change prefix)
/// renders only on the first display row, while continuation rows pad the gutter
/// width and carry the remaining content (with its add/delete backgrounds).
///
/// Rows without a gutter (hunk headers, blank separators, status messages) have
/// an empty `gutter` and `gutter_cols == 0`.
#[derive(Debug, Clone)]
pub struct DiffRow {
    gutter: Vec<Span<'static>>,
    content: Vec<Span<'static>>,
    gutter_cols: usize,
}

/// Unwrapped source rows for the active file-diff viewer, held on `App` beside
/// the `FileDiff` mode (like `diff_viewport_*`) rather than inside the AppMode
/// enum, so soft-wrapping's re-layout inputs don't bloat the enum. Retained so
/// the viewer can re-lay-out `rendered_lines` when the wrap toggle or the pane
/// width changes.
#[derive(Debug, Clone, Default)]
pub struct DiffSource {
    pub rows: Vec<DiffRow>,
    /// Hunk-header positions into `rows` (wrap-independent).
    pub hunk_positions: Vec<usize>,
    /// (wrap enabled, content width) the mode's `rendered_lines` were last laid
    /// out for. `ensure_diff_layout` re-wraps when either drifts.
    pub layout_wrap: bool,
    pub layout_width: usize,
}

impl DiffRow {
    /// A gutter-less row (hunk header, blank separator, status message).
    fn plain(spans: Vec<Span<'static>>) -> Self {
        Self {
            gutter: Vec::new(),
            content: spans,
            gutter_cols: 0,
        }
    }

    /// The unwrapped, single-line rendering (gutter followed by content).
    fn to_line(&self) -> Line<'static> {
        let mut spans = self.gutter.clone();
        spans.extend(self.content.iter().cloned());
        Line::from(spans)
    }

    /// Soft-wrap this row to `avail_width` display columns, returning one display
    /// line per wrapped row. Word boundaries are preferred; overlong tokens are
    /// hard-broken (see [`wrap_offsets`]). The gutter appears only on the first
    /// row; continuation rows are padded to `gutter_cols` so content stays
    /// aligned, and each content span keeps its syntax/diff-background style.
    fn wrap(&self, avail_width: usize) -> Vec<Line<'static>> {
        let content_width = avail_width.saturating_sub(self.gutter_cols);
        // Degenerate widths can't wrap meaningfully — fall back to one line.
        if content_width == 0 {
            return vec![self.to_line()];
        }

        let text: String = self.content.iter().map(|s| s.content.as_ref()).collect();
        let offsets = wrap_offsets(&text, content_width);

        offsets
            .iter()
            .enumerate()
            .map(|(row, &start)| {
                let end = offsets.get(row + 1).copied().unwrap_or(text.len());
                let mut spans = if row == 0 {
                    self.gutter.clone()
                } else if self.gutter_cols > 0 {
                    vec![Span::raw(" ".repeat(self.gutter_cols))]
                } else {
                    Vec::new()
                };
                spans.extend(spans_in_byte_range(&self.content, start, end));
                Line::from(spans)
            })
            .collect()
    }
}

/// Char-index (byte offset) start of each display row when word-wrapping `text`
/// to `width` columns. The first offset is always `0`; the number of offsets is
/// the wrapped row count. Breaks are taken at whitespace where possible; a token
/// longer than `width` is hard-broken at the column limit. A `width` of 0 yields
/// a single row (nothing to wrap into).
pub fn wrap_offsets(text: &str, width: usize) -> Vec<usize> {
    let mut offsets = vec![0usize];
    if width == 0 || text.is_empty() {
        return offsets;
    }

    let chars: Vec<(usize, char, usize)> = text
        .char_indices()
        .map(|(b, c)| (b, c, UnicodeWidthChar::width(c).unwrap_or(0)))
        .collect();

    let mut row_start = 0usize; // index into `chars` where the current row began
    let mut cur_width = 0usize;
    let mut last_break: Option<usize> = None; // char index just after a whitespace
    let mut i = 0usize;
    while i < chars.len() {
        let (_, ch, w) = chars[i];
        if cur_width + w > width && i > row_start {
            // Prefer the most recent whitespace break within this row; otherwise
            // hard-break before the current char.
            let break_idx = match last_break {
                Some(b) if b > row_start => b,
                _ => i,
            };
            offsets.push(chars[break_idx].0);
            row_start = break_idx;
            cur_width = 0;
            last_break = None;
            i = break_idx;
            continue;
        }
        cur_width += w;
        if ch.is_whitespace() {
            last_break = Some(i + 1);
        }
        i += 1;
    }
    offsets
}

/// Slice `spans` to the concatenated-text byte range `[start, end)`, preserving
/// each source span's style. `start`/`end` must fall on char boundaries (they
/// come from [`wrap_offsets`], which only ever splits at `char_indices`).
fn spans_in_byte_range(
    spans: &[Span<'static>],
    start: usize,
    end: usize,
) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    for sp in spans {
        let s = sp.content.as_ref();
        let sp_start = pos;
        let sp_end = pos + s.len();
        pos = sp_end;
        let a = start.max(sp_start);
        let b = end.min(sp_end);
        if a < b {
            out.push(Span::styled(s[a - sp_start..b - sp_start].to_string(), sp.style));
        }
    }
    out
}

/// Prefix-sum the per-source-row display-row `counts` into the display-row index
/// at which each source row begins. `starts[i]` is where source row `i` starts;
/// used to translate hunk-header source positions into wrapped-row positions.
pub fn source_row_starts(counts: &[usize]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(counts.len());
    let mut acc = 0usize;
    for &c in counts {
        starts.push(acc);
        acc += c;
    }
    starts
}

/// Lay out source diff rows into the display lines the viewer renders.
///
/// When `wrap` is false this is a 1:1 mapping (hunk positions unchanged). When
/// `wrap` is true each row is soft-wrapped to `avail_width` and the hunk-header
/// positions are re-mapped into wrapped-row space so scrolling, the scrollbar,
/// and hunk navigation/staging all operate on the same coordinate system.
pub fn layout_diff_rows(
    rows: &[DiffRow],
    hunk_src_positions: &[usize],
    wrap: bool,
    avail_width: usize,
) -> (Vec<Line<'static>>, Vec<usize>) {
    if !wrap {
        return (
            rows.iter().map(DiffRow::to_line).collect(),
            hunk_src_positions.to_vec(),
        );
    }

    let mut lines = Vec::new();
    let mut counts = Vec::with_capacity(rows.len());
    for row in rows {
        let wrapped = row.wrap(avail_width);
        counts.push(wrapped.len());
        lines.extend(wrapped);
    }
    let starts = source_row_starts(&counts);
    let hunk_positions = hunk_src_positions
        .iter()
        .map(|&p| starts.get(p).copied().unwrap_or(0))
        .collect();
    (lines, hunk_positions)
}

/// A diff row for a git conflict-marker line: the marker text as a single
/// error-bar span, keeping the normal gutter (line numbers + change prefix) so
/// it aligns with surrounding rows.
fn make_conflict_marker_line(dl: &DiffLineContent, theme: &Theme) -> DiffRow {
    let spans = vec![Span::styled(dl.content.clone(), theme.conflict_marker_style())];
    make_diff_line(dl, spans, theme)
}

fn make_diff_line(dl: &DiffLineContent, content_spans: Vec<Span<'static>>, theme: &Theme) -> DiffRow {
    let lineno_style = Style::default().fg(theme.text_muted);

    let old_no = dl
        .old_lineno
        .map(|n| format!("{:>4}", n))
        .unwrap_or_else(|| "    ".to_string());
    let new_no = dl
        .new_lineno
        .map(|n| format!("{:>4}", n))
        .unwrap_or_else(|| "    ".to_string());

    let prefix = match dl.origin {
        DiffLineOrigin::Addition => "+",
        DiffLineOrigin::Deletion => "-",
        _ => " ",
    };
    let prefix_style = match dl.origin {
        DiffLineOrigin::Addition => Style::default().fg(theme.file_added).bg(theme.diff_add_bg),
        DiffLineOrigin::Deletion => Style::default().fg(theme.file_deleted).bg(theme.diff_del_bg),
        _ => Style::default(),
    };

    let gutter = vec![
        Span::styled(old_no, lineno_style),
        Span::styled(" ", lineno_style),
        Span::styled(new_no, lineno_style),
        Span::raw(" "),
        Span::styled(prefix.to_string(), prefix_style),
    ];

    DiffRow {
        gutter,
        content: content_spans,
        gutter_cols: GUTTER_COLS,
    }
}

fn determine_syntax(path: &std::path::Path) -> &'static SyntaxReference {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    // Map file types that syntect doesn't know about to similar languages
    let mapped_ext = match ext {
        "svelte" | "vue" | "astro" => "html",
        "tsx" => "jsx",
        "mts" | "cts" => "ts",
        "mjs" | "cjs" => "js",
        _ => ext,
    };

    // Try full filename first (handles Dockerfile, Makefile, etc.), then mapped extension, then original
    SYNTAX_SET
        .find_syntax_by_extension(file_name)
        .or_else(|| SYNTAX_SET.find_syntax_by_extension(mapped_ext))
        .or_else(|| SYNTAX_SET.find_syntax_by_extension(ext))
        .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text())
}

// --- Public: pre-compute highlighted lines (called once on mode entry) ---

/// Build the pre-computed source diff rows and hunk-header positions.
///
/// Returns `(rows, hunk_positions)` where each [`DiffRow`] is one logical diff
/// line (gutter + content, still separable for soft-wrapping) and
/// `hunk_positions` indexes the rows at which each hunk header sits. The caller
/// lays these out into display lines via [`layout_diff_rows`] (optionally
/// wrapping), keeping hunk navigation in sync with the rendered output.
///
/// Any diff line that is a git conflict marker (line-initial, exactly seven
/// marker chars) is rendered with [`Theme::conflict_marker_style`] so a diff
/// that surfaces marker text — e.g. a commit that left `<<<<<<<`/`=======`/
/// `>>>>>>>` lines in a file — reads as a conflict at a glance.
pub fn build_highlighted_lines(content: &FileDiffContent, ui_theme: &Theme) -> (Vec<DiffRow>, Vec<usize>) {
    if content.is_binary {
        return (
            vec![DiffRow::plain(vec![Span::styled(
                "(Binary file - no diff available)",
                Style::default().fg(ui_theme.text_muted),
            )])],
            Vec::new(),
        );
    }

    if content.hunks.is_empty() {
        return (
            vec![DiffRow::plain(vec![Span::styled(
                "(No textual changes)",
                Style::default().fg(ui_theme.text_muted),
            )])],
            Vec::new(),
        );
    }

    let syntax = determine_syntax(&content.path);
    let syntect_theme = THEME_SET
        .themes
        .get(ui_theme.syntect_theme)
        .or_else(|| THEME_SET.themes.values().next())
        .expect("syntect must have at least one built-in theme");
    let mut rows: Vec<DiffRow> = Vec::new();
    let mut hunk_positions = Vec::new();

    // Maintain highlight state across hunks so multi-line constructs
    // (block comments, strings, etc.) that span hunk boundaries are
    // colored correctly.
    let mut old_hl = HighlightLines::new(syntax, syntect_theme);
    let mut new_hl = HighlightLines::new(syntax, syntect_theme);

    for hunk in &content.hunks {
        // Blank line before each hunk header for readability
        rows.push(DiffRow::plain(Vec::new()));

        hunk_positions.push(rows.len());
        rows.push(DiffRow::plain(vec![Span::styled(
            hunk.header.clone(),
            Style::default()
                .fg(ui_theme.diff_hunk_fg)
                .bg(ui_theme.diff_hunk_bg)
                .add_modifier(Modifier::BOLD),
        )]));

        let groups = group_diff_lines(&hunk.lines);

        for group in &groups {
            match group {
                LineGroup::Context(dl) => {
                    // Advance both highlight states even for marker lines so
                    // multi-line constructs stay consistent across the marker.
                    let syn = highlight_line_owned(&mut old_hl, &dl.content);
                    let _ = highlight_line_owned(&mut new_hl, &dl.content);
                    let dark_fg = ui_theme.syntax_use_dark_colors;
                    let row = if crate::conflict::is_conflict_marker(&dl.content) {
                        make_conflict_marker_line(dl, ui_theme)
                    } else {
                        make_diff_line(dl, syntax_to_ratatui(&syn, None, dark_fg), ui_theme)
                    };
                    rows.push(row);
                }
                LineGroup::Change {
                    deletions,
                    additions,
                } => {
                    let emp = compute_word_emphasis(deletions, additions);
                    let dark_fg = ui_theme.syntax_use_dark_colors;

                    for (i, dl) in deletions.iter().enumerate() {
                        let syn = highlight_line_owned(&mut old_hl, &dl.content);
                        let row = if crate::conflict::is_conflict_marker(&dl.content) {
                            make_conflict_marker_line(dl, ui_theme)
                        } else if let Some(emp_spans) = emp.old_spans.get(i) {
                            make_diff_line(dl, merge_syntax_and_emphasis(&syn, emp_spans, ui_theme.diff_del_bg, ui_theme.diff_del_emph_bg, dark_fg), ui_theme)
                        } else {
                            make_diff_line(dl, syntax_to_ratatui(&syn, Some(ui_theme.diff_del_bg), dark_fg), ui_theme)
                        };
                        rows.push(row);
                    }

                    for (i, dl) in additions.iter().enumerate() {
                        let syn = highlight_line_owned(&mut new_hl, &dl.content);
                        let row = if crate::conflict::is_conflict_marker(&dl.content) {
                            make_conflict_marker_line(dl, ui_theme)
                        } else if let Some(emp_spans) = emp.new_spans.get(i) {
                            make_diff_line(dl, merge_syntax_and_emphasis(&syn, emp_spans, ui_theme.diff_add_bg, ui_theme.diff_add_emph_bg, dark_fg), ui_theme)
                        } else {
                            make_diff_line(dl, syntax_to_ratatui(&syn, Some(ui_theme.diff_add_bg), dark_fg), ui_theme)
                        };
                        rows.push(row);
                    }
                }
                LineGroup::NoNewline => {
                    rows.push(DiffRow::plain(vec![Span::styled(
                        "\\ No newline at end of file",
                        Style::default().fg(ui_theme.text_muted),
                    )]));
                }
            }
        }
    }

    (rows, hunk_positions)
}

// --- Widget (renders pre-computed lines) ---

pub struct FileDiffViewWidget<'a> {
    content: &'a FileDiffContent,
    rendered_lines: &'a [Line<'static>],
    scroll_offset: usize,
    horizontal_offset: usize,
    file_position: String,
    theme: &'a Theme,
}

impl<'a> FileDiffViewWidget<'a> {
    pub fn new(
        content: &'a FileDiffContent,
        rendered_lines: &'a [Line<'static>],
        scroll_offset: usize,
        horizontal_offset: usize,
        file_index: usize,
        file_count: usize,
        theme: &'a Theme,
    ) -> Self {
        Self {
            content,
            rendered_lines,
            scroll_offset,
            horizontal_offset,
            file_position: format!("[{}/{}]", file_index + 1, file_count),
            theme,
        }
    }
}

impl<'a> Widget for FileDiffViewWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < MIN_WIDGET_WIDTH || area.height < MIN_WIDGET_HEIGHT {
            render_placeholder_block(area, buf, self.theme);
            return;
        }

        let (indicator, _) = self.theme.file_change_style(&self.content.kind);

        let path_str = self.content.path.to_string_lossy();
        let stats = if self.content.is_binary {
            "(binary)".to_string()
        } else {
            format!(
                "+{} -{}",
                self.content.total_additions, self.content.total_deletions
            )
        };

        let title = format!(
            " [{}] {}  {}  {} ",
            indicator, path_str, stats, self.file_position
        );

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_type(self.theme.border_type())
            .border_style(Style::default().fg(self.theme.popup_border))
            .title_style(
                Style::default()
                    .fg(self.theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            );

        // Pre-slice lines to avoid u16 overflow in Paragraph::scroll()
        // for diffs longer than 65535 lines.
        let visible_height = area.height.saturating_sub(2) as usize; // minus block borders
        let start = self
            .scroll_offset
            .min(self.rendered_lines.len().saturating_sub(1));
        let end = (start + visible_height + 1).min(self.rendered_lines.len());
        let visible_lines = &self.rendered_lines[start..end];

        let h_offset = self.horizontal_offset.min(u16::MAX as usize) as u16;
        let paragraph = Paragraph::new(visible_lines.to_vec())
            .block(block)
            .scroll((0, h_offset));

        Widget::render(paragraph, area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reconstruct a display line's visible text (all span contents joined).
    fn line_text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn wrap_offsets_breaks_at_word_boundaries() {
        // "hello world foo" at width 8 wraps as "hello " | "world " | "foo",
        // breaking after the whitespace rather than mid-word.
        let offsets = wrap_offsets("hello world foo", 8);
        assert_eq!(offsets, vec![0, 6, 12]);
        assert_eq!(offsets.len(), 3, "three wrapped rows");
    }

    #[test]
    fn wrap_offsets_hard_breaks_an_overlong_token() {
        // A single token longer than the width has no whitespace to break at,
        // so it is hard-broken every `width` columns.
        let offsets = wrap_offsets("abcdefghij", 4);
        assert_eq!(offsets, vec![0, 4, 8]);
        assert_eq!(offsets.len(), 3);
    }

    #[test]
    fn wrap_offsets_leaves_short_lines_untouched() {
        assert_eq!(wrap_offsets("short", 20), vec![0]);
        // Exactly filling the width does not force a second row.
        assert_eq!(wrap_offsets("abcd", 4), vec![0]);
    }

    #[test]
    fn wrap_offsets_degenerate_inputs_yield_single_row() {
        assert_eq!(wrap_offsets("anything", 0), vec![0]);
        assert_eq!(wrap_offsets("", 8), vec![0]);
    }

    #[test]
    fn wrap_offsets_row_count_matches_a_worked_example() {
        // Mixed words and a break opportunity: "the quick brown" at width 6.
        // "the " (4) + "quick " overflows -> break after "the "; "quick " (6) +
        // "brown" overflows -> break; "brown" fits. Three rows.
        let offsets = wrap_offsets("the quick brown", 6);
        assert_eq!(offsets.len(), 3);
        assert_eq!(offsets, vec![0, 4, 10]);
    }

    #[test]
    fn source_row_starts_is_a_prefix_sum() {
        assert_eq!(source_row_starts(&[1, 3, 1, 2]), vec![0, 1, 4, 5]);
        assert_eq!(source_row_starts(&[]), Vec::<usize>::new());
        assert_eq!(source_row_starts(&[1, 1, 1]), vec![0, 1, 2]);
    }

    #[test]
    fn layout_without_wrap_is_a_one_to_one_mapping() {
        let rows = vec![
            DiffRow::plain(vec![Span::raw("aa")]),
            DiffRow::plain(vec![Span::raw("this is a long header")]),
            DiffRow::plain(vec![Span::raw("bb")]),
        ];
        let (lines, hunks) = layout_diff_rows(&rows, &[1], false, 4);
        assert_eq!(lines.len(), 3, "no wrapping: one display line per row");
        assert_eq!(hunks, vec![1], "hunk positions pass through unchanged");
    }

    #[test]
    fn layout_with_wrap_remaps_hunk_positions_into_wrapped_space() {
        // Row 1 ("abcdefgh" at width 4) wraps into two display rows, pushing the
        // hunk header at source row 2 down to wrapped-row index 3.
        let rows = vec![
            DiffRow::plain(vec![Span::raw("xx")]),       // 1 display row
            DiffRow::plain(vec![Span::raw("abcdefgh")]), // 2 display rows at width 4
            DiffRow::plain(vec![Span::raw("yy")]),       // 1 display row (hunk header)
        ];
        let (lines, hunks) = layout_diff_rows(&rows, &[2], true, 4);
        assert_eq!(lines.len(), 4, "one row wraps into two -> four display rows");
        assert_eq!(hunks, vec![3], "hunk header remapped to wrapped-row index");
    }

    #[test]
    fn wrapping_keeps_the_gutter_on_the_first_row_only() {
        // gutter "GUT" (3 cols) + content that wraps; avail width leaves 8 cols
        // for content, so "hello world foo" wraps into three rows.
        let row = DiffRow {
            gutter: vec![Span::raw("GUT")],
            content: vec![Span::raw("hello world foo")],
            gutter_cols: 3,
        };
        let lines = row.wrap(11);
        assert_eq!(lines.len(), 3);

        // First row carries the real gutter; continuation rows pad it with spaces.
        assert_eq!(lines[0].spans[0].content.as_ref(), "GUT");
        assert_eq!(lines[1].spans[0].content.as_ref(), "   ");
        assert_eq!(lines[2].spans[0].content.as_ref(), "   ");

        // Concatenating each row's content (dropping the 3-col gutter) rebuilds
        // the original text exactly — no characters lost or duplicated.
        let reassembled: String = lines
            .iter()
            .map(|l| {
                let full = line_text(l);
                full[3..].to_string()
            })
            .collect();
        assert_eq!(reassembled, "hello world foo");
    }

    #[test]
    fn wrapping_preserves_per_span_styles_across_continuation_rows() {
        // Two styled content spans; after wrapping, each display row's content
        // must keep the original style of whatever span it came from.
        let red = Style::default().fg(Color::Red);
        let blue = Style::default().fg(Color::Blue);
        let row = DiffRow {
            gutter: Vec::new(),
            content: vec![
                Span::styled("aaaa", red),
                Span::styled("bbbb", blue),
            ],
            gutter_cols: 0,
        };
        let lines = row.wrap(4);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].content.as_ref(), "aaaa");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Red));
        assert_eq!(lines[1].spans[0].content.as_ref(), "bbbb");
        assert_eq!(lines[1].spans[0].style.fg, Some(Color::Blue));
    }
}
