//! Graph view widget

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, StatefulWidget},
};
use chrono::{DateTime, Local};
use unicode_width::UnicodeWidthChar;

use std::collections::HashMap;

use crate::{
    app::App,
    git::graph::{CellType, GraphNode},
    pr::PrInfo,
};

/// Fixed display width for the date field (fits "59 minutes ago").
const DATE_FIELD_WIDTH: usize = 14;

/// Format a commit timestamp. Within the past week, show a relative string
/// ("just now", "5 minutes ago", "3 days ago"); otherwise the absolute date.
/// Result is right-padded to `DATE_FIELD_WIDTH` so columns stay aligned.
/// `now` is passed in so it's computed once per render, not once per row.
fn format_date_field(timestamp: DateTime<Local>, now: DateTime<Local>) -> String {
    let delta = now.signed_duration_since(timestamp);
    let secs = delta.num_seconds();

    // Future timestamps or older than a week fall back to the absolute date.
    let label = if secs < 0 || delta.num_days() >= 7 {
        timestamp.format("%Y-%m-%d").to_string()
    } else if secs < 60 {
        "just now".to_string()
    } else if delta.num_minutes() < 60 {
        let m = delta.num_minutes();
        format!("{} minute{} ago", m, if m == 1 { "" } else { "s" })
    } else if delta.num_hours() < 24 {
        let h = delta.num_hours();
        format!("{} hour{} ago", h, if h == 1 { "" } else { "s" })
    } else {
        let d = delta.num_days();
        format!("{} day{} ago", d, if d == 1 { "" } else { "s" })
    };

    format!("{:<width$}", label, width = DATE_FIELD_WIDTH)
}

use super::{render_placeholder_block, theme::Theme, MIN_WIDGET_HEIGHT, MIN_WIDGET_WIDTH};

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
fn display_width(s: &str) -> usize {
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

pub struct GraphViewWidget<'a> {
    items: Vec<ListItem<'a>>,
    selected_in_filtered: Option<usize>,
    is_focused: bool,
    title: String,
    theme: &'a Theme,
}

/// Number of leading blank columns before the graph glyphs on every row (the
/// start marker). Both the space-emitter in `render_graph_line` and the pixel
/// overlay in `ui::mod` key off this so the image lines up with the glyph slot.
pub const GRAPH_LEADING_COLUMNS: u16 = 1;

/// The nodes shown by the graph list, paired with their index into
/// `graph_layout.nodes`. When a commit filter is active only the matching
/// nodes are listed, in `visible_commit_indices` order. This is the single
/// source of the list's row ordering — used by the widget and by the pixel
/// pre-pass so their rows stay aligned.
pub fn visible_nodes(app: &App) -> Vec<(usize, &GraphNode)> {
    if app.commit_filter.is_empty() {
        app.graph_layout.nodes.iter().enumerate().collect()
    } else {
        app.visible_commit_indices
            .iter()
            .map(|&idx| (idx, &app.graph_layout.nodes[idx]))
            .collect()
    }
}

/// Build one `RowSpec` per list item (respecting the active commit filter), in
/// the same order the graph widget lists them, so the overlay can index into it
/// by visible-row position. Neighbours follow visible order.
///
/// `available_cells` caps each spec's cell count to the width the overlay will
/// actually draw: the iTerm2/Sixel fixed protocols render *nothing* when the
/// protocol is wider than its render area (only Kitty crops), so the cached
/// protocol's width must equal the rect the overlay uses. Truncating here keeps
/// them in lockstep; truncated specs still hash as themselves so caching holds.
pub fn build_pixel_row_specs(
    app: &App,
    theme: &Theme,
    available_cells: usize,
) -> Vec<crate::ui::graph_pixels::RowSpec> {
    use crate::ui::graph_pixels::build_row_spec;
    let nodes: Vec<&GraphNode> = visible_nodes(app).into_iter().map(|(_, n)| n).collect();
    (0..nodes.len())
        .map(|i| {
            let prev = i.checked_sub(1).map(|p| nodes[p]);
            let next = nodes.get(i + 1).copied();
            let mut spec = build_row_spec(prev, nodes[i], next, theme);
            spec.cells.truncate(available_cells);
            spec
        })
        .collect()
}

impl<'a> GraphViewWidget<'a> {
    pub fn new(app: &App, width: u16, theme: &'a Theme, pixel_mode: bool) -> Self {
        let max_lane = app.graph_layout.max_lane;
        let inner_width = width.saturating_sub(2) as usize;
        let selected_branch_name = app.selected_branch_name();
        let has_filter = !app.commit_filter.is_empty();
        let current_selected = app.graph_nav.graph_list_state.selected();
        let now = Local::now();
        let remotes = &app.remotes;
        let open_prs = &app.open_prs;

        let node_iter = visible_nodes(app);

        let mut selected_in_filtered = None;
        let mut items: Vec<ListItem> = Vec::new();

        for (filtered_pos, (full_idx, node)) in node_iter.into_iter().enumerate() {
            let is_selected = current_selected == Some(full_idx);
            if is_selected {
                selected_in_filtered = Some(filtered_pos);
            }
            // A node is "marked" when it's the pending compare mark or one of the
            // active comparison's two endpoints.
            let is_marked = node.commit.as_ref().is_some_and(|c| {
                app.compare_marked == Some(c.oid)
                    || app
                        .compare_range
                        .is_some_and(|(old, new)| old == c.oid || new == c.oid)
            });
            let line = render_graph_line(
                node,
                max_lane,
                is_selected,
                is_marked,
                inner_width,
                selected_branch_name,
                theme,
                now,
                pixel_mode,
                remotes,
                open_prs,
            );
            items.push(ListItem::new(line));
        }

        let title = if app.commit_filter_active {
            format!(" Commits: {}_ ", app.commit_filter)
        } else if has_filter {
            format!(" Commits [{}] ", app.commit_filter)
        } else {
            " Commits ".to_string()
        };

        Self {
            items,
            selected_in_filtered,
            is_focused: app.focused_panel == crate::app::FocusedPanel::Graph,
            title,
            theme,
        }
    }
}

