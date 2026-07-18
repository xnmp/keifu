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
    config::MetadataColumns,
    git::graph::{CellType, GraphNode},
    mouse::{ChipHit, ChipTarget},
    pr::{CiStatus, PrInfo, ReviewState},
};

/// Fixed display width for the compact date field (fits "11mo", "now", "59m").
/// The full absolute date lives in the commit detail panel.
const DATE_FIELD_WIDTH: usize = 4;

/// Compact relative age of a commit: "now", "59m", "23h", "6d", "3w", "11mo",
/// "2y". Left-padded to `DATE_FIELD_WIDTH` so the column stays aligned.
/// `now` is passed in so it's computed once per render, not once per row.
fn format_date_field(timestamp: DateTime<Local>, now: DateTime<Local>) -> String {
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
    /// Clickable chip regions per rendered row (indexed by filtered row
    /// position), for mouse hit-testing. Consumed by the caller before render.
    pub chip_hits: Vec<Vec<ChipHit>>,
}

/// Number of leading blank columns before the graph glyphs on every row (the
/// start marker). Both the space-emitter in `render_graph_line` and the pixel
/// overlay in `ui::mod` key off this so the image lines up with the glyph slot.
pub const GRAPH_LEADING_COLUMNS: u16 = 1;

/// Columns reserved between the graph and the message for the author avatar
/// (pixel mode only): a ~square 2-cell-wide image, then a 1-cell gap. The text
/// layer emits blank space here; a separate overlay draws the avatar image.
pub const AVATAR_IMAGE_CELLS: u16 = 2;
pub const AVATAR_GAP_CELLS: u16 = 1;
pub const AVATAR_RESERVED_CELLS: u16 = AVATAR_IMAGE_CELLS + AVATAR_GAP_CELLS;

/// Whether avatars should render this frame: pixel mode on, toggle on.
pub fn avatars_active(pixel_mode: bool, metadata_columns: MetadataColumns) -> bool {
    pixel_mode && metadata_columns.avatars
}

/// The screen x-column where the avatar image is drawn: immediately after the
/// (padded) graph column. `inner_x` is the panel's inner-left edge.
pub fn avatar_overlay_x(inner_x: u16, graph_width: usize) -> u16 {
    inner_x + graph_width as u16 + GRAPH_LEADING_COLUMNS
}

/// The graph column width in cells actually shown: the number needed to fit all
/// lanes (`needed`), unless the user set a smaller cap. `cap == None` — or a cap
/// at/above `needed` — means uncapped. Never below 4 (two lanes) or above
/// `needed`.
pub fn effective_graph_width(needed: usize, cap: Option<usize>) -> usize {
    match cap {
        None => needed,
        Some(c) => {
            let lo = 4.min(needed);
            c.clamp(lo, needed)
        }
    }
}

/// The next graph-width cap after a resize step of `direction` lanes (each lane
/// = 2 cells). Negative shrinks (floor 4 cells); positive widens, and widening
/// to or past `needed` returns `None` (uncapped). A stale cap wider than
/// `needed` is treated as uncapped, so shrinking from it caps at `needed - 2`.
pub fn next_graph_cap(needed: usize, cap: Option<usize>, direction: i32) -> Option<usize> {
    let eff = effective_graph_width(needed, cap);
    let new = if direction < 0 {
        eff.saturating_sub(2).max(4).min(needed)
    } else {
        eff + 2
    };
    if new >= needed {
        None
    } else {
        Some(new)
    }
}

/// For a row of `n` graph cells and an effective `graph_width` (in cells),
/// returns (cells to render, whether a `…` marker is appended). When the row
/// overflows the width, one column is reserved for the marker. Shared by both
/// renderers so the truncation point and the ellipsis agree.
fn graph_truncation(n: usize, graph_width: usize) -> (usize, bool) {
    if n > graph_width {
        (graph_width.saturating_sub(1), true)
    } else {
        (n, false)
    }
}

/// Cells drawn in a pixel row's image: the `graph_width` truncation budget,
/// further bounded by what fits the panel (`panel_available`). The `…` marker,
/// when truncating, is drawn by the text layer, so it's excluded here.
fn pixel_row_cells(n: usize, graph_width: usize, panel_available: usize) -> usize {
    graph_truncation(n, graph_width).0.min(panel_available)
}

/// Dim style for the truncation `…` marker.
fn ellipsis_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.text_muted)
        .add_modifier(Modifier::DIM)
}

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

/// One rendered graph row: a node plus, when connectors are folded (pixel mode),
/// the cells of any preceding connector row(s) collapsed into it as an
/// `underlay`. `full_idx` is the row's index into `graph_layout.nodes` (used to
/// map selection, exactly like `visible_nodes`).
pub struct RenderRow<'a> {
    pub full_idx: usize,
    pub node: &'a GraphNode,
    pub underlay: Vec<CellType>,
    /// Per-cell edge OIDs for `underlay`, folded in parallel, for branch tracing.
    pub underlay_oids: Vec<crate::git::graph::CellOids>,
}

/// The rows the graph list renders, in list order. When `fold_connectors` is
/// false (Unicode mode) every visible node is its own row. When true (pixel
/// mode) standalone connector rows are removed and folded into the following
/// commit row's `underlay`, so the list, the pixel specs, the scroll offset, and
/// selection indices all share one filtered index space.
///
/// Connectors always precede their commit, so folding attaches each connector to
/// the next commit row. A trailing connector with no following commit (never
/// produced by `build_graph`, but handled defensively) renders standalone.
pub fn visible_rows(app: &App, fold_connectors: bool) -> Vec<RenderRow<'_>> {
    fold_rows(visible_nodes(app), fold_connectors)
}

/// Pure core of [`visible_rows`]: fold (or not) a list of `(full_idx, node)`
/// pairs into rendered rows. Extracted so the folding is unit-testable without
/// constructing an `App`.
fn fold_rows(base: Vec<(usize, &GraphNode)>, fold_connectors: bool) -> Vec<RenderRow<'_>> {
    if !fold_connectors {
        return base
            .into_iter()
            .map(|(full_idx, node)| RenderRow {
                full_idx,
                node,
                underlay: Vec::new(),
                underlay_oids: Vec::new(),
            })
            .collect();
    }

    let mut rows: Vec<RenderRow> = Vec::new();
    let mut pending: Vec<(usize, &GraphNode)> = Vec::new();
    for (full_idx, node) in base {
        if node.is_connector() {
            pending.push((full_idx, node));
        } else {
            let (underlay, underlay_oids) = merge_connector_cells(&pending);
            pending.clear();
            rows.push(RenderRow {
                full_idx,
                node,
                underlay,
                underlay_oids,
            });
        }
    }
    // Trailing connectors with no following commit: render standalone.
    for (full_idx, node) in pending {
        rows.push(RenderRow {
            full_idx,
            node,
            underlay: Vec::new(),
            underlay_oids: Vec::new(),
        });
    }
    rows
}

