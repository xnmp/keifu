//! Graph view widget

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, StatefulWidget},
};
use chrono::{DateTime, Local};

use std::collections::{HashMap, HashSet};

use crate::{
    app::App,
    config::MetadataColumns,
    git::graph::{CellType, GraphNode},
    mouse::{ChipHit, ChipTarget},
    pr::{PrContext, PrInfo},
};

mod badges;
mod chips;
mod geometry;
mod metrics;
mod rows;

pub use badges::pr_for_branch_labels;
use badges::{merged_badge, merged_style, pr_for_row, PR_BADGE_ICON};

pub use chips::REMOTE_ONLY_ICON;
use chips::optimize_branch_display;

pub use geometry::{
    avatar_overlay_x, avatars_active, effective_graph_width, next_graph_cap, AVATAR_GAP_CELLS,
    AVATAR_IMAGE_CELLS, AVATAR_RESERVED_CELLS, GRAPH_LEADING_COLUMNS,
};
use geometry::{ellipsis_style, graph_truncation, pixel_row_cells};

pub(crate) use metrics::display_width;
use metrics::{format_date_field, truncate_to_width};

pub use rows::{visible_nodes, visible_rows, RenderRow};
use rows::{adjacent_cells, fold_rows_windowed, visible_row_window};

use super::{render_placeholder_block, theme::Theme, MIN_WIDGET_HEIGHT, MIN_WIDGET_WIDTH};

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
    // The physically-adjacent rows' drawn content (folded underlay + own
    // cells), used to bring their boundary-crossing transition curves into
    // this row as `incoming` tails.
    let neighbor = |j: usize| -> crate::ui::graph_pixels::NeighborRow<'_> {
        crate::ui::graph_pixels::NeighborRow {
            underlay: &rows[j].underlay,
            cells: &rows[j].node.cells,
        }
    };
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
                i.checked_sub(1).map(neighbor),
                (i + 1 < rows.len()).then(|| neighbor(i + 1)),
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
/// The window must equal the one the caller feeds `sync_frame`
/// (`graph_pixels::trace_window` while tracing, else `protocol_window`): only
/// those rows are rasterized, and only the on-screen subset
/// `[offset, offset + inner_h)` — always inside either window — is drawn
/// (`overlay_pixel_graph`), so the placeholders are never rasterized or shown.
/// Result is byte-identical to dimming every row and slicing the same window.
pub fn dim_pixel_specs_window(
    app: &App,
    base: &[crate::ui::graph_pixels::RowSpec],
    trace: Option<&crate::app::TraceCache>,
    win_start: usize,
    win_end: usize,
) -> Vec<crate::ui::graph_pixels::RowSpec> {
    // Lit edges and lane colors come from the frame cache (built once per
    // selection move, see `App::ensure_trace_cache`) instead of being
    // recomputed O(graph) on every redraw. With tracing off both are empty:
    // no cell is dimmed by tracing (base-update force-dim still runs).
    let empty_lit = std::collections::HashMap::new();
    let empty_rgb = std::collections::HashMap::new();
    let (lit, lane_rgb) = match trace {
        Some(t) => (&t.lit, &t.lane_rgb),
        None => (&empty_lit, &empty_rgb),
    };
    // Fold only the rows the window can draw: `fold_rows_windowed` allocates a
    // RenderRow (and per-connector underlay) for windowed rows alone, instead of
    // `visible_rows`' O(total-commits) full fold every redraw (#73). Rows are
    // dense in folded-index space, so `win_rows[k]` is folded row `win_start + k`
    // — the same index space `base`/the overlay use.
    // Fetch one extra row on each side of the window: a window-edge row's
    // incoming curve tails restyle from the *neighbour's* dimmed cells, so the
    // boundary neighbours' oids/force-dim must be available too.
    let ext_start = win_start.saturating_sub(1);
    let ext_end = win_end.saturating_add(1);
    let win_rows = if app.commit_filter.is_empty() {
        fold_rows_windowed(
            app.graph_layout.nodes.iter().enumerate(),
            ext_start,
            ext_end,
        )
    } else {
        fold_rows_windowed(
            app.visible_commit_indices
                .iter()
                .map(|&i| (i, &app.graph_layout.nodes[i])),
            ext_start,
            ext_end,
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
        let abs = ext_start + k;
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
    // Merged-lane dim (#108), gated the same way the unicode path is: dim on and
    // hide off. `None` = feature off, so the core skips the merged-lane pass.
    let merged_oids = (app.merged.dim && !app.merged.hide).then_some(&app.merged.lane_oids);
    dim_specs_window_core(
        base, &row_oids, &force_dim, lit, lane_rgb, merged_oids, win_start, win_end,
    )
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
#[allow(clippy::too_many_arguments)] // cohesive base specs + three dim sources + window bounds
fn dim_specs_window_core(
    base: &[crate::ui::graph_pixels::RowSpec],
    row_oids: &[RowOids],
    force_dim: &[bool],
    lit: &std::collections::HashMap<crate::git::graph::CellEdge, git2::Oid>,
    lane_rgb: &std::collections::HashMap<git2::Oid, [u8; 3]>,
    merged_oids: Option<&HashSet<git2::Oid>>,
    win_start: usize,
    win_end: usize,
) -> Vec<crate::ui::graph_pixels::RowSpec> {
    let mut out: Vec<crate::ui::graph_pixels::RowSpec> =
        vec![crate::ui::graph_pixels::RowSpec::default(); base.len()];
    let end = win_end.min(base.len());
    let dim_row = |i: usize| -> crate::ui::graph_pixels::RowSpec {
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
            // the cells that survived truncation. Only run the trace pass when
            // tracing is actually active (`lit` non-empty): with tracing off,
            // `apply_trace_dim` would dim EVERY cell (nothing is lit), so the
            // base specs must stay bright and let force-dim / merged-lane below
            // be the only per-frame dim sources — matching this function's
            // documented "no cell is dimmed by tracing when off" contract.
            if !lit.is_empty() {
                apply_trace_dim(&mut spec.cells, o.cells, lit, lane_rgb);
                apply_trace_dim(&mut spec.underlay, o.underlay, lit, lane_rgb);
            }
        }
        // Merged-lane dim (#108): grey the strokes touching a commit exclusive
        // to a merged branch's lane — exactly the strokes hide-merged removes.
        // Only ever SETS dim, so it composes on top of force-dim / trace above.
        if let (Some(m), Some(o)) = (merged_oids, row_oids.get(i)) {
            apply_merged_lane_dim(&mut spec.cells, o.cells, m);
            apply_merged_lane_dim(&mut spec.underlay, o.underlay, m);
        }
        spec
    };
    for (i, slot) in out.iter_mut().enumerate().take(end).skip(win_start) {
        *slot = dim_row(i);
    }
    // Restyle each incoming neighbour-curve tail from the dimmed neighbour's
    // landing-lane cell, so the two clipped halves of a boundary-crossing
    // cubic keep identical color/dim across the seam (the neighbour draws its
    // half in exactly that cell's post-dim style). Boundary neighbours sit one
    // row outside the window: dim them locally and discard, preserving the
    // out-of-window placeholder contract.
    let above_edge = win_start
        .checked_sub(1)
        .filter(|&i| i < base.len() && win_start < end)
        .map(dim_row);
    let below_edge = (win_start < end && end < base.len()).then(|| dim_row(end));
    let updates: Vec<(usize, Vec<crate::ui::graph_pixels::CellCurve>)> = (win_start..end)
        .filter(|&i| !out[i].incoming.is_empty())
        .map(|i| {
            let fixed = out[i]
                .incoming
                .iter()
                .map(|c| {
                    let neighbor = if c.from_above {
                        match i.checked_sub(1) {
                            Some(n) if n >= win_start => Some(&out[n]),
                            _ => above_edge.as_ref(),
                        }
                    } else if i + 1 < end {
                        Some(&out[i + 1])
                    } else {
                        below_edge.as_ref()
                    };
                    let mut c = *c;
                    if let Some(cell) = neighbor.and_then(|n| {
                        let slice = if c.from_underlay { &n.underlay } else { &n.cells };
                        slice.get(c.col as usize)
                    }) {
                        c.color = cell.color;
                        // A Tee source cell's boundary-crossing curve is its
                        // ARM (the trunk is a straight through-line that never
                        // crosses as a curve), so the tail follows the arm's
                        // flag (#113).
                        c.dim = match cell.shape {
                            crate::ui::graph_pixels::CellShape::TeeRight
                            | crate::ui::graph_pixels::CellShape::TeeLeft => cell.dim_secondary,
                            _ => cell.dim,
                        };
                    }
                    c
                })
                .collect();
            (i, fixed)
        })
        .collect();
    for (i, fixed) in updates {
        out[i].incoming = fixed;
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
        } else if matches!(pc.shape, CellShape::TeeRight | CellShape::TeeLeft) {
            // Two strokes (#113): the connector arm (primary edge, styled via
            // `dim_secondary`) and the lane's trunk through-line (secondary
            // edge — the Pipe the Tee replaced). A fork-connector hub carries
            // no secondary; its trunk then follows the primary like before.
            pc.dim_secondary = !is_lit(primary);
            let trunk = if secondary.is_some() { secondary } else { primary };
            pc.dim = !is_lit(trunk);
            if let Some(rgb) = color_of(primary) {
                pc.color = rgb;
            }
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

/// Set `dim` on every pixel cell whose edge touches a merged-lane commit (issue
/// #108) — exactly the strokes hide-merged would remove. Mirrors
/// `apply_trace_dim`'s per-edge mapping (a `HorizontalPipe` fades each stroke
/// from its own edge, a `Tee*` its arm from the primary and its trunk from the
/// lane's own secondary edge (#113), a commit dot from either edge, every other
/// shape from its primary edge) but ONLY dims: it never clears a `dim` flag and
/// never recolors, so it composes on top of whatever trace/force-dim already
/// decided.
fn apply_merged_lane_dim(
    cells: &mut [crate::ui::graph_pixels::PixelCell],
    oids: &[crate::git::graph::CellOids],
    merged: &HashSet<git2::Oid>,
) {
    use crate::git::graph::edge_touches_merged;
    use crate::ui::graph_pixels::CellShape;
    for (i, pc) in cells.iter_mut().enumerate() {
        let (primary, secondary) = oids.get(i).copied().unwrap_or((None, None));
        if pc.shape == CellShape::HorizontalPipe {
            // Primary = the horizontal stroke (drawn in `secondary`); secondary =
            // the vertical lane crossed underneath (drawn in `color`).
            if edge_touches_merged(primary, merged) {
                pc.dim_secondary = true;
            }
            if edge_touches_merged(secondary, merged) {
                pc.dim = true;
            }
        } else if matches!(pc.shape, CellShape::Commit { .. }) {
            if edge_touches_merged(primary, merged) || edge_touches_merged(secondary, merged) {
                pc.dim = true;
                pc.dim_secondary = true;
            }
        } else if matches!(pc.shape, CellShape::TeeRight | CellShape::TeeLeft) {
            // Two strokes (#113): the connector arm (primary edge) fades via
            // `dim_secondary`; the lane's trunk through-line fades only from
            // its OWN pass-through edge (secondary), so a merge arm into a
            // dimmed lane no longer greys the live trunk it leaves from. A
            // fork-connector hub has no secondary edge — its trunk follows the
            // primary as before.
            if edge_touches_merged(primary, merged) {
                pc.dim_secondary = true;
            }
            let trunk = if secondary.is_some() { secondary } else { primary };
            if edge_touches_merged(trunk, merged) {
                pc.dim = true;
            }
        } else if edge_touches_merged(primary, merged) {
            pc.dim = true;
            pc.dim_secondary = true;
        }
    }
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
        let merged_dim = app.merged.dim;
        // Merged-lane dimming (#108): grey the rows AND graph strokes exclusive
        // to a merged branch's lane. Gated on the dim setting with hide off
        // (hide already removes them from the graph). `lane_oids` stays populated
        // across the toggle, so gating here reflects a settings flip instantly
        // without a rebuild — `None` = feature off, no row/cell is dimmed by it.
        let merged_lane_oids = (app.merged.dim && !app.merged.hide)
            .then_some(&app.merged.lane_oids);
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
            // Lit edges from the frame cache (`ensure_trace_cache` ran at the
            // top of the draw) — no per-draw O(graph) rebuild.
            app.active_trace().map(|t| &t.lit)
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
            merged_dim,
            merged_lane_oids,
            base_update_merges,
            metadata_columns,
            graph_width,
            total_width: inner_width,
            selected_branch_name,
            trace,
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

/// The bare merge glyph a collapsed merge commit shows in place of its message
/// (#59), and the icon [`merged_badge`] prefixes its "merged" label with — the
/// same nf-oct-git_merge icon, so a collapsed merge or a landed branch both
/// read as "this is a merge" at a glance.
const MERGE_ICON: &str = "\u{f419}"; // nf-oct-git_merge

/// Style for a *merged branch name chip* (issue #90): take the chip's own
/// unmerged style and mute its lane color toward the recessive tone (via
/// [`Theme::merged_chip_color`]) rather than flattening it to grey, then DIM it.
/// A landed branch's chip thus still reads as *its* lane — only faded — keeping
/// the branch identity the old flat-grey `merged_style` erased, while staying
/// visibly distinct from an active unmerged chip. Selection's REVERSED and any
/// HEAD BOLD carried on `base` survive; the highlighted row's `selection_style`
/// still subtracts DIM so a selected merged chip never renders muddy.
fn merged_chip_style(base: Style, theme: &Theme) -> Style {
    let muted = base
        .fg
        .map_or(theme.text_muted, |c| theme.merged_chip_color(c));
    base.fg(muted).add_modifier(Modifier::DIM)
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
    /// Whether a merged branch shown in the graph should render dimmed (issue
    /// #106) — chip color + "merged" badge. Independent of whether merged
    /// branches are hidden entirely (that's decided upstream: a hidden branch
    /// never reaches this row at all). Applies uniformly regardless of how
    /// `merged_branches` classified the branch (ancestry, fast-forward, or
    /// squash all live in the same set).
    merged_dim: bool,
    /// Commits exclusive to a merged branch's lane (issue #108), or `None` when
    /// merged-lane dimming is off (`dim` off or `hide` on). When `Some`, a row
    /// whose commit is in the set greys its text like a muted merge, and its
    /// graph strokes (any cell edge touching one of these commits) dim — the
    /// same strokes hide-merged would remove.
    merged_lane_oids: Option<&'a HashSet<git2::Oid>>,
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

    // Merged-lane row (#108): a commit exclusive to a merged branch's lane greys
    // its message + metadata (like a muted merge) and dims its graph strokes.
    // `ctx.merged_lane_oids` is `None` when the feature is off, so this is a
    // cheap constant `false` in the common case.
    let is_merged_lane = node
        .commit
        .as_ref()
        .is_some_and(|c| ctx.merged_lane_oids.is_some_and(|s| s.contains(&c.oid)));

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
        // Merged-lane strokes (#108) dim per-cell, composing with the two above.
        left_width = render_cells_unicode(
            &mut spans,
            node,
            ctx.theme,
            left_width,
            ctx.graph_width,
            ctx.trace,
            is_base_update,
            ctx.merged_lane_oids,
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

    render_graph_line_tail(spans, left_width, node, ctx, flags, is_base_update, is_merged_lane)
}

/// Render the Unicode box-drawing glyphs for a row's cells into `spans`, capped
/// at `cap` graph columns, returning the updated `left_width`. When the row
/// overflows the cap, the last column becomes a dim `…`.
#[allow(clippy::too_many_arguments)] // cohesive per-row render + dim-source inputs
fn render_cells_unicode(
    spans: &mut Vec<Span<'_>>,
    node: &GraphNode,
    theme: &Theme,
    mut left_width: usize,
    cap: usize,
    trace: Option<&std::collections::HashMap<crate::git::graph::CellEdge, git2::Oid>>,
    force_dim: bool,
    merged_oids: Option<&HashSet<git2::Oid>>,
) -> usize {
    // `budget` graph columns are available for glyphs; when truncating, one more
    // column holds the `…`.
    let (budget, ellipsis) = graph_truncation(node.cells.len(), cap);

    // Whether the cell at `idx` should be dimmed. Three independent sources
    // compose (any one dims), mirroring the pixel path:
    //   - `force_dim` (a base-update back-merge row, #55) dims the whole connector;
    //   - a merged-lane stroke (#108): the cell's edge touches a commit exclusive
    //     to a merged branch's lane — exactly a stroke hide-merged would remove;
    //   - trace dim: tracing is active and the cell is not lit by the selection.
    let is_dim = |idx: usize| -> bool {
        let oids = node.cell_oids.get(idx).copied().unwrap_or((None, None));
        // A Tee glyph (├ / ┤) is dominated by its vertical bar — the lane's own
        // trunk through-line — so its merged-lane dim follows the trunk's edge
        // (secondary, the Pipe the Tee replaced; #113): a merge arm into a
        // dimmed lane must not grey the live trunk's glyph. One glyph can't
        // split strokes, so the arm's stub inherits the trunk's style here;
        // the pixel renderer fades each stroke independently.
        let merged_dims = |idx: usize, oids: crate::git::graph::CellOids, m: &HashSet<git2::Oid>| {
            match node.cells.get(idx) {
                Some(CellType::TeeRight(_) | CellType::TeeLeft(_)) => {
                    let trunk = if oids.1.is_some() { oids.1 } else { oids.0 };
                    crate::git::graph::edge_touches_merged(trunk, m)
                }
                _ => crate::git::graph::cell_touches_merged(oids, m),
            }
        };
        force_dim
            || merged_oids.is_some_and(|m| merged_dims(idx, oids, m))
            || trace.is_some_and(|lit| !crate::git::graph::cell_is_traced(oids, lit))
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
    is_merged_lane: bool,
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
    // PR-landed subject rewrite (#99): a commit whose raw subject is GitHub
    // merge/squash machinery — "Merge pull request #123 from …" or "Title
    // (#123)" — reads better as "<icon> #123 <PR title>". Qualifies via the
    // same `is_pr_merge` detection for merge commits (so it agrees with the
    // greying above), or the squash-subject parser for single-parent commits.
    // `collapse_merge` is a stronger reduction (drops the message entirely),
    // so it wins when both would otherwise apply — resolved down where the
    // message text is actually chosen.
    let pr_landed_subject = ctx.metadata_columns.pr_subjects.then(|| {
        if is_pr_merge {
            crate::pr::pr_landed_subject(&commit.message, &commit.full_message, true)
        } else if commit.parent_oids.len() == 1 {
            crate::pr::pr_landed_subject(&commit.message, &commit.full_message, false)
        } else {
            None
        }
    }).flatten();
    // #92: any of the muted-merge categories above should read grey across the
    // whole row, not just the message — DIM alone is too subtle (terminal
    // support is inconsistent), so the hash/author/date columns get the same
    // explicit `text_muted` foreground the message uses.
    // A merged-lane commit (#108) reads muted too — its message, hash, author,
    // and date all grey, matching the other muted categories (#92).
    let row_is_muted =
        is_base_update || is_pr_merge || muted_merge || collapse_merge || is_merged_lane;
    let hash_style = if row_is_muted {
        Style::default().fg(ctx.theme.text_muted)
    } else {
        Style::default().fg(ctx.theme.hash_color)
    };
    let author_style = if row_is_muted {
        Style::default().fg(ctx.theme.text_muted)
    } else {
        Style::default().fg(ctx.theme.author_color)
    };
    let date_style = if row_is_muted {
        Style::default().fg(ctx.theme.text_muted)
    } else {
        Style::default().fg(ctx.theme.date_color)
    };
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
    } else if muted_merge || collapse_merge || is_merged_lane {
        // Merged-lane commits (#108) share the ordinary muted-merge treatment:
        // an explicit muted foreground plus DIM.
        Style::default()
            .fg(ctx.theme.text_muted)
            .add_modifier(Modifier::DIM)
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
    let pr_badge = pr_for_row(
        commit.oid,
        &node.branch_names,
        ctx.remotes,
        ctx.pr_ctx,
        ctx.open_prs,
        ctx.theme,
    );
    // Chip plus a trailing space.
    let pr_badge_width = pr_badge.as_ref().map_or(0, |b| display_width(&b.text) + 1);

    // Merged badge: shown when one of this node's local branches has already
    // landed on the trunk (merge commit, fast-forward, or squash) AND the
    // dim-merged-branches setting is on (issue #106) — the branch chips are
    // dimmed to match. Hidden merged branches never reach this row at all (the
    // hide-merged toggle removes them from the graph entirely upstream), so
    // `merged_dim` is the only gate a shown merged branch still needs; it
    // applies identically regardless of classification kind.
    let has_merged_branch = ctx.merged_dim
        && node
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
        .map(|(i, chip)| display_width(&chip.label) + if i > 0 { 1 } else { 0 })
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

    // Render open-PR badge (#98: moved before the branch pill so PR context
    // leads the row instead of trailing it). Emits nothing when absent, so
    // there's no leading gap before the branch labels on PR-less rows.
    if let Some(badge) = &pr_badge {
        let style = Style::default().fg(badge.color).add_modifier(Modifier::BOLD);
        let chip_start = left_width;
        left_width += display_width(&badge.text) + 1;
        chips.push(ChipHit {
            x_start: chip_start as u16,
            x_end: (chip_start + display_width(&badge.text)) as u16,
            target: ChipTarget::PrBadge,
        });
        spans.push(Span::styled(badge.text.clone(), style));
        spans.push(Span::raw(" "));
    }

    // Render branch labels
    for (i, chip) in branch_display.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
            left_width += 1;
        }
        let chip_start = left_width;
        left_width += display_width(&chip.label);
        // Each chip already carries the branch a click on it resolves to (folded
        // into chip construction, #77), so the render tail no longer re-derives it.
        let is_merged = ctx.merged_dim
            && chip
                .branch
                .as_deref()
                .is_some_and(|n| ctx.merged_branches.contains(n));
        if let Some(name) = &chip.branch {
            chips.push(ChipHit {
                x_start: chip_start as u16,
                x_end: left_width as u16,
                target: ChipTarget::Branch(name.clone()),
            });
        }
        // A merged branch's chip keeps its lane hue, muted and dimmed (#90), so
        // it recedes while still reading as its own branch.
        let style = if is_merged {
            merged_chip_style(chip.style, ctx.theme)
        } else {
            chip.style
        };
        spans.push(Span::styled(chip.label.clone(), style));
    }
    if !branch_display.is_empty() {
        spans.push(Span::raw(" "));
        left_width += 1;
    }

    // Render merged badge (after branch labels)
    if let Some(badge) = &merged_badge_text {
        left_width += display_width(badge) + 1;
        spans.push(Span::styled(badge.clone(), merged_style(ctx.theme)));
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
    // Collapse (#59) replaces the whole message with a single merge glyph — the
    // strongest reduction, so it wins over a PR-subject rewrite (#99) on the
    // same row. Otherwise a qualifying PR-landed commit shows "<icon> <title>"
    // — no number (#101: the parsed number is sometimes an issue reference,
    // and the title alone reads cleaner); anything else truncates the raw
    // message as usual.
    let message = if collapse_merge {
        MERGE_ICON.to_string()
    } else if let Some((_, title)) = &pr_landed_subject {
        truncate_to_width(&format!("{PR_BADGE_ICON} {title}"), available_for_message)
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
    use super::rows::fold_rows;
    use crate::pr::{CiStatus, MergeState, ReviewState};

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

    // ── open-PR badge matching ───────────────────────────────────────

    fn pr(number: u64) -> PrInfo {
        PrInfo {
            number,
            url: format!("https://github.com/o/r/pull/{number}"),
            title: "t".to_string(),
            ci: CiStatus::None,
            review: ReviewState::None,
            merge_state: MergeState::Clear,
            outside_activity: false,
            head_oid: None,
        }
    }

    #[test]
    fn merged_chip_keeps_lane_hue_muted_not_flat_grey() {
        // A merged branch chip (#90) must derive from its own lane color, muted —
        // NOT collapse to the flat `text_muted` grey the old style used, and NOT
        // stay identical to the active unmerged chip.
        let theme = Theme::dark();
        let lane = theme.lane_color(1); // second lane, a distinct hue
        let unmerged = Style::default().fg(lane);
        let merged = merged_chip_style(unmerged, &theme);

        let fg = merged.fg.expect("merged chip keeps an fg color");
        // Derived from the lane hue, not the flat muted grey.
        assert_ne!(fg, theme.text_muted, "must not be flat grey");
        // Actually muted — moved off the raw lane color.
        assert_ne!(fg, lane, "must be muted, not the raw lane color");
        // Matches the theme's blend helper exactly (derivation is lane-based).
        assert_eq!(fg, theme.merged_chip_color(lane));
        // And it is visibly distinct from the unmerged chip's style.
        assert_ne!(merged, unmerged, "merged chip differs from the unmerged chip");
        assert!(merged.add_modifier.contains(Modifier::DIM), "merged chip is dimmed");
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
    fn pr_badge_renders_before_branch_pill() {
        // #98: the PR badge should lead the row, not trail the branch pill.
        let open = open_map(vec![("feat", pr_head(77, 5))]);
        let node = commit_node(5, "head of the feature", &["feat"]);
        let line = render_row(&node, &open, false);

        let badge_idx = line
            .spans
            .iter()
            .position(|s| s.content.contains("#77"))
            .unwrap_or_else(|| panic!("PR badge span not found: {line:?}"));
        let branch_idx = line
            .spans
            .iter()
            .position(|s| s.content.as_ref() == "[feat]")
            .unwrap_or_else(|| panic!("branch pill span not found: {line:?}"));
        assert!(
            badge_idx < branch_idx,
            "PR badge should render before the branch pill: {line:?}"
        );

        // Exactly one separating space span between the badge and the pill —
        // no double space, no missing gap.
        let between: Vec<&str> = line.spans[badge_idx + 1..branch_idx]
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            between,
            vec![" "],
            "exactly one space between the badge and the pill: {line:?}"
        );
    }

    #[test]
    fn pr_badge_absent_leaves_single_separator_before_branch_pill() {
        // A row with a branch pill but no PR badge must still be preceded by
        // just the ordinary one-space graph/content separator — not a stray
        // extra gap left over from the (now absent) badge slot.
        let node = commit_node(9, "no pr here", &["feat"]);
        let line = render_row(&node, &HashMap::new(), false);
        let branch_idx = line
            .spans
            .iter()
            .position(|s| s.content.as_ref() == "[feat]")
            .unwrap_or_else(|| panic!("branch pill span not found: {line:?}"));
        assert_eq!(
            line.spans[branch_idx - 1].content.as_ref(),
            " ",
            "exactly one separator space before the pill: {line:?}"
        );
        assert_ne!(
            line.spans[branch_idx - 2].content.as_ref(),
            " ",
            "no double space before the branch pill when there's no PR badge: {line:?}"
        );
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
            pr_subjects: true,
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
        let lw = render_cells_unicode(&mut spans, node, &theme, 0, cap, None, false, None);
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
            pr_subjects: true,
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
            merged_dim: true,
            merged_lane_oids: None,
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

    /// Render one full graph row with an explicit merged-branch classification
    /// set and the dim-merged-branches setting (issue #106) — the real per-row
    /// render path, so tests observe exactly what the user sees.
    fn render_row_with_merged(
        node: &GraphNode,
        merged_branches: &HashSet<String>,
        merged_dim: bool,
    ) -> Line<'static> {
        let cols = MetadataColumns {
            author: false,
            hash: false,
            date: false,
            mute_merges: true,
            mute_base_merges: false,
            collapse_merges: false,
            avatars: false,
            pr_subjects: true,
        };
        let theme = Theme::dark();
        let open_prs = HashMap::new();
        let pr_ctx = PrContext::new(&open_prs);
        let ctx = RowRenderCtx {
            theme: &theme,
            now: Local::now(),
            pixel_mode: false,
            remotes: &[],
            open_prs: &open_prs,
            pr_ctx: &pr_ctx,
            merged_branches,
            merged_dim,
            merged_lane_oids: None,
            base_update_merges: &HashSet::new(),
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

    // ── merged-branch dimming respects its own setting (#106) ─────────

    /// A single-parent commit row carrying the given branch names — the shape
    /// of a branch-tip row. Both ancestry/fast-forward and squash merges land
    /// an ordinary single-parent commit on the branch itself (only the trunk
    /// side gets a merge commit, or none at all for squash), and both
    /// classifications flow into the same `merged_branches` set, so one
    /// fixture shape covers either origin.
    fn branch_tip_node(message: &str, branch_names: &[&str]) -> GraphNode {
        use crate::git::CommitInfo;
        let commit = CommitInfo {
            oid: git2::Oid::zero(),
            short_id: "abc1234".to_string(),
            author_name: "a".to_string(),
            author_email: "a@b".to_string(),
            timestamp: Local::now(),
            message: message.to_string(),
            full_message: message.to_string(),
            parent_oids: vec![git2::Oid::zero()],
        };
        GraphNode {
            commit: Some(commit),
            lane: 0,
            color_index: 0,
            branch_names: branch_names.iter().map(|s| s.to_string()).collect(),
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

    /// The branch chip span whose text contains `name`. Panics if absent.
    fn find_chip<'a>(line: &'a Line<'static>, name: &str) -> &'a Span<'static> {
        line.spans
            .iter()
            .find(|s| s.content.as_ref().contains(name))
            .unwrap_or_else(|| panic!("branch chip {name:?} not found in {line:?}"))
    }

    /// Whether the row carries the exact "merged" badge span (icon + "merged"),
    /// as opposed to merely a branch name that happens to contain the substring
    /// "merged" (e.g. `feature/ancestry-merged`).
    fn has_merged_badge(line: &Line<'static>) -> bool {
        let badge = merged_badge();
        line.spans.iter().any(|s| s.content.as_ref() == badge)
    }

    #[test]
    fn merged_branch_row_dims_when_dim_setting_on() {
        let node = branch_tip_node("ancestry commit", &["feature/ancestry-landed"]);
        let merged: HashSet<String> = ["feature/ancestry-landed".to_string()].into_iter().collect();

        let line = render_row_with_merged(&node, &merged, true);
        let chip = find_chip(&line, "feature/ancestry-landed");
        assert!(
            chip.style.add_modifier.contains(Modifier::DIM),
            "merged chip dims when the setting is on: {chip:?}"
        );
        assert!(
            has_merged_badge(&line),
            "merged badge shown when the setting is on"
        );
    }

    #[test]
    fn merged_branch_row_renders_normally_when_dim_setting_off() {
        let node = branch_tip_node("ancestry commit", &["feature/ancestry-landed"]);
        let merged: HashSet<String> = ["feature/ancestry-landed".to_string()].into_iter().collect();

        let line = render_row_with_merged(&node, &merged, false);
        let chip = find_chip(&line, "feature/ancestry-landed");
        assert!(
            !chip.style.add_modifier.contains(Modifier::DIM),
            "dim setting off renders the branch chip normally: {chip:?}"
        );
        assert!(
            !has_merged_badge(&line),
            "no merged badge when the dim setting is off"
        );
    }

    #[test]
    fn squash_merged_branch_dims_when_dim_setting_on() {
        // #106: squash-classified branches flow through the same
        // `merged_branches` set as ancestry/fast-forward ones (there is no
        // separate squash code path in the chip-dim logic), so they must dim
        // exactly like an ancestry-landed branch when the setting is on.
        let node = branch_tip_node("feature commit 2", &["feature/squash-me"]);
        let merged: HashSet<String> = ["feature/squash-me".to_string()].into_iter().collect();

        let line = render_row_with_merged(&node, &merged, true);
        let chip = find_chip(&line, "feature/squash-me");
        assert!(
            chip.style.add_modifier.contains(Modifier::DIM),
            "squash-merged chip dims when the setting is on: {chip:?}"
        );
    }

    #[test]
    fn squash_merged_branch_renders_normally_when_dim_setting_off() {
        // The regression in #106: toggling the dim setting off left
        // squash-merged branches greyed regardless, because nothing gated the
        // chip/badge styling on the setting at all. This must render exactly
        // like an unmerged branch once the setting is off.
        let node = branch_tip_node("feature commit 2", &["feature/squash-me"]);
        let merged: HashSet<String> = ["feature/squash-me".to_string()].into_iter().collect();

        let line = render_row_with_merged(&node, &merged, false);
        let chip = find_chip(&line, "feature/squash-me");
        assert!(
            !chip.style.add_modifier.contains(Modifier::DIM),
            "squash-merged chip renders normally when the setting is off: {chip:?}"
        );
        assert!(
            !has_merged_badge(&line),
            "no merged badge for a squash-merged branch when the dim setting is off"
        );
    }

    #[test]
    fn unmerged_branch_never_dims_regardless_of_dim_setting() {
        // A branch absent from `merged_branches` must never pick up the merged
        // styling, whatever the dim setting is — the classification set is
        // still the primary gate.
        let node = branch_tip_node("wip commit", &["feature/still-open"]);
        let empty: HashSet<String> = HashSet::new();

        for dim in [true, false] {
            let line = render_row_with_merged(&node, &empty, dim);
            let chip = find_chip(&line, "feature/still-open");
            assert!(
                !chip.style.add_modifier.contains(Modifier::DIM),
                "unmerged branch never dims (dim setting = {dim}): {chip:?}"
            );
            assert!(
                !has_merged_badge(&line),
                "unmerged branch never shows the merged badge (dim setting = {dim})"
            );
        }
    }

    // ── merged-lane dimming: whole rows grey (#108) ──────────────────────

    /// Render one full graph row with a set of merged-LANE commit OIDs (#108)
    /// and every metadata column on, so tests can assert the message and the
    /// hash/author/date columns all grey. `lane_oids = None` renders the
    /// feature off (the settings toggle held off / hide on).
    fn render_row_with_merged_lane(
        node: &GraphNode,
        lane_oids: Option<&HashSet<git2::Oid>>,
    ) -> Line<'static> {
        let cols = MetadataColumns {
            author: true,
            hash: true,
            date: true,
            mute_merges: false,
            mute_base_merges: false,
            collapse_merges: false,
            avatars: false,
            pr_subjects: true,
        };
        let theme = Theme::dark();
        let open_prs = HashMap::new();
        let pr_ctx = PrContext::new(&open_prs);
        let ctx = RowRenderCtx {
            theme: &theme,
            now: Local::now(),
            pixel_mode: false,
            remotes: &[],
            open_prs: &open_prs,
            pr_ctx: &pr_ctx,
            merged_branches: &HashSet::new(),
            merged_dim: false,
            merged_lane_oids: lane_oids,
            base_update_merges: &HashSet::new(),
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

    #[test]
    fn merged_lane_row_greys_message_and_metadata() {
        // A commit exclusive to a merged branch's lane reads grey across the
        // whole row — message DIM+greyed, and the metadata columns greyed (#92).
        let theme = Theme::dark();
        let node = commit_node(7, "landed feature work", &[]);
        let lane: HashSet<git2::Oid> = [oid(7)].into_iter().collect();

        let line = render_row_with_merged_lane(&node, Some(&lane));

        let msg = find_style(&line, "landed feature work").expect("message span");
        assert_eq!(
            msg.fg,
            Some(theme.text_muted),
            "merged-lane message greyed: {msg:?}"
        );
        assert!(
            msg.add_modifier.contains(Modifier::DIM),
            "merged-lane message DIM: {msg:?}"
        );
        // author_name is "a", padded to 8; author_color != text_muted in dark.
        let author = find_style(&line, &format!("{:<8}", "a")).expect("author span");
        assert_eq!(
            author.fg,
            Some(theme.text_muted),
            "merged-lane author column greyed: {author:?}"
        );
    }

    #[test]
    fn merged_lane_row_renders_normally_when_feature_off() {
        // `None` = dim setting off (or hide on): the same row renders exactly
        // like an ordinary commit — no grey, no DIM.
        let theme = Theme::dark();
        let node = commit_node(7, "landed feature work", &[]);

        let line = render_row_with_merged_lane(&node, None);

        let msg = find_style(&line, "landed feature work").expect("message span");
        assert_eq!(msg.fg, None, "feature off: message not greyed: {msg:?}");
        assert!(
            !msg.add_modifier.contains(Modifier::DIM),
            "feature off: message not DIM: {msg:?}"
        );
        let author = find_style(&line, &format!("{:<8}", "a")).expect("author span");
        assert_eq!(
            author.fg,
            Some(theme.author_color),
            "feature off: author column keeps its own color: {author:?}"
        );
    }

    #[test]
    fn commit_outside_the_merged_lane_stays_bright_when_feature_on() {
        // Only commits IN the lane set dim — a trunk commit rendered while the
        // feature is on keeps full-strength styling.
        let theme = Theme::dark();
        let trunk = commit_node(3, "trunk work", &[]);
        let lane: HashSet<git2::Oid> = [oid(7)].into_iter().collect(); // trunk is oid(3)

        let line = render_row_with_merged_lane(&trunk, Some(&lane));

        let msg = find_style(&line, "trunk work").expect("message span");
        assert_eq!(msg.fg, None, "non-lane commit not greyed: {msg:?}");
        let author = find_style(&line, &format!("{:<8}", "a")).expect("author span");
        assert_eq!(
            author.fg,
            Some(theme.author_color),
            "non-lane author keeps its own color: {author:?}"
        );
    }

    // ── Tee cells: arm dims independently of the trunk (#113) ────────

    #[test]
    fn tee_trunk_stays_bright_when_only_the_arm_touches_a_merged_lane() {
        // The reported bug: a merge of the trunk INTO a feature branch puts a
        // Tee on the trunk lane; the arm edge touches the merged lane's commit
        // and the whole cell dimmed — greying a segment of the live trunk. The
        // arm must fade via `dim_secondary` while the trunk's own edge keeps
        // `dim` off.
        use crate::ui::graph_pixels::CellShape;
        let mut cells = vec![crate::ui::graph_pixels::PixelCell {
            shape: CellShape::TeeRight,
            color: [0, 255, 0],
            secondary: [0, 255, 0],
            dim: false,
            dim_secondary: false,
            curved_above: false,
            curved_below: false,
            spoke_on_dot: false,
        }];
        // primary = arm edge (merge commit → trunk parent); the merge commit
        // is in the dim set. secondary = the lane's own pass-through edge.
        let oids = vec![(
            Some((oid(5), oid(1))),
            Some((oid(2), oid(1))),
        )];
        let merged: HashSet<git2::Oid> = [oid(5)].into_iter().collect();
        apply_merged_lane_dim(&mut cells, &oids, &merged);
        assert!(cells[0].dim_secondary, "the arm into the dim lane fades");
        assert!(!cells[0].dim, "the live trunk through-line stays bright");
    }

    #[test]
    fn fork_hub_tee_without_a_lane_edge_still_dims_wholly() {
        // A fork-connector hub Tee carries no secondary edge; its trunk then
        // follows the primary as before — no behavior change for fork rows.
        use crate::ui::graph_pixels::CellShape;
        let mut cells = vec![crate::ui::graph_pixels::PixelCell {
            shape: CellShape::TeeRight,
            color: [0, 255, 0],
            secondary: [0, 255, 0],
            dim: false,
            dim_secondary: false,
            curved_above: false,
            curved_below: false,
            spoke_on_dot: false,
        }];
        let oids = vec![(Some((oid(5), oid(1))), None)];
        let merged: HashSet<git2::Oid> = [oid(5)].into_iter().collect();
        apply_merged_lane_dim(&mut cells, &oids, &merged);
        assert!(cells[0].dim, "no lane edge: trunk follows the primary");
        assert!(cells[0].dim_secondary, "arm dims too");
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
        let theme = Theme::dark();
        let node = merge_node("merge-branch-into-main");
        // Toggle ON: the merge's message text is dimmed AND explicitly greyed.
        // DIM alone isn't reliable — terminal DIM support is inconsistent, so
        // muted merges must pair it with an explicit `text_muted` fg (#92).
        let muted = message_style(&node, "merge-branch-into-main", true);
        assert!(
            muted.add_modifier.contains(Modifier::DIM),
            "muted merge message should be DIM: {muted:?}"
        );
        assert_eq!(
            muted.fg,
            Some(theme.text_muted),
            "muted merge message should have an explicit grey fg, not rely on DIM alone: {muted:?}"
        );
        // Toggle OFF: the message renders at full strength.
        let normal = message_style(&node, "merge-branch-into-main", false);
        assert!(
            !normal.add_modifier.contains(Modifier::DIM),
            "un-muted merge message must not be DIM: {normal:?}"
        );
        assert_eq!(normal.fg, None, "un-muted merge message must not be greyed");
    }

    #[test]
    fn muted_merge_greys_metadata_columns_not_normal_rows() {
        // #92: the whole row should read grey for a muted merge, not just the
        // message — hash/author/date columns get the same explicit
        // `text_muted` foreground.
        let theme = Theme::dark();
        let cols = MetadataColumns {
            author: true,
            hash: true,
            date: true,
            mute_merges: true,
            mute_base_merges: false,
            collapse_merges: false,
            avatars: false,
            pr_subjects: true,
        };
        let author_text = format!("{:<8}", "a"); // author_name is "a", padded to 8
        let hash_text = "abc1234"; // short_id, already 7 chars

        let merge = merge_node("merge-branch-into-main");
        let line = render_row_with(&merge, &HashMap::new(), cols, &HashSet::new());
        let author_style = find_style(&line, &author_text).expect("author span");
        let hash_style = find_style(&line, hash_text).expect("hash span");
        assert_eq!(
            author_style.fg,
            Some(theme.text_muted),
            "muted merge author column should be greyed: {author_style:?}"
        );
        assert_eq!(
            hash_style.fg,
            Some(theme.text_muted),
            "muted merge hash column should be greyed: {hash_style:?}"
        );

        // A normal (non-merge) row keeps its ordinary per-column metadata
        // colors (not the shared muted grey). Note: in the dark theme
        // `hash_color`/`date_color` happen to equal `text_muted`'s value, so
        // "not muted" is asserted via the actual expected color rather than a
        // simple inequality against `text_muted`.
        let normal = commit_node(1, "normal commit", &[]);
        let normal_line = render_row_with(&normal, &HashMap::new(), cols, &HashSet::new());
        let normal_author = find_style(&normal_line, &author_text).expect("author span");
        let normal_hash = find_style(&normal_line, hash_text).expect("hash span");
        assert_eq!(
            normal_author.fg,
            Some(theme.author_color),
            "normal row author column should use author_color, not be greyed: {normal_author:?}"
        );
        assert_eq!(
            normal_hash.fg,
            Some(theme.hash_color),
            "normal row hash column should use hash_color: {normal_hash:?}"
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
            pr_subjects: true,
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
            pr_subjects: true,
        };
        let line = render_row_with(&node, &HashMap::new(), cols, &HashSet::new());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains(MERGE_ICON), "glyph shown");
        // The short hash (`abc1234`) still renders in its column.
        assert!(text.contains("abc1234"), "hash column preserved: {text:?}");
    }

    // ── PR-landed subject rewrite (#99) ───────────────────────────────

    /// MetadataColumns with only `pr_subjects` and `collapse_merges` set.
    fn cols_pr_subjects(pr_subjects: bool, collapse_merges: bool) -> MetadataColumns {
        MetadataColumns {
            author: false,
            hash: false,
            date: false,
            mute_merges: false,
            mute_base_merges: false,
            collapse_merges,
            avatars: false,
            pr_subjects,
        }
    }

    /// A merge node whose full message carries a body (subject + blank line +
    /// title), the real shape of a GitHub merge-commit message.
    fn merge_node_with_body(oid_byte: u8, subject: &str, title: &str, parents: [u8; 2]) -> GraphNode {
        let mut n = merge_node_full(oid_byte, subject, parents);
        n.commit.as_mut().unwrap().full_message = format!("{subject}\n\n{title}\n");
        n
    }

    #[test]
    fn pr_merge_commit_rewritten_to_icon_and_title_when_toggle_on() {
        let node = merge_node_with_body(
            40,
            "Merge pull request #123 from owner/feat",
            "Add the frobnicator",
            [1, 2],
        );
        let line = render_row_with(&node, &HashMap::new(), cols_pr_subjects(true, false), &HashSet::new());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains(&format!("{PR_BADGE_ICON} Add the frobnicator")),
            "rewritten subject present: {text:?}"
        );
        // #101: the number stays hidden — it's sometimes an issue reference,
        // and the title alone reads cleaner.
        assert!(!text.contains("#123"), "PR number hidden: {text:?}");
        assert!(
            !text.contains("Merge pull request"),
            "raw merge subject text absent: {text:?}"
        );
    }

    #[test]
    fn pr_merge_commit_keeps_raw_subject_when_toggle_off() {
        let node = merge_node_with_body(
            41,
            "Merge pull request #123 from owner/feat",
            "Add the frobnicator",
            [1, 2],
        );
        let line = render_row_with(&node, &HashMap::new(), cols_pr_subjects(false, false), &HashSet::new());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("Merge pull request #123 from owner/feat"),
            "raw subject kept when toggle is off: {text:?}"
        );
        assert!(!text.contains(&PR_BADGE_ICON.to_string()), "no rewrite: {text:?}");
    }

    #[test]
    fn squash_commit_subject_rewritten_to_icon_and_title() {
        let node = commit_node(50, "Add the frobnicator (#456)", &[]);
        let line = render_row_with(&node, &HashMap::new(), cols_pr_subjects(true, false), &HashSet::new());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains(&format!("{PR_BADGE_ICON} Add the frobnicator")),
            "rewritten squash subject: {text:?}"
        );
        assert!(!text.contains("#456"), "number (possibly an issue ref) hidden: {text:?}");
    }

    #[test]
    fn squash_commit_keeps_raw_subject_when_toggle_off() {
        let node = commit_node(51, "Add the frobnicator (#456)", &[]);
        let line = render_row_with(&node, &HashMap::new(), cols_pr_subjects(false, false), &HashSet::new());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("Add the frobnicator (#456)"),
            "raw subject kept when toggle is off: {text:?}"
        );
    }

    #[test]
    fn collapse_merges_wins_over_pr_subject_rewrite_on_the_same_row() {
        let node = merge_node_with_body(
            42,
            "Merge pull request #123 from owner/feat",
            "Add the frobnicator",
            [1, 2],
        );
        let line = render_row_with(&node, &HashMap::new(), cols_pr_subjects(true, true), &HashSet::new());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains(MERGE_ICON), "collapse glyph wins: {text:?}");
        assert!(!text.contains("#123"), "pr-subject rewrite suppressed: {text:?}");
        assert!(!text.contains("Merge pull request"), "raw subject also suppressed: {text:?}");
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
        render_cells_unicode(&mut spans, &node, &theme, 0, 8, Some(&lit), false, None);

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
        render_cells_unicode(&mut spans, &node, &theme, 0, 8, None, false, None);
        assert!(spans
            .iter()
            .all(|s| !s.style.add_modifier.contains(Modifier::DIM)));
    }

    #[test]
    fn merged_lane_dims_only_merged_cells_in_unicode() {
        // A cell whose edge touches a merged-lane commit dims; a trunk cell
        // (no merged endpoint) stays bright — the same per-cell rule as tracing,
        // driven off the merged-lane set instead of the trace lit set (#108).
        let theme = Theme::dark();
        let (m, t) = (oid(4), oid(3));
        let mut node = node_with_cells(vec![CellType::Commit(0), CellType::Pipe(1)], false);
        // Dot self-edge belongs to the merged commit; the pipe is a trunk pipe.
        node.cell_oids = vec![(Some((m, m)), None), (Some((t, t)), None)];
        let merged: HashSet<git2::Oid> = [m].into_iter().collect();

        let mut spans: Vec<Span> = Vec::new();
        render_cells_unicode(&mut spans, &node, &theme, 0, 8, None, false, Some(&merged));

        let dot = spans.iter().find(|s| s.content.contains('●')).unwrap();
        let pipe = spans.iter().find(|s| s.content.contains('│')).unwrap();
        assert!(
            dot.style.add_modifier.contains(Modifier::DIM),
            "merged-lane commit dot dims"
        );
        assert!(
            !pipe.style.add_modifier.contains(Modifier::DIM),
            "a trunk pipe with no merged endpoint stays bright"
        );
    }

    #[test]
    fn merged_lane_dim_composes_with_trace_in_unicode() {
        // Merged-lane dim ORs with trace: a merged-lane cell dims even when the
        // trace would light it, so a selected merged branch still recedes.
        let theme = Theme::dark();
        let m = oid(4);
        let mut node = node_with_cells(vec![CellType::Commit(0)], false);
        node.cell_oids = vec![(Some((m, m)), None)];
        let lit: std::collections::HashMap<crate::git::graph::CellEdge, git2::Oid> =
            [((m, m), m)].into_iter().collect();
        let merged: HashSet<git2::Oid> = [m].into_iter().collect();

        let mut spans: Vec<Span> = Vec::new();
        render_cells_unicode(&mut spans, &node, &theme, 0, 8, Some(&lit), false, Some(&merged));

        let dot = spans.iter().find(|s| s.content.contains('●')).unwrap();
        assert!(
            dot.style.add_modifier.contains(Modifier::DIM),
            "merged-lane dim wins even when tracing lights the cell"
        );
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
            curved_above: false,
            curved_below: false,
            spoke_on_dot: false,
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
            curved_above: false,
            curved_below: false,
            spoke_on_dot: false,
        };
        let base: Vec<_> = (0..n)
            .map(|_| crate::ui::graph_pixels::RowSpec {
                cells: vec![cell()],
                underlay: Vec::new(),
                incoming: Vec::new(),
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
        let out = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, None, 3, 7);

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
        let full = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, None, 0, base.len());
        let windowed = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, None, 4, 9);
        for i in 4..9 {
            assert_eq!(windowed[i], full[i], "row {i} matches the full-range dim");
        }
    }

    #[test]
    fn empty_window_builds_nothing() {
        let (base, oids, lit, lane_rgb) = window_dim_fixture(5);
        let ro = row_oids_of(&oids);
        let ff = no_force(base.len());
        let out = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, None, 2, 2);
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
        let out = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, None, 0, base.len());

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
    fn merged_lane_dims_touching_pixel_cells_only() {
        // With tracing off (empty lit) and a merged-lane set supplied, only the
        // cells whose edge touches a merged commit dim — the rest stay bright.
        // The fixture's even rows carry edge (oid(2),oid(2)); odd rows (oid(9)…).
        let (base, oids, _lit, _rgb) = window_dim_fixture(4);
        let ro = row_oids_of(&oids);
        let ff = no_force(base.len());
        let empty_lit = std::collections::HashMap::new();
        let empty_rgb = std::collections::HashMap::new();
        let merged: HashSet<git2::Oid> = [oid(2)].into_iter().collect();

        let out = dim_specs_window_core(
            &base,
            &ro,
            &ff,
            &empty_lit,
            &empty_rgb,
            Some(&merged),
            0,
            base.len(),
        );

        for (i, spec) in out.iter().enumerate().take(4) {
            // Even rows touch oid(2) -> dim; odd rows have no merged endpoint and
            // must stay bright — proving the empty-lit trace pass is skipped
            // (otherwise it would dim every cell).
            assert_eq!(
                spec.cells[0].dim,
                i % 2 == 0,
                "row {i} merged-lane dim: {:?}",
                spec.cells
            );
        }
    }

    #[test]
    fn trace_off_leaves_pixel_specs_bright() {
        // Regression guard for the empty-lit fix: with no trace, no force-dim,
        // and no merged-lane set, dimming is a no-op — every base cell stays
        // bright (the trace pass must not dim everything when nothing is lit).
        let (base, oids, _lit, _rgb) = window_dim_fixture(4);
        let ro = row_oids_of(&oids);
        let ff = no_force(base.len());
        let empty_lit = std::collections::HashMap::new();
        let empty_rgb = std::collections::HashMap::new();

        let out = dim_specs_window_core(
            &base, &ro, &ff, &empty_lit, &empty_rgb, None, 0, base.len(),
        );

        assert!(
            out.iter().all(|s| s.cells.iter().all(|c| !c.dim && !c.dim_secondary)),
            "no dim source active: all cells stay bright"
        );
    }

    #[test]
    fn incoming_tail_restyles_from_the_dimmed_neighbour_spoke() {
        // A branch curve crosses the row 0 → row 1 boundary: row 1 carries its
        // tail (`RowSpec::incoming`). The tail must restyle exactly like the
        // neighbour's own half — dim when the spoke cell dims, recolored when
        // the spoke cell is lit — or the cubic changes style mid-stroke at the
        // row seam.
        use crate::ui::graph_pixels::{build_row_spec, NeighborRow};
        let theme = crate::ui::theme::Theme::dark();
        let cells_a = vec![
            CellType::Commit(0),
            CellType::Horizontal(1),
            CellType::BranchLeft(1),
        ];
        let cells_b = vec![CellType::Empty, CellType::Empty, CellType::Commit(1)];
        let node_a = commit_row(cells_a.clone());
        let node_b = commit_row(cells_b.clone());
        let base = vec![
            build_row_spec(
                None,
                &node_a,
                Some(&cells_b),
                &[],
                None,
                Some(NeighborRow { underlay: &[], cells: &cells_b }),
                &theme,
            ),
            build_row_spec(
                Some(&cells_a),
                &node_b,
                None,
                &[],
                Some(NeighborRow { underlay: &[], cells: &cells_a }),
                None,
                &theme,
            ),
        ];
        assert_eq!(base[1].incoming.len(), 1, "row 1 carries the branch tail");
        assert!(!base[1].incoming[0].dim, "base tail starts undimmed");

        let branch_edge = (oid(1), oid(2));
        let trunk_edge = (oid(1), oid(3));
        let oids_rows = vec![
            vec![
                (Some(trunk_edge), None),
                (Some(branch_edge), None),
                (Some(branch_edge), None), // the spoke cell (col 2)
            ],
            vec![(None, None), (None, None), (Some(branch_edge), None)],
        ];
        let ro = row_oids_of(&oids_rows);
        let ff = no_force(base.len());

        // Tracing lights only the trunk: the spoke cell dims → so must the tail.
        let lit_trunk = [(trunk_edge, oid(3))].into_iter().collect();
        let out = dim_specs_window_core(&base, &ro, &ff, &lit_trunk, &HashMap::new(), None, 0, 2);
        assert!(out[0].cells[2].dim, "spoke cell dims off-lineage");
        assert!(out[1].incoming[0].dim, "tail dims with its source spoke");

        // Tracing lights the branch: the spoke recolors → the tail follows.
        let red = [255u8, 0, 0];
        let lit_branch = [(branch_edge, oid(2))].into_iter().collect();
        let lane_rgb = [(oid(2), red)].into_iter().collect();
        let out = dim_specs_window_core(&base, &ro, &ff, &lit_branch, &lane_rgb, None, 0, 2);
        assert!(!out[1].incoming[0].dim, "lit tail stays bright");
        assert_eq!(out[1].incoming[0].color, red, "tail takes the spoke's traced color");
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
        render_cells_unicode(&mut spans, &node, &theme, 0, 3, Some(&lit), false, None);

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