/// Strip a real remote prefix (e.g. "origin/", "upstream/") from a ref,
/// returning the bare branch name. Local branches that merely contain a slash
/// ("feature/foo") are not remote refs.
fn strip_remote<'n>(name: &'n str, remotes: &[String]) -> Option<&'n str> {
    remotes.iter().find_map(|r| {
        name.strip_prefix(r.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
    })
}

/// Nerd Font pull-request glyph (nf-oct-git_pull_request) for the open-PR badge.
const PR_BADGE_ICON: char = '\u{f407}';

/// The first open PR (in branch-label order) whose head branch matches one of
/// this node's branch labels. Remote refs are matched by their stripped name
/// (handles non-origin remotes), so `origin/feat` and a local `feat` both match
/// a PR whose `headRefName` is `feat`.
pub fn pr_for_branch_labels<'p>(
    branch_names: &[String],
    remotes: &[String],
    open_prs: &'p HashMap<String, PrInfo>,
) -> Option<&'p PrInfo> {
    branch_names.iter().find_map(|name| {
        let bare = strip_remote(name, remotes).unwrap_or(name.as_str());
        open_prs.get(bare)
    })
}

/// Compact badge text for an open PR, e.g. " #12".
fn pr_badge_text(pr: &PrInfo) -> String {
    format!("{} #{}", PR_BADGE_ICON, pr.number)
}

/// Nerd Font cloud glyph marking a branch that only exists on a remote.
const REMOTE_ONLY_ICON: &str = "\u{f0c2}"; //
/// Marks a local branch whose remote counterpart points at the same commit.
const SYNCED_ICON: &str = "↔";