/// Merge one or more connector rows' cells into a single underlay row: per
/// column, the last non-empty cell wins. `build_graph` never emits adjacent
/// connectors, so in practice this collapses a single connector.
fn merge_connector_cells(
    pending: &[(usize, &GraphNode)],
) -> (Vec<CellType>, Vec<crate::git::graph::CellOids>) {
    let width = pending.iter().map(|(_, n)| n.cells.len()).max().unwrap_or(0);
    let mut out = vec![CellType::Empty; width];
    let mut out_oids = vec![(None, None); width];
    for (_, node) in pending {
        for (col, cell) in node.cells.iter().enumerate() {
            if *cell != CellType::Empty {
                out[col] = *cell;
                // Fold the cell's edge identity alongside it, so tracing marks
                // folded connector strokes the same as unfolded ones.
                out_oids[col] = node.cell_oids.get(col).copied().unwrap_or((None, None));
            }
        }
    }
    (out, out_oids)
}

/// The effective cells physically adjacent to `rows[i]` in the given direction,
/// used to resolve a commit dot's connect-up/down. A folded connector sits
/// between a commit and the row on the connector's far side, so it takes
/// precedence (per column) over the neighbouring commit's own cells.
fn adjacent_cells(rows: &[RenderRow], i: usize, above: bool) -> Option<Vec<CellType>> {
    // The underlay that lies between row i's dot and the neighbour: for the row
    // above, it's row i's own underlay; for the row below, it's the next row's.
    let (underlay, neighbour): (&[CellType], Option<&[CellType]>) = if above {
        (
            &rows[i].underlay,
            i.checked_sub(1).map(|p| rows[p].node.cells.as_slice()),
        )
    } else {
        match rows.get(i + 1) {
            Some(next) => (&next.underlay, Some(next.node.cells.as_slice())),
            None => (&[], None),
        }
    };
    if underlay.is_empty() && neighbour.is_none() {
        return None;
    }
    let width = underlay
        .len()
        .max(neighbour.map_or(0, |c| c.len()));
    let mut out = vec![CellType::Empty; width];
    for (col, slot) in out.iter_mut().enumerate() {
        let u = underlay.get(col).copied().unwrap_or(CellType::Empty);
        *slot = if u != CellType::Empty {
            u
        } else {
            neighbour
                .and_then(|c| c.get(col))
                .copied()
                .unwrap_or(CellType::Empty)
        };
    }
    Some(out)
}

/// Build one `RowSpec` per list item (respecting the active commit filter), in
/// the same order the graph widget lists them, so the overlay can index into it
/// by visible-row position. Neighbours follow visible order.
///
/// Each spec's cells are truncated to what the overlay actually draws:
/// - `graph_width` is the user-capped effective width; a row overflowing it
///   reserves one column for the `…` marker (added by the text layer).
/// - `panel_available` bounds the image to the render area — the iTerm2/Sixel
///   fixed protocols render *nothing* when the protocol is wider than its render
///   area (only Kitty crops), so the cached protocol's width must fit.
///
/// Truncated specs still hash as themselves, so the protocol cache holds.
pub fn build_pixel_row_specs(
    app: &App,
    theme: &Theme,
    graph_width: usize,
    panel_available: usize,
) -> Vec<crate::ui::graph_pixels::RowSpec> {
    use crate::ui::graph_pixels::build_row_spec;
    // The selected commit's lineage, when tracing is active; non-traced cells
    // are dimmed. `None` = tracing off, everything full strength.
    let trace = app.active_trace_lineage();
    // Fold connector rows into their following commit row (pixel-only): one spec
    // per rendered row, in the same order and index space as the list items.
    let rows = visible_rows(app, true);
    (0..rows.len())
        .map(|i| {
            let node = rows[i].node;
            let above = adjacent_cells(&rows, i, true);
            let below = adjacent_cells(&rows, i, false);
            let mut spec = build_row_spec(
                above.as_deref(),
                node,
                below.as_deref(),
                &rows[i].underlay,
                theme,
            );
            // Mark non-lineage cells for dimming. `spec.cells`/`spec.underlay`
            // are 1:1 with `node.cells`/`rows[i].underlay`, so their OIDs align.
            if let Some(lineage) = &trace {
                apply_trace_dim(&mut spec.cells, &node.cell_oids, lineage);
                apply_trace_dim(&mut spec.underlay, &rows[i].underlay_oids, lineage);
            }
            let budget = pixel_row_cells(node.cells.len(), graph_width, panel_available);
            spec.cells.truncate(budget);
            // Keep the underlay within the row's drawn width so it stays inside
            // the rasterized canvas (sized from `cells`).
            let underlay_cap = spec.cells.len();
            spec.underlay.truncate(underlay_cap);
            spec
        })
        .collect()
}

/// Set `dim` on every pixel cell whose edge OIDs are not in `lineage`.
fn apply_trace_dim(
    cells: &mut [crate::ui::graph_pixels::PixelCell],
    oids: &[crate::git::graph::CellOids],
    lineage: &std::collections::HashSet<git2::Oid>,
) {
    for (i, pc) in cells.iter_mut().enumerate() {
        let cell_oids = oids.get(i).copied().unwrap_or((None, None));
        pc.dim = !crate::git::graph::cell_is_traced(cell_oids, lineage);
    }
}

