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

use std::collections::{HashMap, HashSet};

use crate::{
    app::App,
    config::MetadataColumns,
    git::graph::{CellType, GraphNode},
    mouse::{ChipHit, ChipTarget},
    pr::{CiStatus, PrContext, PrInfo, ReviewState},
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
    // The whole (folded) range: identical to the pre-windowing behavior.
    fold_rows_windowed(base.into_iter(), 0, usize::MAX)
}

/// Fold connectors into their following commit row, but materialize only the
/// folded rows whose index falls in `[win_start, win_end)`. The walk still
/// advances the folded index across every visible node (folding is sequential —
/// a windowed commit needs the connectors that precede it), but it allocates a
/// `RenderRow` (and its per-connector underlay) ONLY for windowed rows and stops
/// once the window is passed. On a scrolled multi-thousand-commit graph this is
/// O(window) allocations per redraw instead of O(total commits) — see #73.
///
/// Rows are dense in folded-index space, so the returned `Vec`'s element `k` is
/// folded row `win_start + k`. Passing `(0, usize::MAX)` yields every row, in the
/// exact order and content the old full fold produced.
fn fold_rows_windowed<'a>(
    base: impl Iterator<Item = (usize, &'a GraphNode)>,
    win_start: usize,
    win_end: usize,
) -> Vec<RenderRow<'a>> {
    let mut rows: Vec<RenderRow<'a>> = Vec::new();
    let mut pending: Vec<(usize, &GraphNode)> = Vec::new();
    let mut folded = 0usize;
    for (full_idx, node) in base {
        // Everything from here on has a folded index >= win_end.
        if folded >= win_end {
            return rows;
        }
        if node.is_connector() {
            pending.push((full_idx, node));
            continue;
        }
        if folded >= win_start {
            let (underlay, underlay_oids) = merge_connector_cells(&pending);
            rows.push(RenderRow {
                full_idx,
                node,
                underlay,
                underlay_oids,
            });
        }
        pending.clear();
        folded += 1;
    }
    // Trailing connectors with no following commit: render standalone.
    for (full_idx, node) in pending {
        if folded >= win_end {
            break;
        }
        if folded >= win_start {
            rows.push(RenderRow {
                full_idx,
                node,
                underlay: Vec::new(),
                underlay_oids: Vec::new(),
            });
        }
        folded += 1;
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
pub fn build_pixel_base_specs(
    app: &App,
    theme: &Theme,
    graph_width: usize,
    panel_available: usize,
) -> Vec<crate::ui::graph_pixels::RowSpec> {
    use crate::ui::graph_pixels::build_row_spec;
    // Fold connector rows into their following commit row (pixel-only): one spec
    // per rendered row, in the same order and index space as the list items.
    // These base specs carry NO per-frame dimming — neither branch-trace dimming
    // nor base-update force-dim (#55). Both are per-cell overlays independent of
    // the (expensive) curve geometry, so they are applied lazily to just the
    // on-screen window by `dim_pixel_specs_window`. Keeping the base geometry
    // trace-, mute-toggle- and scroll-independent lets it stay cached while the
    // selection moves, instead of rebuilding every row on every keypress.
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

/// Apply the two per-frame dim sources to the pixel specs in `[win_start,
/// win_end)` only, returning a full-length spec list (rows outside the window
/// are cheap empty placeholders). Dimming recolours/dims per cell but never
/// changes a cell's geometry, so it can be layered onto the cached, undimmed
/// `base` specs without rebuilding any curves.
///
/// Two independent sources feed the dim, mirroring the unicode renderer's
/// `force_dim || trace` rule:
/// - **base-update force-dim (#55)** — a back-merge row's whole connector is
///   dimmed so the noisy line recedes. `lineage`-independent; driven by
///   `app.metadata_columns.mute_base_merges` + `app.merged.base_update.value()`.
/// - **branch-trace dim** — when `lineage` is `Some`, cells not on the selected
///   commit's traced lineage fade. `None` when tracing is inactive.
///
/// Base-update force-dim wins over trace for a row (it dims everything), exactly
/// as the unicode path OR-s the two together.
///
/// The window must equal `graph_pixels::protocol_window(offset, viewport, len)`:
/// only those rows are rasterized (`sync_frame`) and only the on-screen subset
/// `[offset, offset + inner_h)` — always inside the window — is drawn
/// (`overlay_pixel_graph`), so the placeholders are never rasterized or shown.
/// Result is byte-identical to dimming every row and slicing the same window.
pub fn dim_pixel_specs_window(
    app: &App,
    theme: &Theme,
    base: &[crate::ui::graph_pixels::RowSpec],
    lineage: Option<&std::collections::HashSet<git2::Oid>>,
    win_start: usize,
    win_end: usize,
) -> Vec<crate::ui::graph_pixels::RowSpec> {
    // Trace dimming only applies when a lineage is active; otherwise `lit` is
    // empty and no cell is dimmed by tracing (base-update force-dim still runs).
    let lit = lineage
        .map(|l| crate::git::graph::trace_lit_edges(&app.graph_layout, l))
        .unwrap_or_default();
    // Commit → its lane color, for recoloring lit strokes by the commit the
    // lit-edge relation picked (see apply_trace_dim). Only lineage commits are
    // ever looked up (via `lit`), so restrict the map to them: on a big graph a
    // full-node map is a needless per-redraw allocation (#73). Empty when
    // tracing is off (base-update-only dim never recolors).
    let lane_rgb: std::collections::HashMap<git2::Oid, [u8; 3]> = match lineage {
        Some(lineage) => app
            .graph_layout
            .nodes
            .iter()
            .filter_map(|n| {
                n.commit
                    .as_ref()
                    .filter(|c| lineage.contains(&c.oid))
                    .map(|c| {
                        let rgb = crate::ui::graph_pixels::color_to_rgb(
                            theme.lane_color(n.color_index),
                        );
                        (c.oid, rgb)
                    })
            })
            .collect(),
        None => std::collections::HashMap::new(),
    };
    // Fold only the rows the window can draw: `fold_rows_windowed` allocates a
    // RenderRow (and per-connector underlay) for windowed rows alone, instead of
    // `visible_rows`' O(total-commits) full fold every redraw (#73). Rows are
    // dense in folded-index space, so `win_rows[k]` is folded row `win_start + k`
    // — the same index space `base`/the overlay use.
    let win_rows = if app.commit_filter.is_empty() {
        fold_rows_windowed(
            app.graph_layout.nodes.iter().enumerate(),
            win_start,
            win_end,
        )
    } else {
        fold_rows_windowed(
            app.visible_commit_indices
                .iter()
                .map(|&i| (i, &app.graph_layout.nodes[i])),
            win_start,
            win_end,
        )
    };
    // Absolute-indexed sparse inputs the core expects: only windowed positions
    // are filled; the rest are cheap defaults the core never reads (it touches
    // `[win_start, win_end)` only). Two single allocations, no per-row work.
    let mut row_oids: Vec<RowOids> = vec![RowOids::EMPTY; base.len()];
    // Per-row base-update flag, via the same `is_base_update_row` predicate the
    // unicode renderer uses, so pixel and unicode connectors dim on identical
    // rows. Frame/scroll-varying, so it lives here, never in the cached base.
    let mut force_dim: Vec<bool> = vec![false; base.len()];
    for (k, r) in win_rows.iter().enumerate() {
        let abs = win_start + k;
        if abs >= base.len() {
            break;
        }
        row_oids[abs] = RowOids {
            cells: &r.node.cell_oids,
            underlay: &r.underlay_oids,
        };
        force_dim[abs] = is_base_update_row(
            r.node,
            app.metadata_columns.mute_base_merges,
            app.merged.base_update.value(),
        );
    }
    dim_specs_window_core(base, &row_oids, &force_dim, &lit, &lane_rgb, win_start, win_end)
}

/// Per-row edge identities for dimming: parallel to a row's pixel `cells` and
/// `underlay`.
#[derive(Clone, Copy)]
struct RowOids<'a> {
    cells: &'a [crate::git::graph::CellOids],
    underlay: &'a [crate::git::graph::CellOids],
}

impl RowOids<'_> {
    /// A placeholder for out-of-window rows the dim core never reads.
    const EMPTY: RowOids<'static> = RowOids {
        cells: &[],
        underlay: &[],
    };
}

/// Pure core of [`dim_pixel_specs_window`]: clone and dim only the base specs in
/// `[win_start, win_end)`, leaving empty placeholders elsewhere. Independent of
/// `App`, so the windowing/placeholder contract is unit-testable directly.
///
/// The full-length result keeps absolute row indexing valid for the overlay, but
/// the (per-cell hashmap) dim work runs only for the window: out-of-window rows
/// are bulk-filled with empty placeholders (`Default`, no heap), never
/// per-element-branched over the whole graph (#73).
///
/// Invariant: for any row inside the window the result is byte-identical to
/// dimming that row over the full range — dimming is per-row (`apply_trace_dim`
/// reads only that row's cells and oids), so restricting which rows are built
/// never changes a built row's content.
fn dim_specs_window_core(
    base: &[crate::ui::graph_pixels::RowSpec],
    row_oids: &[RowOids],
    force_dim: &[bool],
    lit: &std::collections::HashMap<crate::git::graph::CellEdge, git2::Oid>,
    lane_rgb: &std::collections::HashMap<git2::Oid, [u8; 3]>,
    win_start: usize,
    win_end: usize,
) -> Vec<crate::ui::graph_pixels::RowSpec> {
    let mut out: Vec<crate::ui::graph_pixels::RowSpec> =
        vec![crate::ui::graph_pixels::RowSpec::default(); base.len()];
    let end = win_end.min(base.len());
    for i in win_start..end {
        let mut spec = base[i].clone();
        if force_dim.get(i).copied().unwrap_or(false) {
            // Base-update back-merge (#55): force-dim the entire connector,
            // mirroring the unicode `force_dim` branch. This wins over trace
            // dimming (the unicode path OR-s them), so every cell fades
            // regardless of lineage and no recolor is applied.
            force_dim_all_cells(&mut spec.cells);
            force_dim_all_cells(&mut spec.underlay);
        } else if let Some(o) = row_oids.get(i) {
            // `spec.cells`/`spec.underlay` are (truncated) 1:1 with the node's
            // cells/underlay, so their OIDs align by index; dimming only reads
            // the cells that survived truncation.
            apply_trace_dim(&mut spec.cells, o.cells, lit, lane_rgb);
            apply_trace_dim(&mut spec.underlay, o.underlay, lit, lane_rgb);
        }
        out[i] = spec;
    }
    out
}