/// Optimize branch name display
/// - A lone remote ref gets a cloud icon; a local+remote pair collapses to one ↔ label
/// - If a local branch matches its origin/xxx among other branches, show "xxx <-> origin"
/// - Otherwise, show each name separately
/// - Render in bold with the graph color, wrapped in brackets
/// - Selected branch is shown with inverted colors
fn optimize_branch_display(
    branch_names: &[String],
    is_head: bool,
    color_index: usize,
    selected_branch_name: Option<&str>,
    theme: &Theme,
    remotes: &[String],
) -> Vec<(String, Style)> {
    use std::collections::HashSet;

    if branch_names.is_empty() {
        return Vec::new();
    }

    // Max width for a single branch label (e.g., "[fix/feature-name]")
    const MAX_LABEL_WIDTH: usize = 40;

    // Split local and remote branches by real remote prefix (HashSet for O(1)
    // lookup). A name is remote only when it starts with a configured remote
    // (e.g. "origin/", "upstream/"), never merely for containing a slash.
    let local_branches: HashSet<&str> = branch_names
        .iter()
        .filter(|n| strip_remote(n, remotes).is_none())
        .map(|s| s.as_str())
        .collect();

    // Determine base color: main branch stays blue; other HEADs are green
    let is_main_branch = color_index == crate::graph::colors::MAIN_BRANCH_COLOR;
    let base_color = if is_head && !is_main_branch {
        theme.branch_head
    } else {
        theme.lane_color(color_index)
    };

    // Helper to create style based on selection state
    let make_style = |branch_name: &str| -> Style {
        let style = Style::default().fg(base_color).add_modifier(Modifier::BOLD);
        if selected_branch_name == Some(branch_name) {
            // Reverse video rather than an explicit bg: when this branch's row
            // is also the highlighted row, the list's highlight_style patches a
            // bg over the whole line, which would clobber an explicit bg and
            // leave the label invisible. REVERSED is resolved after that patch,
            // so the selected branch stays legible on any terminal theme.
            style.add_modifier(Modifier::REVERSED)
        } else {
            style
        }
    };

    // Helper to create label with optional icon prefix and abbreviation
    let make_label = |prefix: &str, name: &str, suffix: Option<&str>| -> String {
        let prefix_width = display_width(prefix);
        let (label, abbrev_width) = if let Some(s) = suffix {
            (
                format!("[{}{} {}]", prefix, name, s),
                MAX_LABEL_WIDTH.saturating_sub(s.len() + 3 + prefix_width),
            )
        } else {
            (
                format!("[{}{}]", prefix, name),
                MAX_LABEL_WIDTH.saturating_sub(prefix_width),
            )
        };

        if display_width(&label) <= MAX_LABEL_WIDTH {
            return label;
        }

        let abbrev = abbreviate_branch_label(name, abbrev_width, 0);
        let abbrev = if prefix.is_empty() {
            abbrev
        } else {
            abbrev.replacen('[', &format!("[{}", prefix), 1)
        };
        if let Some(s) = suffix {
            abbrev.replace(']', &format!(" {}]", s))
        } else {
            abbrev
        }
    };

    // Single-branch icon labels (VSCode Git Graph style): a lone remote ref
    // gets a cloud icon; a local ref whose remote counterpart sits on the same
    // commit collapses into one ↔-prefixed label. Commits carrying more than
    // one distinct branch keep the standard rendering below.
    if branch_names.len() == 1 {
        let name = &branch_names[0];
        if strip_remote(name, remotes).is_some() {
            let prefix = format!("{} ", REMOTE_ONLY_ICON);
            return vec![(make_label(&prefix, name, None), make_style(name))];
        }
    } else if branch_names.len() == 2 {
        let synced_local = branch_names.iter().find(|name| {
            strip_remote(name, remotes).is_none()
                && branch_names
                    .iter()
                    .any(|other| strip_remote(other, remotes) == Some(name.as_str()))
        });
        if let Some(local) = synced_local {
            let prefix = format!("{} ", SYNCED_ICON);
            return vec![(make_label(&prefix, local, None), make_style(local))];
        }
    }

    // Process branches in original order (matches tab order from filter_remote_duplicates)
    let mut result: Vec<(String, Style)> = Vec::new();
    for name in branch_names {
        if let Some(bare) = strip_remote(name, remotes) {
            // Remote branch: skip if matching local exists
            if local_branches.contains(bare) {
                continue;
            }
            result.push((make_label("", name, None), make_style(name)));
        } else {
            // Local branch: show "↔ <remote>" when a remote ref points at the
            // same bare name (not just origin — any configured remote).
            let sync_remote = branch_names.iter().find_map(|other| {
                match strip_remote(other, remotes) {
                    Some(bare) if bare == name => other.strip_suffix(bare)?.strip_suffix('/'),
                    _ => None,
                }
            });
            let suffix = sync_remote.map(|r| format!("↔ {}", r));
            result.push((make_label("", name, suffix.as_deref()), make_style(name)));
        }
    }

    // Collapse multiple branches: show up to two labels, then "+N" for the rest
    if result.len() > 1 {
        // Number of labels to display inline before collapsing the remainder
        const SHOWN_LABELS: usize = 2;

        // Find selected index directly from branch_names, clamped to result bounds
        let selected_idx = selected_branch_name
            .and_then(|sel| {
                branch_names
                    .iter()
                    .position(|n| n == sel || n.ends_with(&format!("/{}", sel)))
            })
            .unwrap_or(0)
            .min(result.len().saturating_sub(1));

        // Display order: selected first, then remaining in original order
        let order: Vec<usize> = std::iter::once(selected_idx)
            .chain((0..result.len()).filter(|&i| i != selected_idx))
            .collect();

        let shown = SHOWN_LABELS.min(result.len());
        let extra_count = result.len() - shown;

        // Helper: strip "[...]" / suffix to recover the bare branch name
        let clean = |label: &str| -> String {
            label
                .trim_start_matches('[')
                .split([']', ' '])
                .next()
                .unwrap_or(label)
                .to_string()
        };

        // Budget the available width across the shown labels
        let per_label = MAX_LABEL_WIDTH / shown;

        let mut combined = String::new();
        for (pos, &idx) in order.iter().take(shown).enumerate() {
            let clean_name = clean(&result[idx].0);
            // Only the last shown label carries the "+N" suffix
            let extra = if pos == shown - 1 { extra_count } else { 0 };
            combined.push_str(&abbreviate_branch_label(&clean_name, per_label, extra));
        }

        // Style follows the selected branch
        let style = result[selected_idx].1;
        return vec![(combined, style)];
    }

    result
}