impl<'a> GraphViewWidget<'a> {
    pub fn new(app: &App, width: u16, theme: &'a Theme, pixel_mode: bool) -> Self {
        let needed = (app.graph_layout.max_lane + 1) * 2;
        let graph_width = effective_graph_width(needed, app.graph_width_cap);
        let inner_width = width.saturating_sub(2) as usize;
        let selected_branch_name = app.selected_branch_name();
        let has_filter = !app.commit_filter.is_empty();
        let current_selected = app.graph_nav.graph_list_state.selected();
        let now = Local::now();
        let remotes = &app.remotes;
        let open_prs = &app.open_prs;
        let metadata_columns = app.metadata_columns;
        // Selected commit's lineage for tracing (Unicode dim); None = off. In
        // pixel mode the dim lives in the row specs, not the text layer.
        let trace = if pixel_mode {
            None
        } else {
            app.active_trace_lineage()
        };

        // In pixel mode, connector rows are folded into their commit row so the
        // list items match the pixel specs one-for-one (same filtered index
        // space). In Unicode mode connectors remain their own rows.
        let rows = visible_rows(app, pixel_mode);

        let mut selected_in_filtered = None;
        let mut items: Vec<ListItem> = Vec::new();
        let mut chip_hits: Vec<Vec<ChipHit>> = Vec::new();

        for (filtered_pos, row) in rows.into_iter().enumerate() {
            let (full_idx, node) = (row.full_idx, row.node);
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
            let (line, chips) = render_graph_line(
                node,
                graph_width,
                is_selected,
                is_marked,
                inner_width,
                selected_branch_name,
                theme,
                now,
                pixel_mode,
                remotes,
                open_prs,
                metadata_columns,
                trace.as_ref(),
            );
            items.push(ListItem::new(line));
            chip_hits.push(chips);
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
            chip_hits,
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

/// Single source of truth for whether graph label chips drop the remote prefix:
/// only when the repo has exactly one remote (the cloud icon then conveys
/// remoteness, so `<remote>/` is redundant). Multi-remote repos keep prefixes to
/// disambiguate which remote a ref belongs to.
fn strip_prefix_in_labels(remotes: &[String]) -> bool {
    remotes.len() == 1
}

/// The name shown on a branch chip: for a remote ref in a single-remote repo,
/// the remote prefix is dropped; otherwise the full ref name is used.
fn chip_display_name<'n>(name: &'n str, remotes: &[String]) -> &'n str {
    if strip_prefix_in_labels(remotes) {
        strip_remote(name, remotes).unwrap_or(name)
    } else {
        name
    }
}

/// Nerd Font octicons for the open-PR badge and its actioned markers.
const PR_BADGE_ICON: char = '\u{f407}'; // nf-oct-git_pull_request
const PR_APPROVED_ICON: char = '\u{f42e}'; // nf-oct-check
const PR_CHANGES_ICON: char = '\u{f440}'; // nf-oct-diff (±)
const PR_COMMENT_ICON: char = '\u{f41f}'; // nf-oct-comment

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

/// Compact badge text for an open PR, e.g. ` #12 ✓ ` (approved with outside
/// comments). Review marker first (approved / changes-requested), then a
/// comment marker when a non-author has commented.
fn pr_badge_text(pr: &PrInfo) -> String {
    let mut s = format!("{} #{}", PR_BADGE_ICON, pr.number);
    match pr.review {
        ReviewState::Approved => {
            s.push(' ');
            s.push(PR_APPROVED_ICON);
        }
        ReviewState::ChangesRequested => {
            s.push(' ');
            s.push(PR_CHANGES_ICON);
        }
        ReviewState::None => {}
    }
    if pr.outside_activity {
        s.push(' ');
        s.push(PR_COMMENT_ICON);
    }
    s
}

/// Badge chip color: by CI status, falling back to the neutral badge blue.
fn pr_badge_color(pr: &PrInfo, theme: &Theme) -> Color {
    match pr.ci {
        CiStatus::None => theme.pr_badge,
        CiStatus::Pass => theme.pr_ci_pass,
        CiStatus::Pending => theme.pr_ci_pending,
        CiStatus::Fail => theme.pr_ci_fail,
    }
}

/// Nerd Font cloud glyph marking a branch that only exists on a remote.
pub const REMOTE_ONLY_ICON: &str = "\u{f0c2}"; //
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
            // Single-remote repos drop the "<remote>/" prefix (cloud conveys it).
            let display = chip_display_name(name, remotes);
            return vec![(make_label(&prefix, display, None), make_style(name))];
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
            // Remote branch: skip if matching local exists (dedup keeps a
            // stripped remote-only chip from colliding with its local twin).
            if local_branches.contains(bare) {
                continue;
            }
            if strip_prefix_in_labels(remotes) {
                // Single remote: drop the prefix but add the cloud icon so this
                // remote-only chip still reads as remote in a multi-branch row.
                let prefix = format!("{} ", REMOTE_ONLY_ICON);
                result.push((make_label(&prefix, bare, None), make_style(name)));
            } else {
                result.push((make_label("", name, None), make_style(name)));
            }
        } else {
            // Local branch: mark with the ↔ icon (same convention as the
            // single-branch synced chip) when a remote ref points at the same
            // bare name — dropping the redundant "↔ <remote>" text suffix.
            let has_synced_remote = branch_names
                .iter()
                .any(|other| strip_remote(other, remotes) == Some(name.as_str()));
            let prefix = if has_synced_remote {
                format!("{} ", SYNCED_ICON)
            } else {
                String::new()
            };
            result.push((make_label(&prefix, name, None), make_style(name)));
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

        // Helper: strip "[...]", a leading icon prefix, and any suffix to
        // recover the bare branch name.
        let clean = |label: &str| -> String {
            let s = label.trim_start_matches('[');
            // Drop a leading remote-only/synced icon (+ its trailing space) so a
            // stripped remote-only chip resolves to its name, not the glyph.
            let s = s
                .strip_prefix(REMOTE_ONLY_ICON)
                .or_else(|| s.strip_prefix(SYNCED_ICON))
                .map(str::trim_start)
                .unwrap_or(s);
            s.split([']', ' ']).next().unwrap_or(label).to_string()
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
///
/// Only columns enabled in `cols` are eligible. Among the eligible ones, when
/// the row is too narrow they drop by priority — hash first, then date, then
/// author (author is kept longest). A hidden/dropped column's width is not
/// counted, so it flows to the commit-message budget via `right_width`.
fn compute_right_side_visibility(
    remaining_for_content: usize,
    cols: MetadataColumns,
) -> (bool, bool, bool, usize) {
    // Rendered width of each element incl. its leading separator (see the
    // right-block rendering below): date " "+4, author "  "+8, hash "  "+7.
    const DATE_W: usize = 5;
    const AUTHOR_W: usize = 10;
    const HASH_W: usize = 9;
    const TRAILING_W: usize = 1; // single trailing space when anything shows

    // Ensure minimum space for branch + commit message before showing right-side info
    const CONTENT_MIN_WIDTH: usize = 50;
    let available = remaining_for_content.saturating_sub(CONTENT_MIN_WIDTH);

    let width = |d: bool, a: bool, h: bool| -> usize {
        if !d && !a && !h {
            return 0;
        }
        (if d { DATE_W } else { 0 })
            + (if a { AUTHOR_W } else { 0 })
            + (if h { HASH_W } else { 0 })
            + TRAILING_W
    };

    // Start from the user's enabled set, then shed low-priority columns until
    // the block fits. (With all three enabled this reproduces the original
    // 35/26/11 breakpoints exactly.)
    let mut show_date = cols.date;
    let mut show_author = cols.author;
    let mut show_hash = cols.hash;

    if width(show_date, show_author, show_hash) > available {
        show_hash = false;
    }
    if width(show_date, show_author, show_hash) > available {
        show_date = false;
    }
    if width(show_date, show_author, show_hash) > available {
        show_author = false;
    }

    let right_width = width(show_date, show_author, show_hash);
    (show_date, show_author, show_hash, right_width)
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
    graph_width: usize,
    is_selected: bool,
    is_marked: bool,
    total_width: usize,
    selected_branch_name: Option<&str>,
    theme: &Theme,
    now: DateTime<Local>,
    pixel_mode: bool,
    remotes: &[String],
    open_prs: &HashMap<String, PrInfo>,
    metadata_columns: MetadataColumns,
    trace: Option<&std::collections::HashSet<git2::Oid>>,
) -> (Line<'a>, Vec<ChipHit>) {
    let mut spans: Vec<Span> = Vec::new();

    // Graph start marker (to distinguish from borders). GRAPH_LEADING_COLUMNS
    // is the shared contract with the pixel overlay's x-offset.
    spans.push(Span::raw(" ".repeat(GRAPH_LEADING_COLUMNS as usize)));
    let mut left_width: usize = GRAPH_LEADING_COLUMNS as usize;

    // Pixel mode: the graph column is painted by an image overlay, so emit
    // blank space of the exact same width to keep the text layout identical —
    // plus the `…` marker (in the text layer) when the width cap truncates the
    // row, since the image can't draw it.
    if pixel_mode {
        let (budget, ellipsis) = graph_truncation(node.cells.len(), graph_width);
        for _ in 0..budget {
            spans.push(Span::raw(" "));
            left_width += 1;
        }
        if ellipsis {
            spans.push(Span::styled("…", ellipsis_style(theme)));
            left_width += 1;
        }
    } else {
        // Graph glyphs render bold; with tracing, non-lineage cells are dimmed.
        // Merge muting stays in the message text (see render_graph_line_tail).
        left_width =
            render_cells_unicode(&mut spans, node, theme, left_width, graph_width, trace);
    }

    // Padding to align to the (capped) graph width. Reclaimed width flows to the
    // message budget: the tail sizes the message from `total_width - left_width`.
    let graph_display_width = graph_width;
    if left_width < graph_display_width + 1 {
        // +1 accounts for the start marker
        let padding = graph_display_width + 1 - left_width;
        spans.push(Span::raw(" ".repeat(padding)));
        left_width += padding;
    }

    // Reserve blank columns for the author avatar (drawn by a separate image
    // overlay in pixel mode). The message tail then starts after them.
    if avatars_active(pixel_mode, metadata_columns) {
        spans.push(Span::raw(" ".repeat(AVATAR_RESERVED_CELLS as usize)));
        left_width += AVATAR_RESERVED_CELLS as usize;
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
        metadata_columns,
    )
}

/// Render the Unicode box-drawing glyphs for a row's cells into `spans`, capped
/// at `cap` graph columns, returning the updated `left_width`. When the row
/// overflows the cap, the last column becomes a dim `…`.
fn render_cells_unicode(
    spans: &mut Vec<Span<'_>>,
    node: &GraphNode,
    theme: &Theme,
    mut left_width: usize,
    cap: usize,
    trace: Option<&std::collections::HashSet<git2::Oid>>,
) -> usize {
    // `budget` graph columns are available for glyphs; when truncating, one more
    // column holds the `…`.
    let (budget, ellipsis) = graph_truncation(node.cells.len(), cap);

    // Whether the cell at `idx` should be dimmed: tracing active and this cell
    // is not on the selected commit's lineage.
    let is_dim = |idx: usize| -> bool {
        trace.is_some_and(|lineage| {
            let oids = node.cell_oids.get(idx).copied().unwrap_or((None, None));
            !crate::git::graph::cell_is_traced(oids, lineage)
        })
    };

    // Render cells.
    //
    // The HEAD commit is drawn with a width-2 star emoji so it stands out. To
    // keep column alignment intact, the emoji occupies the commit cell *and*
    // the cell to its right — but only when that right cell carries no graph
    // line (Empty or a plain Horizontal connector). If a pipe or junction sits
    // there, painting over it would corrupt the graph, so we fall back to the
    // width-1 glyph instead.
    let mut cols = 0usize; // graph columns emitted so far
    let mut idx = 0;
    while idx < node.cells.len() {
        let cell = &node.cells[idx];

        // Special-case the HEAD commit star (may consume the next cell).
        if node.is_head {
            if let CellType::Commit(_) = cell {
                // The ◉ fallback uses the HEAD-star gold, matching the ⭐ / the
                // pixel renderer's star.
                let mut style = Style::default()
                    .fg(theme.head_star)
                    .add_modifier(Modifier::BOLD);
                if is_dim(idx) {
                    style = style.add_modifier(Modifier::DIM);
                }

                let right_is_clear = matches!(
                    node.cells.get(idx + 1),
                    None | Some(CellType::Empty) | Some(CellType::Horizontal(_))
                );
                // ⭐ is width-2 and spans two cells; ◉ is the width-1 fallback.
                let (glyph, gw, consumed) = if right_is_clear {
                    ("⭐", 2usize, 2usize)
                } else {
                    ("◉", 1usize, 1usize)
                };
                if cols + gw > budget {
                    // Would overflow the cap — and a width-2 star can't be halved
                    // at the boundary. Stop; the ellipsis padding below fills in.
                    break;
                }
                spans.push(Span::styled(glyph, style));
                cols += gw;
                left_width += gw;
                idx += consumed;
                continue;
            }
        }

        if cols + 1 > budget {
            break;
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

        // Line glyphs render bold; non-lineage cells dim while tracing.
        let mut style = Style::default().fg(color).add_modifier(Modifier::BOLD);
        if is_dim(idx) {
            style = style.add_modifier(Modifier::DIM);
        }

        let ch_str = ch.to_string();
        let ch_width = display_width(&ch_str);
        spans.push(Span::styled(ch_str, style));
        cols += ch_width;
        left_width += ch_width;
        idx += 1;
    }

    if ellipsis {
        // Fill any gap a width-2 star left at the boundary, then the marker.
        while cols < budget {
            spans.push(Span::raw(" "));
            cols += 1;
            left_width += 1;
        }
        spans.push(Span::styled("…", ellipsis_style(theme)));
        left_width += 1;
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
    metadata_columns: MetadataColumns,
) -> (Line<'a>, Vec<ChipHit>) {
    let mut chips: Vec<ChipHit> = Vec::new();
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
        return (Line::from(spans), chips);
    }

    // Early return for connector-only rows
    let commit = match &node.commit {
        Some(c) => c,
        None => return (Line::from(spans), chips),
    };

    // Style definitions
    let hash_style = Style::default().fg(theme.hash_color);
    let author_style = Style::default().fg(theme.author_color);
    let date_style = Style::default().fg(theme.date_color);
    // Muted merge: dim only the message text (VSCode-style) — the graph dot and
    // lines stay full-strength. HEAD is never muted so its message stays legible.
    let muted_merge = metadata_columns.mute_merges && node.is_merge() && !node.is_head;
    let msg_style = if muted_merge {
        Style::default().add_modifier(Modifier::DIM)
    } else if is_selected {
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
    // branches has an open PR. Colored by CI status, with review/comment markers.
    let pr_badge = pr_for_branch_labels(&node.branch_names, remotes, open_prs)
        .map(|pr| (pr_badge_text(pr), pr_badge_color(pr, theme)));
    // Chip plus a trailing space.
    let pr_badge_width = pr_badge.as_ref().map_or(0, |(b, _)| display_width(b) + 1);

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
        compute_right_side_visibility(remaining_for_content, metadata_columns);

    // Render branch labels
    for (i, (label, style)) in branch_display.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
            left_width += 1;
        }
        let chip_start = left_width;
        left_width += display_width(label);
        // Record a clickable region resolving to the underlying branch, so a
        // click on the chip can check that branch out.
        if let Some(name) = resolve_chip_branch(label, &node.branch_names, remotes) {
            chips.push(ChipHit {
                x_start: chip_start as u16,
                x_end: left_width as u16,
                target: ChipTarget::Branch(name),
            });
        }
        spans.push(Span::styled(label.clone(), *style));
    }
    if !branch_display.is_empty() {
        spans.push(Span::raw(" "));
        left_width += 1;
    }

    // Render open-PR badge (after branch labels, before tags)
    if let Some((badge, color)) = &pr_badge {
        let style = Style::default().fg(*color).add_modifier(Modifier::BOLD);
        let chip_start = left_width;
        left_width += display_width(badge) + 1;
        chips.push(ChipHit {
            x_start: chip_start as u16,
            x_end: (chip_start + display_width(badge)) as u16,
            target: ChipTarget::PrBadge,
        });
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

    (Line::from(spans), chips)
}

/// Recover the branch name a rendered chip `label` refers to, matching it
/// against the node's `branch_names`. Chip labels are decorated (`[name]`, an
/// optional remote/synced icon prefix, a possible ` +N` overflow suffix), so we
/// strip the decoration to a bare name and find the branch whose bare form
/// matches (a local branch, or a remote ref bare-equal to it). Returns `None`
/// when nothing matches (e.g. a non-branch decoration).
fn resolve_chip_branch(label: &str, branch_names: &[String], remotes: &[String]) -> Option<String> {
    // Strip the leading '[' and any icon prefix, then take up to the first
    // delimiter (']', ' ', or the start of a "+N" overflow marker).
    let s = label.trim_start_matches('[');
    let s = s
        .strip_prefix(REMOTE_ONLY_ICON)
        .or_else(|| s.strip_prefix(SYNCED_ICON))
        .map(str::trim_start)
        .unwrap_or(s);
    let bare = s.split([']', ' ']).next().unwrap_or(s);
    if bare.is_empty() {
        return None;
    }
    // Exact local match first, then a remote ref whose bare name matches.
    branch_names
        .iter()
        .find(|n| n.as_str() == bare)
        .or_else(|| {
            branch_names
                .iter()
                .find(|n| strip_remote(n, remotes) == Some(bare))
        })
        .cloned()
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

        // Seed from last frame's offset (stored back below) so the viewport
        // scrolls incrementally instead of being re-derived from row 0, which
        // pinned the selection to the bottom edge. Ratatui clamps a stale
        // offset if the item count shrank.
        let mut filtered_state = ListState::default().with_offset(state.offset());
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
        let future = now + Duration::seconds(300);
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
        // Single-remote repo: the "origin/" prefix is dropped (cloud conveys it).
        let out = labels(&["origin/feature"], &["origin"]);
        assert_eq!(out, vec![format!("[{} feature]", REMOTE_ONLY_ICON)]);
    }

    #[test]
    fn lone_remote_ref_respects_non_origin_remotes() {
        // Multi-remote repo: the prefix is kept to disambiguate the remote.
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

    // ── chip click resolution (resolve_chip_branch) ──────────────────────

    #[test]
    fn resolve_chip_branch_recovers_the_branch_name() {
        let remotes = vec!["origin".to_string()];
        let names = vec!["main".to_string(), "feature/x".to_string()];
        // Plain local label.
        assert_eq!(
            resolve_chip_branch("[main]", &names, &remotes).as_deref(),
            Some("main")
        );
        // Label with a slash in the name.
        assert_eq!(
            resolve_chip_branch("[feature/x]", &names, &remotes).as_deref(),
            Some("feature/x")
        );
        // Synced-icon prefix is stripped before matching.
        let synced = format!("[{} main]", SYNCED_ICON);
        assert_eq!(
            resolve_chip_branch(&synced, &names, &remotes).as_deref(),
            Some("main")
        );
        // A cloud-icon remote-only chip (single-remote repo drops the prefix)
        // resolves back to the full remote ref.
        let remote_names = vec!["origin/dev".to_string()];
        let cloud = format!("[{} dev]", REMOTE_ONLY_ICON);
        assert_eq!(
            resolve_chip_branch(&cloud, &remote_names, &remotes).as_deref(),
            Some("origin/dev")
        );
        // No matching branch → None.
        assert_eq!(resolve_chip_branch("[nope]", &names, &remotes), None);
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
            ci: CiStatus::None,
            review: ReviewState::None,
            outside_activity: false,
        }
    }

    fn pr_with(number: u64, ci: CiStatus, review: ReviewState, outside: bool) -> PrInfo {
        PrInfo {
            number,
            url: "u".to_string(),
            title: "t".to_string(),
            ci,
            review,
            outside_activity: outside,
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

    #[test]
    fn pr_badge_appends_review_then_comment_markers() {
        // Plain: no markers.
        let plain = pr_badge_text(&pr_with(1, CiStatus::Pass, ReviewState::None, false));
        assert!(!plain.contains(PR_APPROVED_ICON));
        assert!(!plain.contains(PR_COMMENT_ICON));

        // Approved → check glyph; changes-requested → diff glyph (mutually exclusive).
        let approved = pr_badge_text(&pr_with(1, CiStatus::Pass, ReviewState::Approved, false));
        assert!(approved.contains(PR_APPROVED_ICON));
        assert!(!approved.contains(PR_CHANGES_ICON));
        let changes =
            pr_badge_text(&pr_with(1, CiStatus::Fail, ReviewState::ChangesRequested, false));
        assert!(changes.contains(PR_CHANGES_ICON));
        assert!(!changes.contains(PR_APPROVED_ICON));

        // Outside comment → comment glyph, appended after the review marker.
        let both = pr_badge_text(&pr_with(12, CiStatus::Pass, ReviewState::Approved, true));
        assert!(both.contains(PR_APPROVED_ICON) && both.contains(PR_COMMENT_ICON));
        let check_at = both.find(PR_APPROVED_ICON).unwrap();
        let comment_at = both.find(PR_COMMENT_ICON).unwrap();
        assert!(check_at < comment_at, "review marker precedes comment: {both:?}");
        // Icon(1)+" #12"(4) + " ✓"(2) + " ⌘"(2) = 9 columns.
        assert_eq!(display_width(&both), 9);
    }

    #[test]
    fn pr_badge_color_follows_ci_status() {
        let theme = Theme::dark();
        assert_eq!(pr_badge_color(&pr_with(1, CiStatus::None, ReviewState::None, false), &theme), theme.pr_badge);
        assert_eq!(pr_badge_color(&pr_with(1, CiStatus::Pass, ReviewState::None, false), &theme), theme.pr_ci_pass);
        assert_eq!(pr_badge_color(&pr_with(1, CiStatus::Pending, ReviewState::None, false), &theme), theme.pr_ci_pending);
        assert_eq!(pr_badge_color(&pr_with(1, CiStatus::Fail, ReviewState::None, false), &theme), theme.pr_ci_fail);
    }

    // ── metadata column visibility / width budget ────────────────────

    fn cols(author: bool, hash: bool, date: bool) -> MetadataColumns {
        MetadataColumns {
            author,
            hash,
            date,
            mute_merges: true,
            avatars: false,
        }
    }

    #[test]
    fn all_columns_shown_when_enabled_and_wide() {
        // Compact date (4) + separators: date 5 + author 10 + hash 9 + trailing 1 = 25.
        assert_eq!(
            compute_right_side_visibility(200, cols(true, true, true)),
            (true, true, true, 25)
        );
    }

    #[test]
    fn disabled_column_is_never_shown_and_reclaims_its_width() {
        // Hash off: only date+author, width drops from 25 to 16 (9 reclaimed).
        assert_eq!(
            compute_right_side_visibility(200, cols(true, false, true)),
            (true, true, false, 16)
        );
        // Everything off: no right block at all.
        assert_eq!(
            compute_right_side_visibility(200, cols(false, false, false)),
            (false, false, false, 0)
        );
        // Only date enabled: date " "+4 = 5, +1 trailing = 6.
        assert_eq!(
            compute_right_side_visibility(200, cols(false, false, true)),
            (true, false, false, 6)
        );
    }

    #[test]
    fn enabled_columns_still_drop_by_priority_when_narrow() {
        // available = remaining - 50. Hash drops first, then date, author last.
        // all (25): avail >= 25 → remaining >= 75.
        assert_eq!(
            compute_right_side_visibility(75, cols(true, true, true)),
            (true, true, true, 25)
        );
        // date+author (16): 16 <= avail < 25 → remaining 66..74.
        assert_eq!(
            compute_right_side_visibility(74, cols(true, true, true)),
            (true, true, false, 16)
        );
        // author only (11): 11 <= avail < 16 → remaining 61..65.
        assert_eq!(
            compute_right_side_visibility(65, cols(true, true, true)),
            (false, true, false, 11)
        );
        // none (0): avail < 11 → remaining < 61.
        assert_eq!(
            compute_right_side_visibility(60, cols(true, true, true)),
            (false, false, false, 0)
        );
    }

    // ── single-remote prefix stripping on labels ─────────────────────

    #[test]
    fn single_remote_strips_prefix_and_keeps_cloud_icon() {
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &["origin/feat".to_string()],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        assert_eq!(out.len(), 1);
        let label = &out[0].0;
        assert!(label.contains(REMOTE_ONLY_ICON), "cloud icon kept: {label:?}");
        assert!(label.contains("feat"));
        assert!(!label.contains("origin/"), "prefix stripped: {label:?}");
    }

    #[test]
    fn upstream_only_remote_strips_prefix() {
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &["upstream/main".to_string()],
            false,
            0,
            None,
            &theme,
            &["upstream".to_string()],
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].0.contains("main"));
        assert!(!out[0].0.contains("upstream/"), "{:?}", out[0].0);
    }

    #[test]
    fn multi_remote_keeps_prefix() {
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &["origin/feat".to_string()],
            false,
            0,
            None,
            &theme,
            &["origin".to_string(), "upstream".to_string()],
        );
        assert_eq!(out.len(), 1);
        assert!(
            out[0].0.contains("origin/feat"),
            "multi-remote keeps prefix: {:?}",
            out[0].0
        );
    }

    #[test]
    fn single_remote_synced_pair_still_collapses() {
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &["main".to_string(), "origin/main".to_string()],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        // The local+remote pair collapses to one ↔ chip — no duplicate [main].
        assert_eq!(out.len(), 1);
        assert!(out[0].0.contains("main"));
        assert!(out[0].0.contains(SYNCED_ICON), "synced icon: {:?}", out[0].0);
        assert!(!out[0].0.contains("origin/"));
    }

    #[test]
    fn single_remote_multi_branch_dedups_without_duplicate_or_prefix() {
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &[
                "foo".to_string(),
                "origin/foo".to_string(),
                "bar".to_string(),
            ],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        assert_eq!(out.len(), 1);
        let label = &out[0].0;
        // origin/foo is the remote twin of local foo → deduped, no duplicate and
        // no leftover prefix; two real branches remain, so no "+N".
        assert!(!label.contains("origin/"), "{label:?}");
        assert!(!label.contains('+'), "no overflow marker: {label:?}");
        assert!(label.contains("foo"));
        assert!(label.contains("bar"));
    }

    #[test]
    fn single_remote_remote_only_chip_in_multi_branch_resolves_to_name() {
        let theme = Theme::dark();
        // A remote-only ref alongside a local branch: the combined label must
        // show the stripped name, never the bare cloud glyph.
        let out = optimize_branch_display(
            &["origin/lonely".to_string(), "bar".to_string()],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        assert_eq!(out.len(), 1);
        let label = &out[0].0;
        assert!(label.contains("lonely"), "stripped name present: {label:?}");
        assert!(!label.contains("origin/"), "{label:?}");
    }

    // ── graph width cap arithmetic ───────────────────────────────────

    #[test]
    fn effective_graph_width_clamps_and_honours_uncapped() {
        assert_eq!(effective_graph_width(10, None), 10, "None = uncapped");
        assert_eq!(effective_graph_width(10, Some(6)), 6);
        assert_eq!(effective_graph_width(10, Some(2)), 4, "floor at 4");
        assert_eq!(effective_graph_width(10, Some(100)), 10, "cap >= needed = uncapped");
        // Graph too small to cap: floor collapses to needed.
        assert_eq!(effective_graph_width(2, Some(6)), 2);
        assert_eq!(effective_graph_width(2, None), 2);
    }

    #[test]
    fn next_graph_cap_steps_by_two_and_uncaps_past_needed() {
        // Shrink from uncapped caps at needed-2.
        assert_eq!(next_graph_cap(10, None, -1), Some(8));
        assert_eq!(next_graph_cap(10, Some(8), -1), Some(6));
        // Floor at 4.
        assert_eq!(next_graph_cap(10, Some(4), -1), Some(4));
        // Widen loosens; reaching needed uncaps.
        assert_eq!(next_graph_cap(10, Some(4), 1), Some(6));
        assert_eq!(next_graph_cap(10, Some(8), 1), None);
        assert_eq!(next_graph_cap(10, None, 1), None);
        // A stale cap wider than needed resets on shrink, uncaps on widen.
        assert_eq!(next_graph_cap(10, Some(100), -1), Some(8));
        assert_eq!(next_graph_cap(10, Some(100), 1), None);
        // Graph too small to cap stays uncapped.
        assert_eq!(next_graph_cap(2, None, -1), None);
    }

    #[test]
    fn graph_truncation_reserves_a_column_for_the_marker() {
        assert_eq!(graph_truncation(8, 6), (5, true));
        assert_eq!(graph_truncation(6, 6), (6, false));
        assert_eq!(graph_truncation(3, 6), (3, false));
    }

    #[test]
    fn pixel_row_cells_is_min_of_cap_budget_and_panel() {
        // Cap truncates (8 cells, width 6 → budget 5), panel wide.
        assert_eq!(pixel_row_cells(8, 6, 100), 5);
        // Uncapped and fits.
        assert_eq!(pixel_row_cells(8, 100, 100), 8);
        // Panel narrower than the cap budget bounds it further.
        assert_eq!(pixel_row_cells(8, 100, 3), 3);
        assert_eq!(pixel_row_cells(8, 6, 3), 3);
        // The image cell count depends on graph_width, so the pixel spec cache
        // must key on it (different caps ⇒ different specs).
        assert_ne!(pixel_row_cells(8, 6, 100), pixel_row_cells(8, 8, 100));
    }

    // ── unicode cell clamping ────────────────────────────────────────

    fn node_with_cells(cells: Vec<CellType>, is_head: bool) -> GraphNode {
        GraphNode {
            commit: None,
            lane: 0,
            color_index: 0,
            branch_names: Vec::new(),
            tag_names: Vec::new(),
            is_head,
            is_uncommitted: false,
            is_stash: false,
            stash_label: None,
            uncommitted_count: None,
            cells,
            cell_oids: Vec::new(),
        }
    }

    /// Render just the graph cells at `cap` and return the emitted text.
    fn render_cells(node: &GraphNode, cap: usize) -> String {
        let theme = Theme::dark();
        let mut spans: Vec<Span> = Vec::new();
        let lw = render_cells_unicode(&mut spans, node, &theme, 0, cap, None);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        // left_width is columns emitted (started at 0); it must equal the text's
        // display width.
        assert_eq!(lw, display_width(&text), "left_width tracks display width");
        text
    }

    #[test]
    fn unicode_cells_clamp_to_cap_with_ellipsis() {
        // Eight pipes, capped at 6 → 5 glyphs + a dim …, exactly 6 columns.
        let node = node_with_cells(vec![CellType::Pipe(0); 8], false);
        let text = render_cells(&node, 6);
        assert_eq!(display_width(&text), 6);
        assert!(text.ends_with('…'), "truncation marker present: {text:?}");
        assert_eq!(text.chars().filter(|c| *c == '│').count(), 5);
    }

    #[test]
    fn unicode_cells_untruncated_when_within_cap() {
        let node = node_with_cells(vec![CellType::Pipe(0); 4], false);
        let text = render_cells(&node, 8);
        assert_eq!(display_width(&text), 4);
        assert!(!text.contains('…'));
    }

    #[test]
    fn head_star_fits_when_it_lands_before_the_boundary() {
        // Star (width-2) + one pipe fit in budget 3 (cap 4), then ….
        let node = node_with_cells(
            vec![
                CellType::Commit(0),
                CellType::Empty,
                CellType::Pipe(0),
                CellType::Pipe(0),
                CellType::Pipe(0),
                CellType::Pipe(0),
            ],
            true,
        );
        let text = render_cells(&node, 4);
        assert_eq!(display_width(&text), 4);
        assert!(text.contains('⭐'));
        assert!(text.ends_with('…'));
    }

    #[test]
    fn head_star_at_boundary_emits_no_broken_glyph() {
        // Two pipes then the head star: budget 3 (cap 4) leaves room for only
        // one more column, so the width-2 star is dropped (a space fills the
        // gap) rather than emitting half a glyph.
        let node = node_with_cells(
            vec![
                CellType::Pipe(0),
                CellType::Pipe(0),
                CellType::Commit(0),
                CellType::Empty,
                CellType::Pipe(0),
                CellType::Pipe(0),
            ],
            true,
        );
        let text = render_cells(&node, 4);
        assert_eq!(display_width(&text), 4, "exactly cap wide: {text:?}");
        assert!(!text.contains('⭐'), "no half star: {text:?}");
        assert!(text.ends_with('…'));
    }

    // ── muted merges dim only the message text ───────────────────────

    fn merge_node(message: &str) -> GraphNode {
        use crate::git::CommitInfo;
        let commit = CommitInfo {
            oid: git2::Oid::zero(),
            short_id: "abc1234".to_string(),
            author_name: "a".to_string(),
            author_email: "a@b".to_string(),
            timestamp: Local::now(),
            message: message.to_string(),
            full_message: message.to_string(),
            parent_oids: vec![git2::Oid::zero(); 2], // 2 parents => a merge
        };
        GraphNode {
            commit: Some(commit),
            lane: 0,
            color_index: 0,
            branch_names: Vec::new(),
            tag_names: Vec::new(),
            is_head: false,
            is_uncommitted: false,
            is_stash: false,
            stash_label: None,
            uncommitted_count: None,
            cells: vec![CellType::Commit(0)],
            cell_oids: Vec::new(),
        }
    }

    /// The style applied to `node`'s message span when rendered with the given
    /// mute-merges setting. Panics if the message span isn't found.
    fn message_style(node: &GraphNode, message: &str, mute_merges: bool) -> Style {
        let theme = Theme::dark();
        let cols = MetadataColumns {
            author: false,
            hash: false,
            date: false,
            mute_merges,
            avatars: false,
        };
        let (line, _chips) = render_graph_line(
            node,
            4,
            false,
            false,
            200,
            None,
            &theme,
            Local::now(),
            false,
            &[],
            &HashMap::new(),
            cols,
            None,
        );
        line.spans
            .iter()
            .find(|s| s.content.as_ref() == message)
            .unwrap_or_else(|| panic!("message span not found in {:?}", line))
            .style
    }

    #[test]
    fn muted_merge_dims_message_text_not_the_graph() {
        let node = merge_node("merge-branch-into-main");
        // Toggle ON: the merge's message text is dimmed.
        let muted = message_style(&node, "merge-branch-into-main", true);
        assert!(
            muted.add_modifier.contains(Modifier::DIM),
            "muted merge message should be DIM: {muted:?}"
        );
        // Toggle OFF: the message renders at full strength.
        let normal = message_style(&node, "merge-branch-into-main", false);
        assert!(
            !normal.add_modifier.contains(Modifier::DIM),
            "un-muted merge message must not be DIM: {normal:?}"
        );
    }

    #[test]
    fn head_merge_is_never_muted() {
        let mut node = merge_node("head-merge");
        node.is_head = true;
        // Even with muting on, the HEAD commit's message stays legible.
        let style = message_style(&node, "head-merge", true);
        assert!(!style.add_modifier.contains(Modifier::DIM));
    }

    // ── connector folding (pixel mode) ───────────────────────────────

    fn connector_node(cells: Vec<CellType>) -> GraphNode {
        // commit: None + not uncommitted => a connector row.
        node_with_cells(cells, false)
    }

    fn commit_row(cells: Vec<CellType>) -> GraphNode {
        let mut n = merge_node("m");
        n.cells = cells;
        n
    }

    #[test]
    fn unicode_mode_keeps_connector_rows_as_their_own_rows() {
        let a = commit_row(vec![CellType::Commit(0)]);
        let conn = connector_node(vec![CellType::TeeRight(0)]);
        let b = commit_row(vec![CellType::Commit(0)]);
        let base = vec![(0, &a), (1, &conn), (2, &b)];
        let rows = fold_rows(base, false);
        // All three nodes remain, each with an empty underlay.
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.underlay.is_empty()));
        assert_eq!(rows.iter().map(|r| r.full_idx).collect::<Vec<_>>(), [0, 1, 2]);
    }

    #[test]
    fn pixel_mode_folds_connector_into_the_following_commit_row() {
        let a = commit_row(vec![CellType::Commit(0)]);
        let conn = connector_node(vec![CellType::TeeRight(0), CellType::MergeLeft(1)]);
        let b = commit_row(vec![CellType::Commit(0)]);
        let base = vec![(0, &a), (1, &conn), (2, &b)];
        let rows = fold_rows(base, true);

        // N_commits rows from N_commits + N_connectors nodes.
        assert_eq!(rows.len(), 2, "connector row is folded away");
        // The commit rows keep their real graph indices (selection alignment).
        assert_eq!(rows[0].full_idx, 0);
        assert_eq!(rows[1].full_idx, 2);
        // Row 0 has no preceding connector; row 1 carries the folded connector.
        assert!(rows[0].underlay.is_empty());
        assert_eq!(rows[1].underlay, vec![CellType::TeeRight(0), CellType::MergeLeft(1)]);
    }

    #[test]
    fn folded_index_space_maps_selection_to_the_right_row() {
        // Select the second commit (full_idx 2). After folding, it's row 1.
        let a = commit_row(vec![CellType::Commit(0)]);
        let conn = connector_node(vec![CellType::TeeRight(0)]);
        let b = commit_row(vec![CellType::Commit(0)]);
        let base = vec![(0, &a), (1, &conn), (2, &b)];
        let rows = fold_rows(base, true);
        let selected_full = 2;
        let filtered_pos = rows.iter().position(|r| r.full_idx == selected_full);
        assert_eq!(filtered_pos, Some(1), "selected commit maps to folded row 1");
    }

    #[test]
    fn multiple_consecutive_connectors_all_fold_into_the_next_commit() {
        let c1 = connector_node(vec![CellType::TeeRight(0), CellType::Empty]);
        let c2 = connector_node(vec![CellType::Empty, CellType::MergeLeft(1)]);
        let b = commit_row(vec![CellType::Commit(0)]);
        let base = vec![(0, &c1), (1, &c2), (2, &b)];
        let rows = fold_rows(base, true);
        assert_eq!(rows.len(), 1);
        // Per-column merge of both connectors.
        assert_eq!(
            rows[0].underlay,
            vec![CellType::TeeRight(0), CellType::MergeLeft(1)]
        );
    }

    #[test]
    fn trailing_connector_without_a_commit_renders_standalone() {
        let a = commit_row(vec![CellType::Commit(0)]);
        let conn = connector_node(vec![CellType::TeeRight(0)]);
        let base = vec![(0, &a), (1, &conn)];
        let rows = fold_rows(base, true);
        // The commit row, plus the dangling connector as its own row.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].full_idx, 1);
        assert!(rows[1].underlay.is_empty());
    }

    #[test]
    fn adjacent_cells_prefers_folded_underlay_over_the_neighbour() {
        // Row 1 folds a connector whose column 0 is a TeeRight (touches bottom).
        // Its "above" cells should read the underlay, not row 0's node cells.
        let a = commit_row(vec![CellType::Empty]);
        let b = commit_row(vec![CellType::Commit(0)]);
        let rows = vec![
            RenderRow { full_idx: 0, node: &a, underlay: Vec::new(), underlay_oids: Vec::new() },
            RenderRow {
                full_idx: 2,
                node: &b,
                underlay: vec![CellType::TeeRight(0)],
                underlay_oids: Vec::new(),
            },
        ];
        let above = adjacent_cells(&rows, 1, true).unwrap();
        assert_eq!(above[0], CellType::TeeRight(0), "underlay wins over neighbour");
    }

    // ── branch tracing: Unicode dim + fold + truncation ──────────────────

    fn oid(b: u8) -> git2::Oid {
        git2::Oid::from_bytes(&[b; 20]).unwrap()
    }

    #[test]
    fn tracing_dims_non_lineage_cells_in_unicode() {
        use std::collections::HashSet;
        let theme = Theme::dark();
        let (a, b) = (oid(1), oid(2));
        let mut node = node_with_cells(vec![CellType::Commit(0), CellType::Pipe(1)], false);
        node.cell_oids = vec![(Some(a), None), (Some(b), None)];
        let lineage: HashSet<git2::Oid> = [a].into_iter().collect();

        let mut spans: Vec<Span> = Vec::new();
        render_cells_unicode(&mut spans, &node, &theme, 0, 8, Some(&lineage));

        let commit = spans.iter().find(|s| s.content.contains('●')).unwrap();
        let pipe = spans.iter().find(|s| s.content.contains('│')).unwrap();
        assert!(
            !commit.style.add_modifier.contains(Modifier::DIM),
            "the lineage commit stays at full strength"
        );
        assert!(
            pipe.style.add_modifier.contains(Modifier::DIM),
            "the non-lineage pipe is dimmed"
        );
    }

    #[test]
    fn tracing_off_dims_nothing_in_unicode() {
        let theme = Theme::dark();
        let mut node = node_with_cells(vec![CellType::Commit(0), CellType::Pipe(1)], false);
        node.cell_oids = vec![(Some(oid(1)), None), (Some(oid(2)), None)];
        let mut spans: Vec<Span> = Vec::new();
        render_cells_unicode(&mut spans, &node, &theme, 0, 8, None);
        assert!(spans
            .iter()
            .all(|s| !s.style.add_modifier.contains(Modifier::DIM)));
    }

    #[test]
    fn folding_carries_connector_cell_oids_into_the_underlay() {
        let a = oid(7);
        let mut connector = connector_node(vec![CellType::Empty, CellType::TeeRight(0)]);
        connector.cell_oids = vec![(None, None), (Some(a), None)];
        let commit = commit_row(vec![CellType::Commit(0), CellType::Empty]);
        let base = vec![(0usize, &connector), (1usize, &commit)];

        let rows = fold_rows(base, true);
        assert_eq!(rows.len(), 1, "the connector folds into the commit row");
        assert_eq!(
            rows[0].underlay_oids.get(1).copied(),
            Some((Some(a), None)),
            "the connector's edge OID is preserved in the folded underlay"
        );
    }

    #[test]
    fn trace_dim_survives_width_truncation_in_unicode() {
        use std::collections::HashSet;
        let theme = Theme::dark();
        let a = oid(1);
        // Four cells, but a cap that only fits some: the mask must stay aligned
        // for the rendered cells and simply drop the truncated ones.
        let mut node = node_with_cells(
            vec![
                CellType::Commit(0),
                CellType::Pipe(1),
                CellType::Pipe(1),
                CellType::Pipe(1),
            ],
            false,
        );
        node.cell_oids = vec![
            (Some(a), None),
            (Some(oid(2)), None),
            (Some(oid(3)), None),
            (Some(oid(4)), None),
        ];
        let lineage: HashSet<git2::Oid> = [a].into_iter().collect();

        let mut spans: Vec<Span> = Vec::new();
        // cap = 3 leaves room for 2 glyphs plus the `…` marker.
        render_cells_unicode(&mut spans, &node, &theme, 0, 3, Some(&lineage));

        let commit = spans.iter().find(|s| s.content.contains('●')).unwrap();
        assert!(!commit.style.add_modifier.contains(Modifier::DIM));
        // The rendered pipe (col 1) is non-lineage → dimmed; nothing panicked on
        // the truncated columns.
        let pipe = spans.iter().find(|s| s.content.contains('│')).unwrap();
        assert!(pipe.style.add_modifier.contains(Modifier::DIM));
        // The `…` marker means the row was truncated within the cap.
        assert!(spans.iter().any(|s| s.content.contains('…')));
    }
}