/// Force every pixel cell in a row to its dimmed (low-alpha) variant, both
/// strokes. Used for base-update back-merge rows (#55) whose whole connector
/// recedes; colours are preserved (dim only lowers alpha), matching the unicode
/// renderer's DIM modifier.
fn force_dim_all_cells(cells: &mut [crate::ui::graph_pixels::PixelCell]) {
    for pc in cells.iter_mut() {
        pc.dim = true;
        pc.dim_secondary = true;
    }
}

/// Set `dim` on every pixel cell whose OWN edge is not in `lineage`. A
/// `HorizontalPipe` crossing carries two independent edges — the horizontal
/// stroke (primary edge, drawn in the cell's `secondary` color) and the
/// vertical lane crossed underneath (secondary edge, the cell's `color`) — so
/// each direction is dimmed from its own edge rather than all-or-nothing.
/// Every other shape's stroke belongs to its PRIMARY edge alone: a secondary
/// edge there is a different branch co-routed through the column, drawn by
/// that branch's own curve, so it neither lights nor recolors this cell
/// (commit dots are the exception — they light via either edge, keeping
/// their own color). An edge is traced only when both its `(child, parent)`
/// endpoints are on the lineage (see `edge_is_traced`).
fn apply_trace_dim(
    cells: &mut [crate::ui::graph_pixels::PixelCell],
    oids: &[crate::git::graph::CellOids],
    lit: &std::collections::HashMap<crate::git::graph::CellEdge, git2::Oid>,
    lane_rgb: &std::collections::HashMap<git2::Oid, [u8; 3]>,
) {
    use crate::git::graph::CellEdge;
    use crate::ui::graph_pixels::CellShape;
    let is_lit = |edge: Option<CellEdge>| edge.is_some_and(|e| lit.contains_key(&e));
    // A lit stroke takes the lane color of the commit the lit-edge relation
    // picked (the branch line's own commit), so a traced route reads as one
    // continuous color even through cells a different branch drew (shared fork
    // runs, crossings, its merge arc into the trunk).
    let color_of = |edge: Option<CellEdge>| -> Option<[u8; 3]> {
        edge.and_then(|e| lit.get(&e))
            .and_then(|oid| lane_rgb.get(oid))
            .copied()
    };
    for (i, pc) in cells.iter_mut().enumerate() {
        let (primary, secondary) = oids.get(i).copied().unwrap_or((None, None));
        if pc.shape == CellShape::HorizontalPipe {
            pc.dim_secondary = !is_lit(primary);
            pc.dim = !is_lit(secondary);
            if let Some(rgb) = color_of(primary) {
                pc.secondary = rgb;
            }
            if let Some(rgb) = color_of(secondary) {
                pc.color = rgb;
            }
        } else if matches!(pc.shape, CellShape::Commit { .. }) {
            // Dots keep their own color (including the gold HEAD star).
            pc.dim = !(is_lit(primary) || is_lit(secondary));
            pc.dim_secondary = pc.dim;
        } else {
            // The cell's own stroke is its PRIMARY edge. A secondary edge here
            // is another branch co-routed through this column (a farther fork
            // arm passing a nearer arm's ┴, or a shared run) — under the curve
            // renderer that branch draws its own curve from its own spoke
            // cell, so the co-routed edge must not light or recolor THIS
            // cell's stroke: doing so painted a sibling's lead-in bright in
            // the traced branch's color.
            pc.dim = !is_lit(primary);
            pc.dim_secondary = pc.dim;
            if let Some(rgb) = color_of(primary) {
                pc.color = rgb;
            }
        }
    }
}

/// The half-open range of list rows worth building fully, given `n` total rows,
/// the current scroll `offset`, the drawable `viewport` height, and the selected
/// row's filtered position (if any).
///
/// Every list item is one line tall, so ratatui draws exactly the window
/// `[final_offset, final_offset + viewport)`, where `final_offset` is `offset`
/// clamped so the selection stays visible — always within
/// `[min(offset, selected), max(offset + viewport, selected + 1))`. Building that
/// span (plus a small margin for scroll-adjust and folding slack) is sufficient
/// to render an identical frame; everything else is off-screen. Returns
/// `[0, n)` degenerately for tiny graphs so nothing is ever clipped.
fn visible_row_window(
    n: usize,
    offset: usize,
    viewport: usize,
    selected_pos: Option<usize>,
) -> (usize, usize) {
    const MARGIN: usize = 8;
    let lo = selected_pos.map_or(offset, |s| s.min(offset));
    let hi = selected_pos.map_or(offset + viewport, |s| (s + 1).max(offset + viewport));
    let start = lo.saturating_sub(MARGIN);
    let end = hi.saturating_add(MARGIN).min(n);
    (start.min(end), end)
}