/// Truncate a string to the specified display width.
/// Handles VS16 which changes preceding character to emoji presentation (width 2).
fn truncate_to_width(s: &str, max_width: usize) -> String {
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

/// Determine which right-side elements (date, author, hash) to display based on available width.
/// Returns (show_date, show_author, show_hash, total_right_width).
/// Priority: author > date > hash (hash disappears first, then date, then author)
fn compute_right_side_visibility(remaining_for_content: usize) -> (bool, bool, bool, usize) {
    // Widths for each display level (right-aligned block)
    const WIDTH_DATE_AUTHOR_HASH: usize = 35; // " <date:14>  author    hash   "
    const WIDTH_DATE_AUTHOR: usize = 26; // " <date:14>  author   "
    const WIDTH_AUTHOR_ONLY: usize = 11; // "  author   "

    // Ensure minimum space for branch + commit message before showing right-side info
    const CONTENT_MIN_WIDTH: usize = 50;
    let available = remaining_for_content.saturating_sub(CONTENT_MIN_WIDTH);

    if available >= WIDTH_DATE_AUTHOR_HASH {
        (true, true, true, WIDTH_DATE_AUTHOR_HASH)
    } else if available >= WIDTH_DATE_AUTHOR {
        (true, true, false, WIDTH_DATE_AUTHOR)
    } else if available >= WIDTH_AUTHOR_ONLY {
        (false, true, false, WIDTH_AUTHOR_ONLY)
    } else {
        (false, false, false, 0)
    }
}

/// Abbreviate branch name to max_width, showing "+N" if more branches exist
/// Uses format: prefix/head...tail (preserving last 5 chars)
fn abbreviate_branch_label(name: &str, max_width: usize, extra_count: usize) -> String {
    const TAIL_LEN: usize = 5;
    const ELLIPSIS: &str = "...";

    let suffix = if extra_count > 0 {
        format!(" +{}", extra_count)
    } else {
        String::new()
    };

    let suffix_len = display_width(&suffix);
    let available = max_width.saturating_sub(suffix_len).saturating_sub(2); // -2 for brackets

    // If name fits, return as-is
    if display_width(name) <= available {
        return format!("[{}]{}", name, suffix);
    }

    // Find "/" position to preserve prefix
    let slash_pos = name.find('/');

    // Split into prefix and rest
    let (prefix, rest) = match slash_pos {
        Some(pos) => (&name[..=pos], &name[pos + 1..]),
        None => ("", name),
    };

    let prefix_width = display_width(prefix);
    let ellipsis_width = display_width(ELLIPSIS);

    // Get last TAIL_LEN characters from rest
    let rest_chars: Vec<char> = rest.chars().collect();
    let tail: String = if rest_chars.len() > TAIL_LEN {
        rest_chars[rest_chars.len() - TAIL_LEN..].iter().collect()
    } else {
        rest.to_string()
    };
    let tail_width = display_width(&tail);

    // Calculate available width for head portion
    let head_available = available.saturating_sub(prefix_width + ellipsis_width + tail_width);

    if head_available == 0 {
        // Not enough space for head, just show truncated name
        let truncated = truncate_to_width(name, available.saturating_sub(3));
        return format!("[{}...]{}", truncated, suffix);
    }

    let head = truncate_to_width(rest, head_available);

    format!("[{}{}{}{}]{}", prefix, head, ELLIPSIS, tail, suffix)
}

/// Format tag labels for a node. Tags render as `<name>` in the tag color,
/// visually distinct from `[branch]` and `{stash}` labels. Long tag names are
/// truncated with an ellipsis so a single tag can't dominate the row.
fn build_tag_labels(tag_names: &[String], theme: &Theme) -> Vec<(String, Style)> {
    // Total label width including the enclosing `<` `>` delimiters.
    const MAX_TAG_LABEL_WIDTH: usize = 24;
    let style = Style::default()
        .fg(theme.tag_label)
        .add_modifier(Modifier::BOLD);
    tag_names
        .iter()
        .map(|name| {
            let label = if display_width(name) + 2 <= MAX_TAG_LABEL_WIDTH {
                format!("<{}>", name)
            } else {
                // -3: two delimiters plus one ellipsis character.
                let head = truncate_to_width(name, MAX_TAG_LABEL_WIDTH - 3);
                format!("<{}…>", head)
            };
            (label, style)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)] // cohesive per-row render params; a struct would add indirection without clarity
fn render_graph_line<'a>(
    node: &GraphNode,
    max_lane: usize,
    is_selected: bool,
    is_marked: bool,
    total_width: usize,
    selected_branch_name: Option<&str>,
    theme: &Theme,
    now: DateTime<Local>,
    pixel_mode: bool,
    remotes: &[String],
    open_prs: &HashMap<String, PrInfo>,
) -> Line<'a> {
    let mut spans: Vec<Span> = Vec::new();

    // Graph start marker (to distinguish from borders). GRAPH_LEADING_COLUMNS
    // is the shared contract with the pixel overlay's x-offset.
    spans.push(Span::raw(" ".repeat(GRAPH_LEADING_COLUMNS as usize)));
    let mut left_width: usize = GRAPH_LEADING_COLUMNS as usize;

    // Pixel mode: the graph column is painted by an image overlay, so emit
    // blank space of the exact same width (one column per cell, HEAD star
    // included) to keep the text layout identical.
    if pixel_mode {
        for _ in &node.cells {
            spans.push(Span::raw(" "));
            left_width += 1;
        }
    } else {
        left_width = render_cells_unicode(&mut spans, node, theme, left_width);
    }

    // Padding to align graph width (display width based)
    let graph_display_width = (max_lane + 1) * 2;
    if left_width < graph_display_width + 1 {
        // +1 accounts for the start marker
        let padding = graph_display_width + 1 - left_width;
        spans.push(Span::raw(" ".repeat(padding)));
        left_width += padding;
    }

    render_graph_line_tail(
        spans,
        left_width,
        node,
        is_selected,
        is_marked,
        total_width,
        selected_branch_name,
        theme,
        now,
        remotes,
        open_prs,
    )
}

/// Render the Unicode box-drawing glyphs for a row's cells into `spans`,
/// returning the updated `left_width`.
fn render_cells_unicode(
    spans: &mut Vec<Span<'_>>,
    node: &GraphNode,
    theme: &Theme,
    mut left_width: usize,
) -> usize {
    // Render cells.
    //
    // The HEAD commit is drawn with a width-2 star emoji so it stands out. To
    // keep column alignment intact, the emoji occupies the commit cell *and*
    // the cell to its right — but only when that right cell carries no graph
    // line (Empty or a plain Horizontal connector). If a pipe or junction sits
    // there, painting over it would corrupt the graph, so we fall back to the
    // width-1 glyph instead.
    let mut idx = 0;
    while idx < node.cells.len() {
        let cell = &node.cells[idx];

        // Special-case the HEAD commit star (may consume the next cell).
        if node.is_head {
            if let CellType::Commit(color_idx) = cell {
                let is_main = *color_idx == crate::graph::colors::MAIN_BRANCH_COLOR;
                let color = if !is_main {
                    theme.branch_head
                } else {
                    theme.lane_color(*color_idx)
                };
                let style = Style::default().fg(color).add_modifier(Modifier::BOLD);

                let right_is_clear = matches!(
                    node.cells.get(idx + 1),
                    None | Some(CellType::Empty) | Some(CellType::Horizontal(_))
                );
                if right_is_clear {
                    // ⭐ is width-2; it spans the commit cell + the cleared neighbor.
                    spans.push(Span::styled("⭐", style));
                    left_width += 2;
                    idx += 2; // skip the swallowed cell
                    continue;
                } else {
                    // No room to widen: keep the distinct width-1 HEAD glyph.
                    spans.push(Span::styled("◉".to_string(), style));
                    left_width += 1;
                    idx += 1;
                    continue;
                }
            }
        }

        let (ch, color) = match cell {
            CellType::Empty => (' ', Color::Reset),
            CellType::Pipe(color_idx) => ('│', theme.lane_color(*color_idx)),
            CellType::Commit(color_idx) => ('●', theme.lane_color(*color_idx)),
            CellType::BranchRight(color_idx) => ('╭', theme.lane_color(*color_idx)),
            CellType::BranchLeft(color_idx) => ('╮', theme.lane_color(*color_idx)),
            CellType::MergeRight(color_idx) => ('╰', theme.lane_color(*color_idx)),
            CellType::MergeLeft(color_idx) => ('╯', theme.lane_color(*color_idx)),
            CellType::Horizontal(color_idx) => ('─', theme.lane_color(*color_idx)),
            CellType::HorizontalPipe(_h_color_idx, p_color_idx) => {
                // Vertical and horizontal lines cross (use pipe color)
                ('┼', theme.lane_color(*p_color_idx))
            }
            CellType::TeeRight(color_idx) => ('├', theme.lane_color(*color_idx)),
            CellType::TeeLeft(color_idx) => ('┤', theme.lane_color(*color_idx)),
            CellType::TeeUp(color_idx) => ('┴', theme.lane_color(*color_idx)),
        };

        // Draw all line glyphs in bold
        let style = Style::default().fg(color).add_modifier(Modifier::BOLD);

        let ch_str = ch.to_string();
        let ch_width = display_width(&ch_str);
        spans.push(Span::styled(ch_str, style));
        left_width += ch_width;
        idx += 1;
    }

    left_width
}

/// Render everything after the graph column: separator, compare marker,
/// branch/tag/stash labels, message, and the right-aligned metadata block.
#[allow(clippy::too_many_arguments)] // cohesive per-row render params; a struct would add indirection without clarity
fn render_graph_line_tail<'a>(
    mut spans: Vec<Span<'a>>,
    mut left_width: usize,
    node: &GraphNode,
    is_selected: bool,
    is_marked: bool,
    total_width: usize,
    selected_branch_name: Option<&str>,
    theme: &Theme,
    now: DateTime<Local>,
    remotes: &[String],
    open_prs: &HashMap<String, PrInfo>,
) -> Line<'a> {
    // Separator between graph and commit info
    spans.push(Span::raw(" "));
    left_width += 1;

    // Compare marker: flags commits that are marked or a comparison endpoint.
    if is_marked {
        spans.push(Span::styled(
            "◆ ",
            Style::default()
                .fg(theme.search_cursor)
                .add_modifier(Modifier::BOLD),
        ));
        left_width += 2;
    }

    // Handle uncommitted changes row
    if node.is_uncommitted {
        let text = match node.uncommitted_count {
            Some(count) => format!("uncommitted changes ({})", count),
            None => "uncommitted changes".to_string(),
        };
        let style = Style::default().fg(theme.text_primary);
        spans.push(Span::styled(text, style));
        return Line::from(spans);
    }

    // Early return for connector-only rows
    let commit = match &node.commit {
        Some(c) => c,
        None => return Line::from(spans),
    };

    // Style definitions
    let hash_style = Style::default().fg(theme.hash_color);
    let author_style = Style::default().fg(theme.author_color);
    let date_style = Style::default().fg(theme.date_color);
    let msg_style = if is_selected {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    // === Left-aligned: branch names + message ===

    // Optimize branch names (compact when local matches origin/local)
    let branch_display = optimize_branch_display(
        &node.branch_names,
        node.is_head,
        node.color_index,
        selected_branch_name,
        theme,
        remotes,
    );

    // Tag labels render after branch labels with a distinct color.
    let tag_display = build_tag_labels(&node.tag_names, theme);

    // Open-PR badge: chip after the branch labels when one of this node's
    // branches has an open PR.
    let pr_badge = pr_for_branch_labels(&node.branch_names, remotes, open_prs).map(pr_badge_text);
    // Chip plus a trailing space.
    let pr_badge_width = pr_badge.as_ref().map_or(0, |b| display_width(b) + 1);

    // === Right-aligned: date author hash (fixed width) ===
    let date = format_date_field(commit.timestamp, now); // DATE_FIELD_WIDTH chars
    let author = truncate_to_width(&commit.author_name, 8);
    let author_formatted = format!("{:<8}", author); // fixed 8 chars
    let hash = truncate_to_width(&commit.short_id, 7);
    let hash_formatted = format!("{:<7}", hash); // fixed 7 chars

    // Calculate branch width first (before rendering)
    let branch_width: usize = branch_display
        .iter()
        .enumerate()
        .map(|(i, (label, _))| display_width(label) + if i > 0 { 1 } else { 0 })
        .sum::<usize>()
        + if !branch_display.is_empty() { 1 } else { 0 };

    // Each tag label carries a trailing space (see rendering below).
    let tag_width: usize = tag_display
        .iter()
        .map(|(label, _)| display_width(label) + 1)
        .sum();

    // Calculate remaining space for branch + message + right info
    let graph_width = left_width;
    let remaining_for_content = total_width.saturating_sub(graph_width);

    // Determine which right-side elements to show based on available space
    let (show_date, show_author, show_hash, right_width) =
        compute_right_side_visibility(remaining_for_content);

    // Render branch labels
    for (i, (label, style)) in branch_display.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
            left_width += 1;
        }
        left_width += display_width(label);
        spans.push(Span::styled(label.clone(), *style));
    }
    if !branch_display.is_empty() {
        spans.push(Span::raw(" "));
        left_width += 1;
    }

    // Render open-PR badge (after branch labels, before tags)
    if let Some(badge) = &pr_badge {
        let style = Style::default()
            .fg(theme.pr_badge)
            .add_modifier(Modifier::BOLD);
        left_width += display_width(badge) + 1;
        spans.push(Span::styled(badge.clone(), style));
        spans.push(Span::raw(" "));
    }

    // Render tag labels (after branches, before the stash label)
    for (label, style) in &tag_display {
        left_width += display_width(label) + 1;
        spans.push(Span::styled(label.clone(), *style));
        spans.push(Span::raw(" "));
    }

    // Render stash label
    let stash_width = if let Some(stash_label) = &node.stash_label {
        let label = format!("{{{}}}", stash_label);
        let stash_style = Style::default()
            .fg(theme.text_muted)
            .add_modifier(Modifier::ITALIC);
        let w = display_width(&label) + 1;
        left_width += w;
        spans.push(Span::styled(label, stash_style));
        spans.push(Span::raw(" "));
        w
    } else {
        0
    };

    // Compute max message width (remaining space after branch, stash label, and right side)
    let available_for_message = remaining_for_content
        .saturating_sub(branch_width)
        .saturating_sub(pr_badge_width)
        .saturating_sub(tag_width)
        .saturating_sub(stash_width)
        .saturating_sub(right_width);
    let message = truncate_to_width(&commit.message, available_for_message);
    let message_width = display_width(&message);
    spans.push(Span::styled(message, msg_style));
    left_width += message_width;

    // Padding so the right-aligned block starts at a fixed column
    let padding = total_width
        .saturating_sub(left_width)
        .saturating_sub(right_width);
    if padding > 0 {
        spans.push(Span::raw(" ".repeat(padding)));
    }

    // === Append right-aligned block (display: date, author, hash) ===
    if show_date {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(date, date_style));
    }
    if show_author {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(author_formatted, author_style));
    }
    if show_hash {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(hash_formatted, hash_style));
    }
    if show_date || show_author || show_hash {
        spans.push(Span::raw(" "));
    }

    Line::from(spans)
}