impl<'a> GraphViewWidget<'a> {
    pub fn new(
        app: &App,
        width: u16,
        theme: &'a Theme,
        pixel_mode: bool,
        viewport_height: u16,
    ) -> Self {
        let needed = (app.graph_layout.max_lane + 1) * 2;
        let graph_width = effective_graph_width(needed, app.graph_width_cap);
        let inner_width = width.saturating_sub(2) as usize;
        let selected_branch_name = app.selected_branch_name();
        let has_filter = !app.commit_filter.is_empty();
        let current_selected = app.graph_nav.graph_list_state.selected();
        let now = Local::now();
        let remotes = &app.remotes;
        let open_prs = &app.open_prs;
        // Head-OID index over the open PRs, built once per frame (O(#PRs), not
        // O(#commits)): lets each row answer "is this a PR head / PR merge?" in
        // O(1) without any per-render scan, keeping the work windowed.
        let pr_ctx = PrContext::new(open_prs);
        let merged_branches = &app.merged.branches;
        // OIDs of base-update ("back-merge") commits (#55); a per-row O(1)
        // membership test, so the check stays inside the windowed render path.
        let base_update_merges = app.merged.base_update.value();
        let metadata_columns = app.metadata_columns;
        // Selected commit's lit-edge set for tracing (Unicode dim); None =
        // off. In pixel mode the dim lives in the row specs, not the text
        // layer.
        let trace = if pixel_mode {
            None
        } else {
            app.active_trace_lineage()
                .map(|l| crate::git::graph::trace_lit_edges(&app.graph_layout, &l))
        };

        // Frame-constant render inputs, gathered once and shared by every row (see
        // `RowRenderCtx`). Per-row values (`node`, `RowFlags`) travel separately.
        let ctx = RowRenderCtx {
            theme,
            now,
            pixel_mode,
            remotes,
            open_prs,
            pr_ctx: &pr_ctx,
            merged_branches,
            base_update_merges,
            metadata_columns,
            graph_width,
            total_width: inner_width,
            selected_branch_name,
            trace: trace.as_ref(),
        };

        // In pixel mode, connector rows are folded into their commit row so the
        // list items match the pixel specs one-for-one (same filtered index
        // space). In Unicode mode connectors remain their own rows.
        let rows = visible_rows(app, pixel_mode);

        // Only the rows the list can actually draw are worth the (per-row
        // non-trivial) `render_graph_line` cost. Every list item is exactly one
        // line tall, so ratatui's scroll math is unaffected by keeping the item
        // count at `rows.len()` while filling out-of-window rows with cheap blank
        // placeholders. See `visible_row_window` for why the window is correct.
        let n = rows.len();
        let offset = app.graph_nav.graph_list_state.offset();
        let selected_pos = current_selected.and_then(|sel| {
            rows.iter().position(|r| r.full_idx == sel)
        });
        let (win_start, win_end) =
            visible_row_window(n, offset, viewport_height as usize, selected_pos);

        let mut selected_in_filtered = None;
        let mut items: Vec<ListItem> = Vec::with_capacity(n);
        let mut chip_hits: Vec<Vec<ChipHit>> = Vec::with_capacity(n);

        for (filtered_pos, row) in rows.into_iter().enumerate() {
            // Out-of-window rows are never drawn: emit a blank one-line item so
            // the list keeps its full length (offset/selection indices, the
            // scrollbar, and `graph_chip_hits`' row alignment all stay intact)
            // without paying to build a line the user cannot see.
            if filtered_pos < win_start || filtered_pos >= win_end {
                items.push(ListItem::new(""));
                chip_hits.push(Vec::new());
                continue;
            }
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
            let (line, chips) =
                render_graph_line(node, &ctx, RowFlags { is_selected, is_marked });
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

/// The bare merge glyph a collapsed merge commit shows in place of its message
/// (#59), and the icon [`merged_badge`] prefixes its "merged" label with — the
/// same nf-oct-git_merge icon, so a collapsed merge or a landed branch both
/// read as "this is a merge" at a glance.
const MERGE_ICON: &str = "\u{f419}"; // nf-oct-git_merge

/// Badge appended to a branch already merged into the trunk (merge or squash).
/// Rendered muted/dimmed; the branch chips themselves are dimmed to match.
/// Derived from [`MERGE_ICON`] so the glyph's codepoint exists in one place.
fn merged_badge() -> String {
    format!("{MERGE_ICON} merged")
}

/// Style for merged-branch decorations: muted and dimmed, so a landed branch
/// recedes without disappearing (the hide-merged toggle removes it entirely).
fn merged_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.text_muted)
        .add_modifier(Modifier::DIM)
}

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

/// The open PR to badge on a graph row, or `None`. Primary, data-driven rule
/// (#42): the row's commit is a PR's head commit — this pins the badge to
/// exactly one row per PR, even when a local and a remote ref for the same
/// branch sit on different commits. Fallback: a head-branch *name* label, but
/// only for PRs `gh` gave no head OID for. A PR that has a head OID is therefore
/// only ever badged on that exact commit and can never double-render via a
/// branch label, which is what old name-only matching did.
fn pr_for_row<'p>(
    commit_oid: git2::Oid,
    branch_names: &[String],
    remotes: &[String],
    pr_ctx: &PrContext<'p>,
    open_prs: &'p HashMap<String, PrInfo>,
) -> Option<&'p PrInfo> {
    if let Some(pr) = pr_ctx.pr_for_head_commit(commit_oid) {
        return Some(pr);
    }
    branch_names.iter().find_map(|name| {
        let bare = strip_remote(name, remotes).unwrap_or(name.as_str());
        open_prs.get(bare).filter(|pr| pr.head_oid.is_none())
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

    // Badge color always resolves through the same palette lookup as the
    // graph line/node for this commit's lane (`theme.lane_color`), so a
    // branch's badge and its line never diverge. HEAD is distinguished by
    // weight (bold, below), not by a separate color.
    let base_color = theme.lane_color(color_index);

    // Helper to create style based on selection state. Restraint: color is the
    // single emphasis device for ordinary chips; only the checked-out HEAD's
    // chips also carry bold, reserving the stronger accent for the one ref that
    // matters most. Selection adds REVERSED on top (orthogonal affordance).
    let make_style = |branch_name: &str| -> Style {
        let mut style = Style::default().fg(base_color);
        if is_head {
            style = style.add_modifier(Modifier::BOLD);
        }
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

        let shown = SHOWN_LABELS.min(result.len());
        let extra_count = result.len() - shown;

        // Helper: split a formatted label into its leading icon prefix (cloud
        // for a remote-only chip, ↔ for a synced local, or empty) and the bare
        // branch name. The name is recovered for re-abbreviation; the icon is
        // returned separately so the collapse below can re-attach it — a
        // multi-branch row must keep its remote/synced markers, not drop them.
        let split_label = |label: &str| -> (String, String) {
            let s = label.trim_start_matches('[');
            let (icon, rest) = if let Some(r) = s.strip_prefix(REMOTE_ONLY_ICON) {
                (format!("{} ", REMOTE_ONLY_ICON), r.trim_start())
            } else if let Some(r) = s.strip_prefix(SYNCED_ICON) {
                (format!("{} ", SYNCED_ICON), r.trim_start())
            } else {
                (String::new(), s)
            };
            let bare = rest.split([']', ' ']).next().unwrap_or(label).to_string();
            (icon, bare)
        };

        // Budget the available width across the shown labels
        let per_label = MAX_LABEL_WIDTH / shown;

        // Display order: always the stable badge order from `result` (which
        // in turn follows `branch_names`, itself deterministically sorted at
        // the source — see `BranchInfo::list_all`). Never reordered by which
        // branch is selected/navigated to: that would make badge order flap
        // as the cursor moves, which is the bug this fixes.
        let mut combined = String::new();
        for (pos, (label, _)) in result.iter().take(shown).enumerate() {
            let (icon, clean_name) = split_label(label);
            // Only the last shown label carries the "+N" suffix
            let extra = if pos == shown - 1 { extra_count } else { 0 };
            // Reserve budget for the icon so the abbreviated name plus its
            // re-attached marker still fits the per-label allowance.
            let budget = per_label.saturating_sub(display_width(&icon));
            let abbrev = abbreviate_branch_label(&clean_name, budget, extra);
            // Re-attach the icon marker inside the brackets (mirrors make_label).
            let abbrev = if icon.is_empty() {
                abbrev
            } else {
                abbrev.replacen('[', &format!("[{}", icon), 1)
            };
            combined.push_str(&abbrev);
        }

        // Style follows the selected branch when it's among these labels
        // (highlight only — does not affect display order above).
        let selected_idx = selected_branch_name
            .and_then(|sel| {
                branch_names
                    .iter()
                    .position(|n| n == sel || n.ends_with(&format!("/{}", sel)))
            })
            .unwrap_or(0)
            .min(result.len().saturating_sub(1));
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
    // Restraint: the tag color alone distinguishes the chip; no bold (bold is
    // reserved for the HEAD branch and actionable PR badges), so tags read as
    // one quiet, consistent family alongside non-HEAD branch chips.
    let style = Style::default().fg(theme.tag_label);
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

/// Whether a row is a base-update ("back-merge") merge that should render
/// strongly muted (#55): its message greys+dims and its own graph connector is
/// force-dimmed so the noisy back-merge line recedes. This is the single source
/// of truth shared by BOTH renderers — the unicode path (`render_cells_unicode`
/// force_dim) and the pixel path (`dim_pixel_specs_window` per-row force-dim) —
/// so they agree on which rows mute. HEAD is never muted.
fn is_base_update_row(
    node: &GraphNode,
    mute_base_merges: bool,
    base_update_merges: &HashSet<git2::Oid>,
) -> bool {
    mute_base_merges
        && !node.is_head
        && node.is_merge()
        && node
            .commit
            .as_ref()
            .is_some_and(|c| base_update_merges.contains(&c.oid))
}

/// Frame-constant render inputs, built once per frame and shared by every row.
/// Bundles the ~13 values that used to be threaded individually through
/// `render_graph_line` / `render_graph_line_tail`; per-row values travel
/// separately as `node` and `RowFlags`.
struct RowRenderCtx<'a> {
    theme: &'a Theme,
    now: DateTime<Local>,
    pixel_mode: bool,
    remotes: &'a [String],
    open_prs: &'a HashMap<String, PrInfo>,
    pr_ctx: &'a PrContext<'a>,
    merged_branches: &'a HashSet<String>,
    base_update_merges: &'a HashSet<git2::Oid>,
    metadata_columns: MetadataColumns,
    /// Graph column width cap (glyph budget), same for every row this frame.
    graph_width: usize,
    /// Total drawable inner width available to a row.
    total_width: usize,
    selected_branch_name: Option<&'a str>,
    /// Selected commit's lit-edge trace set; `None` when tracing is off.
    trace: Option<&'a std::collections::HashMap<crate::git::graph::CellEdge, git2::Oid>>,
}

/// Per-row render flags decided at the call site.
#[derive(Clone, Copy)]
struct RowFlags {
    is_selected: bool,
    is_marked: bool,
}

fn render_graph_line<'a>(
    node: &GraphNode,
    ctx: &RowRenderCtx<'_>,
    flags: RowFlags,
) -> (Line<'a>, Vec<ChipHit>) {
    let mut spans: Vec<Span> = Vec::new();

    // A base-update ("back-merge") commit (#55): when the option is on, its
    // message renders strongly muted and its own graph glyphs (the noisy
    // back-merge connector) are force-dimmed. Decided once here (via the shared
    // `is_base_update_row` predicate) so both the unicode cell renderer and the
    // pixel dim pass agree with the message tail. HEAD is never muted.
    let is_base_update = is_base_update_row(
        node,
        ctx.metadata_columns.mute_base_merges,
        ctx.base_update_merges,
    );

    // Graph start marker (to distinguish from borders). GRAPH_LEADING_COLUMNS
    // is the shared contract with the pixel overlay's x-offset.
    spans.push(Span::raw(" ".repeat(GRAPH_LEADING_COLUMNS as usize)));
    let mut left_width: usize = GRAPH_LEADING_COLUMNS as usize;

    // Pixel mode: the graph column is painted by an image overlay, so emit
    // blank space of the exact same width to keep the text layout identical —
    // plus the `…` marker (in the text layer) when the width cap truncates the
    // row, since the image can't draw it.
    if ctx.pixel_mode {
        let (budget, ellipsis) = graph_truncation(node.cells.len(), ctx.graph_width);
        for _ in 0..budget {
            spans.push(Span::raw(" "));
            left_width += 1;
        }
        if ellipsis {
            spans.push(Span::styled("…", ellipsis_style(ctx.theme)));
            left_width += 1;
        }
    } else {
        // Graph glyphs render bold; with tracing, non-lineage cells are dimmed.
        // A base-update back-merge (#55) force-dims its whole connector so the
        // noisy line recedes; ordinary merge muting stays in the message text.
        left_width = render_cells_unicode(
            &mut spans,
            node,
            ctx.theme,
            left_width,
            ctx.graph_width,
            ctx.trace,
            is_base_update,
        );
    }

    // Padding to align to the (capped) graph width. Reclaimed width flows to the
    // message budget: the tail sizes the message from `total_width - left_width`.
    let graph_display_width = ctx.graph_width;
    if left_width < graph_display_width + 1 {
        // +1 accounts for the start marker
        let padding = graph_display_width + 1 - left_width;
        spans.push(Span::raw(" ".repeat(padding)));
        left_width += padding;
    }

    // Reserve blank columns for the author avatar (drawn by a separate image
    // overlay in pixel mode). The message tail then starts after them.
    if avatars_active(ctx.pixel_mode, ctx.metadata_columns) {
        spans.push(Span::raw(" ".repeat(AVATAR_RESERVED_CELLS as usize)));
        left_width += AVATAR_RESERVED_CELLS as usize;
    }

    render_graph_line_tail(spans, left_width, node, ctx, flags, is_base_update)
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
    trace: Option<&std::collections::HashMap<crate::git::graph::CellEdge, git2::Oid>>,
    force_dim: bool,
) -> usize {
    // `budget` graph columns are available for glyphs; when truncating, one more
    // column holds the `…`.
    let (budget, ellipsis) = graph_truncation(node.cells.len(), cap);

    // Whether the cell at `idx` should be dimmed: `force_dim` (a base-update
    // back-merge row, #55) dims the entire connector; otherwise a cell dims when
    // tracing is active and it is not lit by the selected commit's trace.
    let is_dim = |idx: usize| -> bool {
        force_dim
            || trace.is_some_and(|lit| {
                let oids = node.cell_oids.get(idx).copied().unwrap_or((None, None));
                !crate::git::graph::cell_is_traced(oids, lit)
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
fn render_graph_line_tail<'a>(
    mut spans: Vec<Span<'a>>,
    mut left_width: usize,
    node: &GraphNode,
    ctx: &RowRenderCtx<'_>,
    flags: RowFlags,
    is_base_update: bool,
) -> (Line<'a>, Vec<ChipHit>) {
    let mut chips: Vec<ChipHit> = Vec::new();
    // Separator between graph and commit info
    spans.push(Span::raw(" "));
    left_width += 1;

    // Compare marker: flags commits that are marked or a comparison endpoint.
    if flags.is_marked {
        spans.push(Span::styled(
            "◆ ",
            Style::default()
                .fg(ctx.theme.search_cursor)
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
        let style = Style::default().fg(ctx.theme.text_primary);
        spans.push(Span::styled(text, style));
        return (Line::from(spans), chips);
    }

    // Early return for connector-only rows
    let commit = match &node.commit {
        Some(c) => c,
        None => return (Line::from(spans), chips),
    };

    // Style definitions
    let hash_style = Style::default().fg(ctx.theme.hash_color);
    let author_style = Style::default().fg(ctx.theme.author_color);
    let date_style = Style::default().fg(ctx.theme.date_color);
    // PR-merge commit (#52): a merge that landed a GitHub PR reads as machinery,
    // not authored work, so its message renders greyed (an explicit muted color,
    // distinct from the plain DIM used for ordinary muted merges). Detection is
    // data-driven (second parent is a known PR head) with the GitHub merge
    // message format as fallback for already-merged PRs; both inputs come off
    // this row's commit, so the check stays inside the windowed per-row path.
    // HEAD is never greyed, matching the muted-merge rule below.
    let is_pr_merge = !node.is_head
        && ctx.pr_ctx.is_pr_merge(
            node.is_merge(),
            commit.parent_oids.get(1).copied(),
            &commit.message,
        );
    // Muted merge: dim only the message text (VSCode-style) — the graph dot and
    // lines stay full-strength. HEAD is never muted so its message stays legible.
    let muted_merge = ctx.metadata_columns.mute_merges && node.is_merge() && !node.is_head;
    // Collapse merges (#59): a stronger form of muting that replaces the merge
    // commit's message with a bare merge glyph. Same detection seam as
    // `mute_merges` (any merge, never HEAD), so the two options unify rather than
    // duplicate. When on it implies muting even if `mute_merges` is off.
    let collapse_merge = ctx.metadata_columns.collapse_merges && node.is_merge() && !node.is_head;
    // Three separate style domains, decided independently and never merged here:
    //   1. message-text precedence (this chain) — which mute wins for the message;
    //   2. connector-cell dim (force_dim ∪ trace) — see `render_cells_unicode`
    //      and `dim_pixel_specs_window`, applied to the graph glyphs, not text;
    //   3. chip styling (branch/tag/merged badges) — see `optimize_branch_display`
    //      and `merged_style`.
    // The widget-level selection highlight (`Theme::selection_style`) then patches
    // BOLD over the whole selected line and subtracts DIM, so BOLD wins when a row
    // is selected without any domain going muddy (DIM+BOLD).
    //
    // Precedence: a base-update back-merge (#55) is the noisiest, so it wins with
    // the strongest mute (muted color + DIM). Then a PR-landing merge (#52), then
    // ordinary merge muting/collapse, then selection bold.
    let msg_style = if is_base_update {
        Style::default()
            .fg(ctx.theme.text_muted)
            .add_modifier(Modifier::DIM)
    } else if is_pr_merge {
        Style::default().fg(ctx.theme.text_muted)
    } else if muted_merge || collapse_merge {
        Style::default().add_modifier(Modifier::DIM)
    } else if flags.is_selected {
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
        ctx.selected_branch_name,
        ctx.theme,
        ctx.remotes,
    );

    // Tag labels render after branch labels with a distinct color.
    let tag_display = build_tag_labels(&node.tag_names, ctx.theme);

    // Open-PR badge (#42): shown exactly once per PR, on the PR's head commit.
    // Colored by CI status, with review/comment markers (#43).
    let pr_badge = pr_for_row(commit.oid, &node.branch_names, ctx.remotes, ctx.pr_ctx, ctx.open_prs)
        .map(|pr| (pr_badge_text(pr), pr_badge_color(pr, ctx.theme)));
    // Chip plus a trailing space.
    let pr_badge_width = pr_badge.as_ref().map_or(0, |(b, _)| display_width(b) + 1);

    // Merged badge: shown when one of this node's local branches has already
    // landed on the trunk (merge commit, fast-forward, or squash). The branch
    // chips are dimmed to match. Only reachable when the hide-merged toggle is
    // off — hidden merged branches drop out of the graph entirely.
    let has_merged_branch = node
        .branch_names
        .iter()
        .any(|n| ctx.merged_branches.contains(n));
    // Computed once and reused below (render section) so the badge text is
    // built at most once per row.
    let merged_badge_text = has_merged_branch.then(merged_badge);
    let merged_badge_width = merged_badge_text
        .as_deref()
        .map_or(0, |b| display_width(b) + 1);

    // === Right-aligned: date author hash (fixed width) ===
    let date = format_date_field(commit.timestamp, ctx.now); // DATE_FIELD_WIDTH chars
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
    let remaining_for_content = ctx.total_width.saturating_sub(graph_width);

    // Determine which right-side elements to show based on available space
    let (show_date, show_author, show_hash, right_width) =
        compute_right_side_visibility(remaining_for_content, ctx.metadata_columns);

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
        let branch = resolve_chip_branch(label, &node.branch_names, ctx.remotes);
        let is_merged = branch.as_deref().is_some_and(|n| ctx.merged_branches.contains(n));
        if let Some(name) = branch {
            chips.push(ChipHit {
                x_start: chip_start as u16,
                x_end: left_width as u16,
                target: ChipTarget::Branch(name),
            });
        }
        // A merged branch's chip is dimmed (line color included) so it recedes.
        let style = if is_merged { merged_style(ctx.theme) } else { *style };
        spans.push(Span::styled(label.clone(), style));
    }
    if !branch_display.is_empty() {
        spans.push(Span::raw(" "));
        left_width += 1;
    }

    // Render merged badge (after branch labels, before the PR badge)
    if let Some(badge) = &merged_badge_text {
        left_width += display_width(badge) + 1;
        spans.push(Span::styled(badge.clone(), merged_style(ctx.theme)));
        spans.push(Span::raw(" "));
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
            .fg(ctx.theme.text_muted)
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
        .saturating_sub(merged_badge_width)
        .saturating_sub(pr_badge_width)
        .saturating_sub(tag_width)
        .saturating_sub(stash_width)
        .saturating_sub(right_width);
    // Collapse (#59) replaces the whole message with a single merge glyph;
    // otherwise the message is truncated to its width budget as usual.
    let message = if collapse_merge {
        MERGE_ICON.to_string()
    } else {
        truncate_to_width(&commit.message, available_for_message)
    };
    let message_width = display_width(&message);
    spans.push(Span::styled(message, msg_style));
    left_width += message_width;

    // Padding so the right-aligned block starts at a fixed column
    let padding = ctx.total_width
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
            .title_style(self.theme.title_style(self.is_focused))
            .borders(Borders::ALL)
            .border_style(self.theme.border_style(self.is_focused))
            .border_type(self.theme.border_type());

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

    // ── visible_row_window (viewport slice for lazy row building) ─────
    //
    // Contract: whatever rows ratatui may draw for a given scroll offset and
    // selection must lie inside the window, so the placeholder rows outside it
    // are never visible. ratatui draws `[final_offset, final_offset + viewport)`
    // where `final_offset` is `offset` clamped so the selection stays visible.

    /// Every row ratatui could draw after scrolling to `selected` must be inside
    /// the window `[start, end)`. Emulates ratatui's offset clamp.
    fn assert_covers(n: usize, offset: usize, viewport: usize, selected: usize) {
        let (start, end) = visible_row_window(n, offset, viewport, Some(selected));
        // ratatui's scroll clamp: keep `selected` within the drawn viewport.
        let final_offset = if selected < offset {
            selected
        } else if viewport > 0 && selected >= offset + viewport {
            selected + 1 - viewport
        } else {
            offset
        };
        let drawn_end = (final_offset + viewport).min(n);
        assert!(
            start <= final_offset && drawn_end <= end,
            "window [{start},{end}) fails to cover drawn [{final_offset},{drawn_end}) \
             (n={n}, offset={offset}, viewport={viewport}, selected={selected})"
        );
        // The selection itself is always inside the window.
        assert!(
            selected >= start && selected < end,
            "selection {selected} outside window [{start},{end})"
        );
    }

    #[test]
    fn window_covers_drawn_range_for_all_scroll_positions() {
        let n = 5000;
        let viewport = 40;
        // Selection above, inside, and below the current viewport.
        for &offset in &[0usize, 100, 2000, 4960, 4999] {
            for &selected in &[0usize, offset, offset + 20, offset + 200, 4999] {
                let sel = selected.min(n - 1);
                assert_covers(n, offset, viewport, sel);
            }
        }
        // A jump far past the viewport (PageDown / G) still covers the target.
        assert_covers(n, 0, viewport, 4999);
        assert_covers(n, 4999, viewport, 0);
    }

    #[test]
    fn window_never_exceeds_bounds_and_is_ordered() {
        for &(n, offset, vp, sel) in &[
            (0usize, 0usize, 40usize, 0usize),
            (1, 0, 40, 0),
            (5, 0, 40, 4),
            (10, 100, 40, 5), // stale offset past the end
        ] {
            let (start, end) = visible_row_window(n, offset, vp, (sel < n).then_some(sel));
            assert!(start <= end, "start {start} > end {end}");
            assert!(end <= n, "end {end} > n {n}");
        }
    }

    #[test]
    fn small_graph_builds_every_row() {
        // A graph shorter than the viewport must build all of its rows.
        let (start, end) = visible_row_window(12, 0, 40, Some(3));
        assert_eq!((start, end), (0, 12));
    }

    #[test]
    fn window_without_selection_tracks_offset() {
        // No selection: the window follows the scroll offset's viewport.
        let (start, end) = visible_row_window(5000, 2000, 40, None);
        assert!(start <= 2000 && end >= 2040, "window [{start},{end})");
        assert!(end <= 5000);
    }

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
        // A local `main` and an unrelated remote `origin/dev` (no local twin):
        // they must NOT merge into a single ↔ synced label, but the remote-only
        // ref still carries its cloud marker (issue #74 — the cloud must not be
        // dropped just because another chip shares the row).
        let out = labels(&["main", "origin/dev"], &["origin"]);
        assert!(
            !out.iter().any(|l| l.contains(SYNCED_ICON)),
            "unrelated local+remote must not be treated as synced: {out:?}"
        );
        let joined = out.join("");
        assert!(
            joined.contains(REMOTE_ONLY_ICON),
            "remote-only ref keeps its cloud: {out:?}"
        );
        assert!(joined.contains("main") && joined.contains("dev"), "{out:?}");
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
            head_oid: None,
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
            head_oid: None,
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

    // ── one badge per PR, on its head commit (#42) ───────────────────

    /// A non-merge commit node at `oid(oid_byte)` carrying `branches`.
    fn commit_node(oid_byte: u8, message: &str, branches: &[&str]) -> GraphNode {
        use crate::git::CommitInfo;
        let commit = CommitInfo {
            oid: oid(oid_byte),
            short_id: "abc1234".to_string(),
            author_name: "a".to_string(),
            author_email: "a@b".to_string(),
            timestamp: Local::now(),
            message: message.to_string(),
            full_message: message.to_string(),
            parent_oids: vec![oid(0)], // single parent => not a merge
        };
        GraphNode {
            commit: Some(commit),
            lane: 0,
            color_index: 0,
            branch_names: branches.iter().map(|s| s.to_string()).collect(),
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

    /// A `PrInfo` whose head commit is `oid(head_byte)`.
    fn pr_head(number: u64, head_byte: u8) -> PrInfo {
        let mut p = pr(number);
        p.head_oid = Some(oid(head_byte).to_string());
        p
    }

    fn open_map(pairs: Vec<(&str, PrInfo)>) -> HashMap<String, PrInfo> {
        pairs.into_iter().map(|(b, p)| (b.to_string(), p)).collect()
    }

    #[test]
    fn pr_badge_renders_only_on_the_head_commit() {
        // PR #12's head is commit 5.
        let open = open_map(vec![("feat", pr_head(12, 5))]);
        // The head commit gets the badge even with NO branch label — it is
        // resolved by OID, not by name.
        let head = commit_node(5, "head of the feature", &[]);
        assert!(
            row_text(&head, &open).contains("#12"),
            "head commit shows the badge: {:?}",
            row_text(&head, &open)
        );
        // Another commit of the same branch (carrying the `feat` label, e.g. an
        // out-of-date remote ref) is NOT the head → no badge. This is the #42
        // fix: a multi-commit PR no longer paints its badge on every labelled row.
        let other = commit_node(3, "an earlier commit", &["feat"]);
        assert!(
            !row_text(&other, &open).contains("#12"),
            "non-head commit shows no badge: {:?}",
            row_text(&other, &open)
        );
    }

    #[test]
    fn pr_badge_falls_back_to_branch_name_without_a_head_oid() {
        // A PR gh gave no head OID for still badges via its head-branch label.
        let open = open_map(vec![("feat", pr(7))]); // head_oid None
        let tip = commit_node(9, "tip", &["feat"]);
        assert!(row_text(&tip, &open).contains("#7"));
    }

    #[test]
    fn pr_badge_full_row_encodes_approved_and_comment_state() {
        // #43: a full-row render surfaces the approved + outside-comment markers.
        let mut approved = pr_head(1, 5);
        approved.review = ReviewState::Approved;
        approved.outside_activity = true;
        let open = open_map(vec![("feat", approved)]);
        let node = commit_node(5, "x", &[]);
        let text = row_text(&node, &open);
        assert!(text.contains(PR_APPROVED_ICON), "approved glyph present: {text:?}");
        assert!(text.contains(PR_COMMENT_ICON), "comment glyph present: {text:?}");
    }

    // ── PR merge commits render greyed (#52) ─────────────────────────

    /// A merge node at `oid(oid_byte)` with the two given parent OIDs.
    fn merge_node_full(oid_byte: u8, message: &str, parents: [u8; 2]) -> GraphNode {
        let mut n = merge_node(message);
        let c = n.commit.as_mut().unwrap();
        c.oid = oid(oid_byte);
        c.parent_oids = vec![oid(parents[0]), oid(parents[1])];
        n
    }

    /// The style of the span whose content is exactly `content`.
    fn span_style(line: &Line, content: &str) -> Style {
        line.spans
            .iter()
            .find(|s| s.content.as_ref() == content)
            .unwrap_or_else(|| panic!("span {content:?} not found in {line:?}"))
            .style
    }

    #[test]
    fn pr_merge_message_is_greyed_data_driven() {
        let theme = Theme::dark();
        // PR #9's head is commit 5; a merge whose 2nd parent is 5 is a PR merge.
        let open = open_map(vec![("feat", pr_head(9, 5))]);
        let node = merge_node_full(20, "Merge feat", [1, 5]);
        // mute_merges OFF, so only the PR-merge rule can grey the message.
        let line = render_row(&node, &open, false);
        assert_eq!(
            span_style(&line, "Merge feat").fg,
            Some(theme.text_muted),
            "PR merge message is greyed"
        );
        // A plain local merge (2nd parent not a PR head, non-GitHub message) is
        // left at full strength when muting is off.
        let plain = merge_node_full(21, "Merge branch 'x'", [1, 2]);
        let plain_line = render_row(&plain, &open, false);
        assert_ne!(
            span_style(&plain_line, "Merge branch 'x'").fg,
            Some(theme.text_muted),
            "a plain local merge is not greyed"
        );
    }

    #[test]
    fn pr_merge_message_is_greyed_by_github_format_when_pr_closed() {
        let theme = Theme::dark();
        // No open PRs (a merged PR has left `gh pr list`); the message format
        // alone identifies it as a PR merge.
        let open = HashMap::new();
        let node = merge_node_full(20, "Merge pull request #42 from o/b", [1, 2]);
        let line = render_row(&node, &open, false);
        assert_eq!(
            span_style(&line, "Merge pull request #42 from o/b").fg,
            Some(theme.text_muted),
        );
    }

    // ── metadata column visibility / width budget ────────────────────

    fn cols(author: bool, hash: bool, date: bool) -> MetadataColumns {
        MetadataColumns {
            author,
            hash,
            date,
            mute_merges: true,
            mute_base_merges: false,
            collapse_merges: false,
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

    // ── issue #74: multi-branch rows keep their cloud/synced markers ──
    //
    // Regression: the collapse path (result.len() > 1) rebuilt each chip from
    // the cleaned bare name and dropped the icon prefix, so a row with two
    // remote refs — or a synced pair alongside another branch — rendered as
    // bare local-looking labels (`[mac][main]`). Assert the markers survive.

    #[test]
    fn two_remote_refs_on_one_commit_both_keep_cloud_icon() {
        // The #74 repro: origin/mac + origin/main sit together on an older
        // commit (no local ref there). Single remote → prefix dropped, but each
        // chip must carry the cloud so it still reads as remote-only.
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &["origin/mac".to_string(), "origin/main".to_string()],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        assert_eq!(out.len(), 1);
        let label = &out[0].0;
        assert!(label.contains("mac") && label.contains("main"), "{label:?}");
        assert!(!label.contains("origin/"), "prefix dropped: {label:?}");
        // Two cloud glyphs — one per remote-only chip.
        assert_eq!(
            label.matches(REMOTE_ONLY_ICON).count(),
            2,
            "cloud on both remote chips: {label:?}"
        );
        assert!(!label.contains(SYNCED_ICON), "not synced: {label:?}");
    }

    #[test]
    fn synced_pair_in_multi_branch_row_keeps_synced_icon() {
        // A synced local+remote pair (main / origin/main) alongside an
        // unrelated local branch: the pair collapses to one ↔ chip and the ↔
        // marker must survive the multi-branch collapse.
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &[
                "main".to_string(),
                "origin/main".to_string(),
                "dev".to_string(),
            ],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        assert_eq!(out.len(), 1);
        let label = &out[0].0;
        assert!(label.contains(SYNCED_ICON), "synced marker kept: {label:?}");
        assert!(label.contains("main") && label.contains("dev"), "{label:?}");
        assert!(!label.contains("origin/"), "{label:?}");
    }

    #[test]
    fn local_only_multi_branch_row_never_gets_a_cloud() {
        // Two purely local branches with no remote counterparts: neither chip
        // may acquire a cloud (or synced) marker.
        let theme = Theme::dark();
        let out = optimize_branch_display(
            &["feature".to_string(), "hotfix".to_string()],
            false,
            0,
            None,
            &theme,
            &["origin".to_string()],
        );
        assert_eq!(out.len(), 1);
        let label = &out[0].0;
        assert!(
            !label.contains(REMOTE_ONLY_ICON),
            "no cloud on local chips: {label:?}"
        );
        assert!(!label.contains(SYNCED_ICON), "no synced marker: {label:?}");
        assert!(label.contains("feature") && label.contains("hotfix"), "{label:?}");
    }

    // ── badge order is stable regardless of selection (issue #50) ────

    #[test]
    fn badge_order_is_independent_of_which_branch_is_selected() {
        // Three branches on one commit — more than SHOWN_LABELS (2), so this
        // also exercises the collapse path. Regression: selecting each
        // branch in turn (as branch-cycling navigation does) used to move
        // that branch's chip to the front, flipping the visible order.
        let theme = Theme::dark();
        let names = [
            "alpha".to_string(),
            "beta".to_string(),
            "origin/gamma".to_string(),
        ];
        let remotes = ["origin".to_string()];

        let no_selection = optimize_branch_display(&names, false, 0, None, &theme, &remotes);
        let selected_alpha =
            optimize_branch_display(&names, false, 0, Some("alpha"), &theme, &remotes);
        let selected_beta =
            optimize_branch_display(&names, false, 0, Some("beta"), &theme, &remotes);
        let selected_gamma =
            optimize_branch_display(&names, false, 0, Some("origin/gamma"), &theme, &remotes);

        // Only the label text (not the style/highlight) needs to stay fixed.
        let text = |v: &[(String, Style)]| v.iter().map(|(s, _)| s.clone()).collect::<Vec<_>>();
        assert_eq!(text(&no_selection), text(&selected_alpha));
        assert_eq!(text(&no_selection), text(&selected_beta));
        assert_eq!(text(&no_selection), text(&selected_gamma));
    }

    #[test]
    fn badge_order_matches_source_order_two_labels() {
        // Two non-synced branches render as one combined chip, e.g.
        // "[mac][origin/mac]" (issue #50's exact example) — assert the
        // bracket groups keep `branch_names`' order and never flip when a
        // different branch becomes the "selected" one.
        let theme = Theme::dark();
        let names = ["mac".to_string(), "zzz-other".to_string()];
        let out = optimize_branch_display(&names, false, 0, None, &theme, &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "[mac][zzz-other]");

        // Selecting the second branch must not move it first.
        let out_selected =
            optimize_branch_display(&names, false, 0, Some("zzz-other"), &theme, &[]);
        assert_eq!(out_selected[0].0, "[mac][zzz-other]");
    }

    // ── badge color matches lane color (issue #53) ────────────────────

    #[test]
    fn head_badge_color_matches_lane_color_not_a_fixed_head_color() {
        // Regression: the checked-out HEAD branch's badge used to be forced
        // to a fixed color (green) regardless of the commit's actual lane
        // color, diverging from the graph line/node drawn in that lane.
        let theme = Theme::dark();
        for color_index in 0..theme.lane_colors.len() {
            if color_index == crate::graph::colors::MAIN_BRANCH_COLOR {
                continue; // main branch is blue either way — not the regression case
            }
            let out = optimize_branch_display(
                &["feature".to_string()],
                true, // is_head
                color_index,
                None,
                &theme,
                &[],
            );
            assert_eq!(out.len(), 1);
            assert_eq!(
                out[0].1.fg,
                Some(theme.lane_color(color_index)),
                "HEAD badge fg must equal the lane's own color at index {color_index}"
            );
        }
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
        let lw = render_cells_unicode(&mut spans, node, &theme, 0, cap, None, false);
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

    /// Render one full graph row against `open_prs` and return its `Line`. The
    /// real per-row render path, so tests observe exactly what the user sees.
    fn render_row(
        node: &GraphNode,
        open_prs: &HashMap<String, PrInfo>,
        mute_merges: bool,
    ) -> Line<'static> {
        let cols = MetadataColumns {
            author: false,
            hash: false,
            date: false,
            mute_merges,
            mute_base_merges: false,
            collapse_merges: false,
            avatars: false,
        };
        render_row_with(node, open_prs, cols, &HashSet::new())
    }

    /// Render one full graph row with explicit metadata columns and a set of
    /// base-update ("back-merge") commit OIDs (#55). The real per-row render
    /// path, so tests observe exactly what the user sees.
    fn render_row_with(
        node: &GraphNode,
        open_prs: &HashMap<String, PrInfo>,
        cols: MetadataColumns,
        base_update_merges: &HashSet<git2::Oid>,
    ) -> Line<'static> {
        let theme = Theme::dark();
        let pr_ctx = PrContext::new(open_prs);
        let ctx = RowRenderCtx {
            theme: &theme,
            now: Local::now(),
            pixel_mode: false,
            remotes: &[],
            open_prs,
            pr_ctx: &pr_ctx,
            merged_branches: &HashSet::new(),
            base_update_merges,
            metadata_columns: cols,
            graph_width: 4,
            total_width: 200,
            selected_branch_name: None,
            trace: None,
        };
        let (line, _chips) = render_graph_line(
            node,
            &ctx,
            RowFlags {
                is_selected: false,
                is_marked: false,
            },
        );
        line
    }

    /// The full rendered text of a row (all spans concatenated).
    fn row_text(node: &GraphNode, open_prs: &HashMap<String, PrInfo>) -> String {
        render_row(node, open_prs, true)
            .spans
            .iter()
            .map(|s| s.content.as_ref().to_string())
            .collect()
    }

    /// The style applied to `node`'s message span when rendered with the given
    /// mute-merges setting. Panics if the message span isn't found.
    fn message_style(node: &GraphNode, message: &str, mute_merges: bool) -> Style {
        let line = render_row(node, &HashMap::new(), mute_merges);
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

    // ── base-update ("back-merge") muting (#55) ──────────────────────

    /// MetadataColumns with only the given merge-muting toggles set.
    fn merge_cols(mute_merges: bool, mute_base_merges: bool, collapse_merges: bool) -> MetadataColumns {
        MetadataColumns {
            author: false,
            hash: false,
            date: false,
            mute_merges,
            mute_base_merges,
            collapse_merges,
            avatars: false,
        }
    }

    /// The style of the span whose content is exactly `content`, or None.
    fn find_style(line: &Line, content: &str) -> Option<Style> {
        line.spans
            .iter()
            .find(|s| s.content.as_ref() == content)
            .map(|s| s.style)
    }

    #[test]
    fn base_update_merge_message_is_strongly_muted_when_option_on() {
        let theme = Theme::dark();
        let node = merge_node_full(30, "Merge main into feature", [1, 2]);
        let mut set = HashSet::new();
        set.insert(oid(30));
        // Option ON + this commit is in the set → strong mute (muted fg + DIM).
        let line = render_row_with(&node, &HashMap::new(), merge_cols(false, true, false), &set);
        let style = find_style(&line, "Merge main into feature").expect("message span");
        assert_eq!(style.fg, Some(theme.text_muted), "strong-muted fg");
        assert!(style.add_modifier.contains(Modifier::DIM), "strong-muted DIM");
    }

    #[test]
    fn base_update_merge_is_not_muted_when_option_off_or_not_in_set() {
        let node = merge_node_full(30, "Merge main into feature", [1, 2]);
        let mut set = HashSet::new();
        set.insert(oid(30));
        // Option OFF → not muted even though the commit is in the set.
        let off = render_row_with(&node, &HashMap::new(), merge_cols(false, false, false), &set);
        let s_off = find_style(&off, "Merge main into feature").expect("span");
        assert_eq!(s_off.fg, None);
        assert!(!s_off.add_modifier.contains(Modifier::DIM));
        // Option ON but commit NOT in the set → not muted.
        let on_empty =
            render_row_with(&node, &HashMap::new(), merge_cols(false, true, false), &HashSet::new());
        let s_empty = find_style(&on_empty, "Merge main into feature").expect("span");
        assert_eq!(s_empty.fg, None);
        assert!(!s_empty.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn base_update_muting_dims_the_graph_connector_cells() {
        // The back-merge's own graph glyphs are force-dimmed so the noisy line
        // recedes (a merge-arc cell, not just the commit dot).
        let mut node = merge_node_full(30, "Merge main into feature", [1, 2]);
        node.cells = vec![CellType::Commit(0), CellType::MergeRight(1)];
        let mut set = HashSet::new();
        set.insert(oid(30));
        let line = render_row_with(&node, &HashMap::new(), merge_cols(false, true, false), &set);
        // The merge-arc glyph ╰ is present and carries DIM.
        let arc = find_style(&line, "╰").expect("merge-arc glyph present");
        assert!(arc.add_modifier.contains(Modifier::DIM), "connector cell dimmed: {arc:?}");
    }

    #[test]
    fn is_base_update_row_predicate_is_the_shared_contract() {
        // The single predicate both renderers key off. Only ON + merge + in-set +
        // not-HEAD qualifies; anything else must not mute.
        let mut set = HashSet::new();
        set.insert(oid(30));
        let merge = merge_node_full(30, "Merge main into feature", [1, 2]);
        assert!(is_base_update_row(&merge, true, &set), "qualifying back-merge");
        assert!(!is_base_update_row(&merge, false, &set), "toggle off never mutes");
        assert!(
            !is_base_update_row(&merge, true, &HashSet::new()),
            "not in set never mutes"
        );
        // HEAD is never muted even when it qualifies otherwise.
        let mut head = merge_node_full(30, "Merge main into feature", [1, 2]);
        head.is_head = true;
        assert!(!is_base_update_row(&head, true, &set), "HEAD is never muted");
        // A non-merge commit in the set is not a back-merge.
        let mut single = merge_node_full(30, "regular", [1, 2]);
        single.commit.as_mut().unwrap().parent_oids = vec![oid(1)];
        assert!(!is_base_update_row(&single, true, &set), "non-merge never mutes");
    }

    /// Render a one-row List through the real StatefulWidget highlight path and
    /// return the resulting buffer. `selected` drives the widget-level highlight
    /// patch (`Theme::selection_style`), exactly as the app does.
    fn render_list_row(line: Line<'static>, theme: &Theme, selected: bool) -> Buffer {
        let area = Rect::new(0, 0, 200, 1);
        let mut buf = Buffer::empty(area);
        let list = List::new(vec![ListItem::new(line)]).highlight_style(theme.selection_style());
        let mut state = ListState::default();
        if selected {
            state.select(Some(0));
        }
        StatefulWidget::render(list, area, &mut buf, &mut state);
        buf
    }

    /// The modifier of the first alphabetic cell in row 0 — the start of the
    /// message text (graph glyphs carry no letters).
    fn first_letter_modifier(buf: &Buffer) -> Modifier {
        for x in 0..buf.area.width {
            let cell = &buf[(x, 0)];
            if cell.symbol().chars().next().is_some_and(|c| c.is_alphabetic()) {
                return cell.modifier;
            }
        }
        panic!("no message letter cell found in buffer row");
    }

    #[test]
    fn selected_muted_row_is_bold_not_dim_through_the_list_layer() {
        // Render-level regression through the actual List/StatefulWidget highlight
        // patch (not render_row_with alone): a base-update-muted row's message is
        // DIM when unselected, but selecting it must yield BOLD *without* DIM —
        // the widget-level `sub_modifier(DIM)` makes BOLD win when selected, so
        // DIM+BOLD never renders muddy.
        let theme = Theme::dark();
        let mut node = merge_node_full(30, "Merge main into feature", [1, 2]);
        node.cells = vec![CellType::Commit(0)];
        let mut set = HashSet::new();
        set.insert(oid(30));
        // The muted row Line as the real renderer produces it (message is DIM).
        let line = render_row_with(&node, &HashMap::new(), merge_cols(false, true, false), &set);

        // Unselected: the message keeps its mute DIM (baseline the fix must not
        // regress).
        let unselected = first_letter_modifier(&render_list_row(line.clone(), &theme, false));
        assert!(
            unselected.contains(Modifier::DIM),
            "unselected muted message stays DIM: {unselected:?}"
        );

        // Selected: BOLD is present and DIM is gone.
        let selected = first_letter_modifier(&render_list_row(line, &theme, true));
        assert!(
            selected.contains(Modifier::BOLD),
            "selected row is BOLD: {selected:?}"
        );
        assert!(
            !selected.contains(Modifier::DIM),
            "selected row drops DIM (no muddy DIM+BOLD): {selected:?}"
        );
    }

    // ── collapse merge messages to a glyph (#59) ─────────────────────

    #[test]
    fn collapse_merge_replaces_message_with_the_merge_glyph() {
        let node = merge_node_full(31, "Merge branch 'topic'", [1, 2]);
        let line = render_row_with(&node, &HashMap::new(), merge_cols(false, false, true), &HashSet::new());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains(MERGE_ICON), "merge glyph shown: {text:?}");
        assert!(!text.contains("Merge branch 'topic'"), "no message text: {text:?}");
        // The glyph span is dimmed (collapse implies muting).
        let style = find_style(&line, MERGE_ICON).expect("glyph span");
        assert!(style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn collapse_leaves_non_merge_commits_alone() {
        // A non-merge commit keeps its message even with collapse on.
        let mut node = merge_node("real message");
        node.commit.as_mut().unwrap().parent_oids = vec![git2::Oid::zero()]; // 1 parent
        let line = render_row_with(&node, &HashMap::new(), merge_cols(false, false, true), &HashSet::new());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("real message"), "non-merge keeps message: {text:?}");
    }

    #[test]
    fn collapse_keeps_metadata_columns() {
        // Collapsing the message must not drop the hash/author/date columns.
        let node = merge_node_full(31, "Merge branch 'topic'", [1, 2]);
        let cols = MetadataColumns {
            author: true,
            hash: true,
            date: true,
            mute_merges: false,
            mute_base_merges: false,
            collapse_merges: true,
            avatars: false,
        };
        let line = render_row_with(&node, &HashMap::new(), cols, &HashSet::new());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains(MERGE_ICON), "glyph shown");
        // The short hash (`abc1234`) still renders in its column.
        assert!(text.contains("abc1234"), "hash column preserved: {text:?}");
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
    fn windowed_fold_matches_the_full_fold_sliced() {
        // A mix of commits and connectors; every window must produce exactly the
        // rows the full fold produces at those folded indices (#73 windowing).
        let a = commit_row(vec![CellType::Commit(0)]);
        let c1 = connector_node(vec![CellType::TeeRight(0)]);
        let b = commit_row(vec![CellType::Commit(0)]);
        let c2 = connector_node(vec![CellType::MergeLeft(1)]);
        let d = commit_row(vec![CellType::Commit(0)]);
        let e = commit_row(vec![CellType::Commit(0)]);
        let c3 = connector_node(vec![CellType::TeeRight(0)]); // trailing
        let nodes = [&a, &c1, &b, &c2, &d, &e, &c3];
        let base: Vec<(usize, &GraphNode)> =
            nodes.iter().enumerate().map(|(i, n)| (i, *n)).collect();

        let full = fold_rows(base.clone(), true);
        // Folded rows: a(0), b(2), d(4), e(5), then trailing c3 standalone.
        assert_eq!(full.len(), 5);

        let same = |r: &RenderRow, s: &RenderRow| {
            r.full_idx == s.full_idx && r.underlay == s.underlay
        };
        for win_start in 0..=full.len() {
            for win_end in win_start..=full.len() {
                let win = fold_rows_windowed(base.clone().into_iter(), win_start, win_end);
                let expected = &full[win_start..win_end];
                assert_eq!(
                    win.len(),
                    expected.len(),
                    "window [{win_start},{win_end}) row count"
                );
                for (w, e) in win.iter().zip(expected) {
                    assert!(
                        same(w, e),
                        "window [{win_start},{win_end}) row full_idx={} != {}",
                        w.full_idx,
                        e.full_idx
                    );
                }
            }
        }
    }

    #[test]
    fn windowed_fold_past_the_end_yields_no_rows() {
        let a = commit_row(vec![CellType::Commit(0)]);
        let b = commit_row(vec![CellType::Commit(0)]);
        let base = vec![(0, &a), (1, &b)];
        let win = fold_rows_windowed(base.into_iter(), 5, 9);
        assert!(win.is_empty(), "a window entirely past the graph is empty");
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
        let theme = Theme::dark();
        let (a, b) = (oid(1), oid(2));
        let mut node = node_with_cells(vec![CellType::Commit(0), CellType::Pipe(1)], false);
        // Commit dot is a self-edge on the lineage; the pipe is a self-edge off it.
        node.cell_oids = vec![(Some((a, a)), None), (Some((b, b)), None)];
        let lit: std::collections::HashMap<crate::git::graph::CellEdge, git2::Oid> =
            [((a, a), a)].into_iter().collect();

        let mut spans: Vec<Span> = Vec::new();
        render_cells_unicode(&mut spans, &node, &theme, 0, 8, Some(&lit), false);

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
        node.cell_oids = vec![(Some((oid(1), oid(1))), None), (Some((oid(2), oid(2))), None)];
        let mut spans: Vec<Span> = Vec::new();
        render_cells_unicode(&mut spans, &node, &theme, 0, 8, None, false);
        assert!(spans
            .iter()
            .all(|s| !s.style.add_modifier.contains(Modifier::DIM)));
    }

    #[test]
    fn folding_carries_connector_cell_oids_into_the_underlay() {
        // A fork-connector stroke's edge climbs from a feature child up to the fork.
        let (child, fork) = (oid(7), oid(8));
        let mut connector = connector_node(vec![CellType::Empty, CellType::TeeRight(0)]);
        connector.cell_oids = vec![(None, None), (Some((child, fork)), None)];
        let commit = commit_row(vec![CellType::Commit(0), CellType::Empty]);
        let base = vec![(0usize, &connector), (1usize, &commit)];

        let rows = fold_rows(base, true);
        assert_eq!(rows.len(), 1, "the connector folds into the commit row");
        assert_eq!(
            rows[0].underlay_oids.get(1).copied(),
            Some((Some((child, fork)), None)),
            "the connector's edge is preserved in the folded underlay"
        );
    }

    #[test]
    fn co_routed_traced_edge_does_not_light_or_recolor_a_sibling_spoke() {
        use crate::ui::graph_pixels::{CellShape, PixelCell};
        // The fdcc78b junction shape: tracing a branch whose fork stroke is
        // co-routed through a nearer sibling arm's ┴ (TeeUp). The sibling's
        // riser must stay dim in its OWN color — before this fix the co-routed
        // lit edge in the secondary slot lit the whole cell and recolored it
        // to the traced branch's lane color ("pink lead-in rendered yellow").
        let sibling_child = oid(1);
        let traced_child = oid(2);
        let fork = oid(3);
        let traced_edge = (traced_child, fork);
        let sibling_edge = (sibling_child, fork);
        let yellow = [205u8, 205, 0];
        let red = [205u8, 0, 0];
        let solid = |shape: CellShape, rgb: [u8; 3]| PixelCell {
            shape,
            color: rgb,
            secondary: rgb,
            dim: false,
            dim_secondary: false,
        };
        let mut cells = vec![
            solid(CellShape::TeeRight, [92, 92, 255]),
            solid(CellShape::Horizontal, red),
            solid(CellShape::TeeUp, yellow),
            solid(CellShape::Horizontal, red),
            solid(CellShape::MergeLeft, red),
        ];
        let oids: Vec<crate::git::graph::CellOids> = vec![
            (Some((oid(4), fork)), None),
            (Some(traced_edge), Some(sibling_edge)),
            (Some(sibling_edge), Some(traced_edge)), // ┴: own riser + co-route
            (Some(traced_edge), None),
            (Some(traced_edge), None),
        ];
        let lit: std::collections::HashMap<crate::git::graph::CellEdge, git2::Oid> =
            [(traced_edge, traced_child)].into_iter().collect();
        let lane_rgb: std::collections::HashMap<git2::Oid, [u8; 3]> =
            [(traced_child, red)].into_iter().collect();

        apply_trace_dim(&mut cells, &oids, &lit, &lane_rgb);

        assert!(cells[2].dim, "sibling ┴ riser must dim: its own edge is unlit");
        assert_eq!(
            cells[2].color, yellow,
            "sibling ┴ riser keeps its own color, not the traced branch's"
        );
        assert!(!cells[4].dim, "the traced branch's own turn stays lit");
        assert_eq!(cells[4].color, red, "lit stroke takes the traced lane color");
        assert!(!cells[1].dim, "shared run cell: primary (traced) edge lights it");
        assert!(cells[0].dim, "the trunk tee is off-lineage here and dims");
    }

    // ── dim_specs_window_core (windowed pixel-spec dimming) ──────────

    type WindowDimFixture = (
        Vec<crate::ui::graph_pixels::RowSpec>,
        Vec<Vec<crate::git::graph::CellOids>>,
        std::collections::HashMap<crate::git::graph::CellEdge, git2::Oid>,
        std::collections::HashMap<git2::Oid, [u8; 3]>,
    );

    /// A small fixture of undimmed base specs plus their per-cell edge OIDs,
    /// with one traced edge lit. `n` single-cell rows: even rows carry the
    /// traced edge (should stay lit), odd rows an unrelated edge (should dim).
    fn window_dim_fixture(n: usize) -> WindowDimFixture {
        use crate::ui::graph_pixels::{CellShape, PixelCell};
        let traced = (oid(2), oid(2));
        let other = (oid(9), oid(9));
        let blue = [0u8, 0, 255];
        let cell = || PixelCell {
            shape: CellShape::Pipe,
            color: blue,
            secondary: blue,
            dim: false,
            dim_secondary: false,
        };
        let base: Vec<_> = (0..n)
            .map(|_| crate::ui::graph_pixels::RowSpec {
                cells: vec![cell()],
                underlay: Vec::new(),
            })
            .collect();
        let oids: Vec<Vec<crate::git::graph::CellOids>> = (0..n)
            .map(|i| vec![(Some(if i % 2 == 0 { traced } else { other }), None)])
            .collect();
        let lit = [(traced, oid(2))].into_iter().collect();
        let lane_rgb = [(oid(2), [255u8, 0, 0])].into_iter().collect();
        (base, oids, lit, lane_rgb)
    }

    fn row_oids_of(oids: &[Vec<crate::git::graph::CellOids>]) -> Vec<RowOids<'_>> {
        oids.iter()
            .map(|o| RowOids {
                cells: o.as_slice(),
                underlay: &[],
            })
            .collect()
    }

    /// No base-update force-dim on any row (the trace-only fixtures).
    fn no_force(n: usize) -> Vec<bool> {
        vec![false; n]
    }

    #[test]
    fn window_dim_builds_only_inside_the_window() {
        let (base, oids, lit, lane_rgb) = window_dim_fixture(10);
        let ro = row_oids_of(&oids);
        let ff = no_force(base.len());
        let out = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, 3, 7);

        assert_eq!(out.len(), base.len(), "result keeps full length");
        for (i, spec) in out.iter().enumerate() {
            if (3..7).contains(&i) {
                assert_eq!(spec.cells.len(), 1, "row {i} is built");
                // Even rows carry the lit edge (stay bright), odd rows dim.
                assert_eq!(spec.cells[0].dim, i % 2 != 0, "row {i} dim flag");
            } else {
                assert!(
                    spec.cells.is_empty() && spec.underlay.is_empty(),
                    "row {i} outside window is an empty placeholder"
                );
            }
        }
    }

    #[test]
    fn windowed_rows_are_identical_to_full_range_dim() {
        // The core invariant the windowing relies on: a built row's content does
        // not depend on which window it was built in, so the on-screen result is
        // byte-identical to dimming every row.
        let (base, oids, lit, lane_rgb) = window_dim_fixture(12);
        let ro = row_oids_of(&oids);
        let ff = no_force(base.len());
        let full = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, 0, base.len());
        let windowed = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, 4, 9);
        for i in 4..9 {
            assert_eq!(windowed[i], full[i], "row {i} matches the full-range dim");
        }
    }

    #[test]
    fn empty_window_builds_nothing() {
        let (base, oids, lit, lane_rgb) = window_dim_fixture(5);
        let ro = row_oids_of(&oids);
        let ff = no_force(base.len());
        let out = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, 2, 2);
        assert_eq!(out.len(), 5);
        assert!(out.iter().all(|s| s.cells.is_empty()));
    }

    #[test]
    fn base_update_force_dim_dims_the_whole_pixel_connector() {
        // Pixel-mode mirror of `base_update_muting_dims_the_graph_connector_cells`
        // (the unicode test): a base-update back-merge row's entire connector is
        // force-dimmed in the windowed per-frame pass — every cell, regardless of
        // trace lineage, so the noisy line recedes exactly as in unicode mode.
        let (base, oids, lit, lane_rgb) = window_dim_fixture(4);
        let ro = row_oids_of(&oids);
        // Row 2 is a base-update merge; the rest are ordinary rows.
        let mut ff = no_force(base.len());
        ff[2] = true;
        let out = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, 0, base.len());

        // Row 2's connector is fully dimmed by force-dim...
        assert!(
            out[2].cells.iter().all(|c| c.dim && c.dim_secondary),
            "base-update row connector force-dimmed: {:?}",
            out[2].cells
        );
        // ...even though row 2 is an EVEN row that trace dimming would leave lit —
        // force-dim wins over trace, mirroring the unicode `force_dim || trace`.
        assert!(!out[0].cells[0].dim, "non-forced even row 0 stays lit by trace");
    }

    #[test]
    fn trace_dim_survives_width_truncation_in_unicode() {
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
            (Some((a, a)), None),
            (Some((oid(2), oid(2))), None),
            (Some((oid(3), oid(3))), None),
            (Some((oid(4), oid(4))), None),
        ];
        let lit: std::collections::HashMap<crate::git::graph::CellEdge, git2::Oid> =
            [((a, a), a)].into_iter().collect();

        let mut spans: Vec<Span> = Vec::new();
        // cap = 3 leaves room for 2 glyphs plus the `…` marker.
        render_cells_unicode(&mut spans, &node, &theme, 0, 3, Some(&lit), false);

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