impl<'a> StatefulWidget for GraphViewWidget<'a> {
    type State = ListState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.width < MIN_WIDGET_WIDTH || area.height < MIN_WIDGET_HEIGHT {
            render_placeholder_block(area, buf, self.theme);
            return;
        }

        let block = Block::default()
            .title(self.title)
            .borders(Borders::ALL)
            .border_style(self.theme.border_style(self.is_focused))
            .border_type(self.theme.border_type(self.is_focused));

        let list = List::new(self.items)
            .block(block)
            .highlight_style(self.theme.selection_style());

        let mut filtered_state = ListState::default();
        filtered_state.select(self.selected_in_filtered);
        StatefulWidget::render(list, area, buf, &mut filtered_state);
        // Propagate scroll offset back to the original state
        *state.offset_mut() = *filtered_state.offset_mut();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    /// `now` minus `secs_ago` seconds. Duration arithmetic on `DateTime<Tz>`
    /// operates on the underlying instant, so this is exact regardless of
    /// DST — safe to use with `Local::now()` as the anchor.
    fn ago(now: DateTime<Local>, secs_ago: i64) -> DateTime<Local> {
        now - Duration::seconds(secs_ago)
    }

    // ── format_date_field ────────────────────────────────────────────

    #[test]
    fn future_timestamp_shows_absolute_date() {
        let now = Local::now();
        let future = now + Duration::seconds(60);
        let result = format_date_field(future, now);
        assert_eq!(result.trim_end(), future.format("%Y-%m-%d").to_string());
    }

    #[test]
    fn seven_days_or_more_shows_absolute_date() {
        let now = Local::now();
        let old = ago(now, 7 * 24 * 3600);
        let result = format_date_field(old, now);
        assert_eq!(result.trim_end(), old.format("%Y-%m-%d").to_string());
    }

    #[test]
    fn under_a_minute_is_just_now() {
        let now = Local::now();
        let recent = ago(now, 30);
        assert_eq!(format_date_field(recent, now).trim_end(), "just now");
    }

    #[test]
    fn fifty_nine_seconds_is_still_just_now() {
        let now = Local::now();
        let recent = ago(now, 59);
        assert_eq!(format_date_field(recent, now).trim_end(), "just now");
    }

    #[test]
    fn sixty_seconds_is_one_minute_ago() {
        let now = Local::now();
        let recent = ago(now, 60);
        assert_eq!(format_date_field(recent, now).trim_end(), "1 minute ago");
    }

    #[test]
    fn singular_minute() {
        let now = Local::now();
        let t = ago(now, 90); // num_minutes() truncates to 1
        assert_eq!(format_date_field(t, now).trim_end(), "1 minute ago");
    }

    #[test]
    fn plural_minutes() {
        let now = Local::now();
        let t = ago(now, 5 * 60);
        assert_eq!(format_date_field(t, now).trim_end(), "5 minutes ago");
    }

    #[test]
    fn singular_hour() {
        let now = Local::now();
        let t = ago(now, 3600);
        assert_eq!(format_date_field(t, now).trim_end(), "1 hour ago");
    }

    #[test]
    fn plural_hours() {
        let now = Local::now();
        let t = ago(now, 3 * 3600);
        assert_eq!(format_date_field(t, now).trim_end(), "3 hours ago");
    }

    #[test]
    fn singular_day() {
        let now = Local::now();
        let t = ago(now, 24 * 3600);
        assert_eq!(format_date_field(t, now).trim_end(), "1 day ago");
    }

    #[test]
    fn plural_days() {
        let now = Local::now();
        let t = ago(now, 3 * 24 * 3600);
        assert_eq!(format_date_field(t, now).trim_end(), "3 days ago");
    }

    #[test]
    fn six_days_twenty_three_hours_is_still_relative() {
        let now = Local::now();
        let t = ago(now, 6 * 24 * 3600 + 23 * 3600);
        assert_eq!(format_date_field(t, now).trim_end(), "6 days ago");
    }

    #[test]
    fn result_is_padded_to_fixed_width() {
        let now = Local::now();
        let recent = ago(now, 5);
        assert_eq!(format_date_field(recent, now).len(), DATE_FIELD_WIDTH);
    }

    // ── optimize_branch_display icons ────────────────────────────────

    fn labels(branch_names: &[&str], remotes: &[&str]) -> Vec<String> {
        let names: Vec<String> = branch_names.iter().map(|s| s.to_string()).collect();
        let remotes: Vec<String> = remotes.iter().map(|s| s.to_string()).collect();
        let theme = Theme::dark();
        optimize_branch_display(&names, false, 0, None, &theme, &remotes)
            .into_iter()
            .map(|(label, _)| label)
            .collect()
    }

    #[test]
    fn lone_remote_ref_gets_a_cloud_icon() {
        let out = labels(&["origin/feature"], &["origin"]);
        assert_eq!(out, vec![format!("[{} origin/feature]", REMOTE_ONLY_ICON)]);
    }

    #[test]
    fn lone_remote_ref_respects_non_origin_remotes() {
        let out = labels(&["upstream/main"], &["origin", "upstream"]);
        assert_eq!(out, vec![format!("[{} upstream/main]", REMOTE_ONLY_ICON)]);
    }

    #[test]
    fn synced_local_and_remote_collapse_to_one_sync_label() {
        let out = labels(&["main", "origin/main"], &["origin"]);
        assert_eq!(out, vec![format!("[{} main]", SYNCED_ICON)]);
    }

    #[test]
    fn synced_pair_order_does_not_matter() {
        let out = labels(&["origin/main", "main"], &["origin"]);
        assert_eq!(out, vec![format!("[{} main]", SYNCED_ICON)]);
    }

    #[test]
    fn slashed_local_branch_is_not_mistaken_for_a_remote_ref() {
        let out = labels(&["feature/foo"], &["origin"]);
        assert_eq!(out, vec!["[feature/foo]".to_string()]);
    }

    #[test]
    fn multiple_distinct_branches_get_no_icons() {
        let out = labels(&["main", "dev"], &["origin"]);
        for label in &out {
            assert!(!label.contains(REMOTE_ONLY_ICON), "no cloud icon: {label}");
            assert!(!label.contains(SYNCED_ICON), "no sync icon: {label}");
        }
    }

    #[test]
    fn two_unrelated_refs_do_not_collapse() {
        let out = labels(&["main", "origin/dev"], &["origin"]);
        assert!(
            !out.iter()
                .any(|l| l.contains(SYNCED_ICON) || l.contains(REMOTE_ONLY_ICON)),
            "unrelated local+remote must not be treated as synced: {out:?}"
        );
    }

    #[test]
    fn long_remote_ref_with_icon_stays_within_label_budget() {
        let long = format!("origin/{}", "x".repeat(60));
        let out = labels(&[&long], &["origin"]);
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with(&format!("[{} ", REMOTE_ONLY_ICON)));
        assert!(display_width(&out[0]) <= 40, "label too wide: {}", out[0]);
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

    // ── build_tag_labels ─────────────────────────────────────────────

    #[test]
    fn no_tags_produces_no_labels() {
        let theme = Theme::dark();
        assert!(build_tag_labels(&[], &theme).is_empty());
    }

    #[test]
    fn short_tag_is_wrapped_in_angle_brackets() {
        let theme = Theme::dark();
        let labels = build_tag_labels(&["v1.0".to_string()], &theme);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].0, "<v1.0>");
        // Distinct from branch labels: rendered in the tag color.
        assert_eq!(labels[0].1.fg, Some(theme.tag_label));
    }

    #[test]
    fn each_tag_gets_its_own_label() {
        let theme = Theme::dark();
        let labels = build_tag_labels(&["v1.0".to_string(), "release".to_string()], &theme);
        let rendered: Vec<&str> = labels.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(rendered, vec!["<v1.0>", "<release>"]);
    }

    #[test]
    fn overlong_tag_is_truncated_within_the_width_budget() {
        let theme = Theme::dark();
        let long = "an-extremely-long-tag-name-that-will-not-fit-on-one-line".to_string();
        let labels = build_tag_labels(&[long], &theme);
        let label = &labels[0].0;
        // Never exceeds the label budget, stays bracketed, and is elided.
        assert!(display_width(label) <= 24, "label too wide: {label:?}");
        assert!(label.starts_with('<') && label.ends_with('>'));
        assert!(label.contains('…'), "expected an ellipsis: {label:?}");
    }

    // ── remote classification (strip_remote / optimize_branch_display) ───

    #[test]
    fn strip_remote_classifies_by_configured_remote_only() {
        let remotes = vec!["origin".to_string(), "upstream".to_string()];
        assert_eq!(strip_remote("upstream/main", &remotes), Some("main"));
        assert_eq!(strip_remote("origin/feature/x", &remotes), Some("feature/x"));
        // A slash alone does not make a branch remote.
        assert_eq!(strip_remote("feature/x", &remotes), None);
        assert_eq!(strip_remote("main", &remotes), None);
        // Not a configured remote.
        assert_eq!(strip_remote("fork/main", &remotes), None);
    }

    #[test]
    fn multi_branch_dedupes_non_origin_remote_counterpart() {
        let theme = Theme::dark();
        // main + its upstream counterpart + an unrelated local branch. Three
        // names skip the single/pair early-returns and hit the general loop.
        let names = vec![
            "main".to_string(),
            "upstream/main".to_string(),
            "dev".to_string(),
        ];
        let remotes = vec!["upstream".to_string()];
        let out = optimize_branch_display(&names, false, 0, None, &theme, &remotes);
        // Collapses to a single combined label.
        assert_eq!(out.len(), 1);
        let label = &out[0].0;
        // upstream/main is recognized as main's remote counterpart and deduped,
        // leaving two real branches — so no "+N" overflow marker appears.
        assert!(
            !label.contains('+'),
            "non-origin remote counterpart should be deduped: {label:?}"
        );
        assert!(label.contains("main"), "expected main: {label:?}");
        assert!(label.contains("dev"), "expected dev: {label:?}");
    }

    #[test]
    fn slashed_local_branch_is_not_treated_as_remote() {
        let theme = Theme::dark();
        // "feature/x" contains a slash but "feature" is not a remote.
        let names = vec!["feature/x".to_string()];
        let remotes = vec!["origin".to_string()];
        let out = optimize_branch_display(&names, false, 0, None, &theme, &remotes);
        assert_eq!(out.len(), 1);
        // No cloud icon (remote-only marker); rendered as a plain local label.
        assert!(
            !out[0].0.contains(REMOTE_ONLY_ICON),
            "local branch must not get the remote icon: {:?}",
            out[0].0
        );
    }

    // ── open-PR badge matching ───────────────────────────────────────

    fn pr(number: u64) -> PrInfo {
        PrInfo {
            number,
            url: format!("https://github.com/o/r/pull/{number}"),
            title: "t".to_string(),
        }
    }

    fn prs(pairs: &[(&str, u64)]) -> HashMap<String, PrInfo> {
        pairs.iter().map(|(b, n)| (b.to_string(), pr(*n))).collect()
    }

    #[test]
    fn pr_matches_local_branch_label() {
        let open = prs(&[("feat/x", 12)]);
        let names = vec!["feat/x".to_string()];
        let found = pr_for_branch_labels(&names, &[], &open);
        assert_eq!(found.map(|p| p.number), Some(12));
    }

    #[test]
    fn pr_matches_remote_ref_by_stripped_name() {
        let open = prs(&[("feat/x", 3)]);
        // Both origin and a non-origin remote strip to the PR's head branch.
        let remotes = vec!["origin".to_string(), "upstream".to_string()];
        assert_eq!(
            pr_for_branch_labels(&["origin/feat/x".to_string()], &remotes, &open).map(|p| p.number),
            Some(3)
        );
        assert_eq!(
            pr_for_branch_labels(&["upstream/feat/x".to_string()], &remotes, &open)
                .map(|p| p.number),
            Some(3)
        );
    }

    #[test]
    fn pr_no_match_returns_none() {
        let open = prs(&[("feat/x", 1)]);
        assert!(pr_for_branch_labels(&["other".to_string()], &[], &open).is_none());
        // A slashed local branch is not stripped, so it won't accidentally match
        // a PR whose head is the trailing segment.
        let open2 = prs(&[("x", 9)]);
        assert!(pr_for_branch_labels(&["feature/x".to_string()], &["origin".to_string()], &open2).is_none());
    }

    #[test]
    fn pr_first_matching_label_wins() {
        let open = prs(&[("b", 2), ("a", 1)]);
        // Labels checked in order; "a" comes first, so PR #1.
        let names = vec!["a".to_string(), "b".to_string()];
        assert_eq!(pr_for_branch_labels(&names, &[], &open).map(|p| p.number), Some(1));
    }

    #[test]
    fn pr_badge_text_is_compact() {
        let text = pr_badge_text(&pr(42));
        assert!(text.contains("#42"));
        assert!(text.starts_with(PR_BADGE_ICON));
        // Icon(1) + space(1) + "#42"(3) = 5 display columns.
        assert_eq!(display_width(&text), 5);
    }
}
