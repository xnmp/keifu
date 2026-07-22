//! Pixel-spec build + per-frame dim windowing.
//!
//! Builds the cached, per-row `graph_pixels::RowSpec` base geometry
//! (`build_pixel_base_specs`) and layers the two per-frame dim sources
//! (branch-trace dim + base-update force-dim, plus merged-lane dim) onto just
//! the on-screen window (`dim_pixel_specs_window` → `dim_specs_window_core`).
//! The dim rule here is deliberately a parallel implementation of
//! `render_cells_unicode`'s dim rule (see the comment in `mod.rs`); the two
//! dim domains are kept separate on purpose.

use std::collections::HashSet;

use crate::app::App;

use super::geometry::pixel_row_cells;
use super::rows::{adjacent_cells, fold_rows_windowed, visible_rows};
use crate::ui::theme::Theme;
use super::{apply_merged_lane_dim, is_base_update_row};

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
    // The selected commit is exempt (mirrors the unicode path's
    // `RowRenderCtx::merged_exempt`): its dot and its own strokes stay live.
    let merged_exempt = app
        .selected_commit_node()
        .and_then(|n| n.commit.as_ref())
        .map(|c| c.oid);
    dim_specs_window_core(
        base, &row_oids, &force_dim, lit, lane_rgb, merged_oids, merged_exempt, win_start, win_end,
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
    merged_exempt: Option<git2::Oid>,
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
            apply_merged_lane_dim(&mut spec.cells, o.cells, m, merged_exempt);
            apply_merged_lane_dim(&mut spec.underlay, o.underlay, m, merged_exempt);
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
                        // A TeeDown's boundary-crossing curve is its STEM,
                        // carried in the secondary stroke (#115).
                        c.color = match cell.shape {
                            crate::ui::graph_pixels::CellShape::TeeDown => cell.secondary,
                            _ => cell.color,
                        };
                        // A Tee source cell's boundary-crossing curve is its
                        // ARM (the trunk is a straight through-line that never
                        // crosses as a curve), so the tail follows the arm's
                        // flag (#113); a TeeDown's stem likewise styles via
                        // `dim_secondary`.
                        c.dim = match cell.shape {
                            crate::ui::graph_pixels::CellShape::TeeRight
                            | crate::ui::graph_pixels::CellShape::TeeLeft
                            | crate::ui::graph_pixels::CellShape::TeeDown => cell.dim_secondary,
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::rows::fold_rows;
    use crate::git::graph::{CellType, GraphNode};
    use chrono::Local;
    use std::collections::HashMap;

    // ── fixtures ─────────────────────────────────────────────────────

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

    fn oid(b: u8) -> git2::Oid {
        git2::Oid::from_bytes(&[b; 20]).unwrap()
    }

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
        let out = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, None, None, 3, 7);

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
        let full = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, None, None, 0, base.len());
        let windowed = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, None, None, 4, 9);
        for i in 4..9 {
            assert_eq!(windowed[i], full[i], "row {i} matches the full-range dim");
        }
    }

    #[test]
    fn empty_window_builds_nothing() {
        let (base, oids, lit, lane_rgb) = window_dim_fixture(5);
        let ro = row_oids_of(&oids);
        let ff = no_force(base.len());
        let out = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, None, None, 2, 2);
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
        let out = dim_specs_window_core(&base, &ro, &ff, &lit, &lane_rgb, None, None, 0, base.len());

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
            None,
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
            &base, &ro, &ff, &empty_lit, &empty_rgb, None, None, 0, base.len(),
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
        let out = dim_specs_window_core(&base, &ro, &ff, &lit_trunk, &HashMap::new(), None, None, 0, 2);
        assert!(out[0].cells[2].dim, "spoke cell dims off-lineage");
        assert!(out[1].incoming[0].dim, "tail dims with its source spoke");

        // Tracing lights the branch: the spoke recolors → the tail follows.
        let red = [255u8, 0, 0];
        let lit_branch = [(branch_edge, oid(2))].into_iter().collect();
        let lane_rgb = [(oid(2), red)].into_iter().collect();
        let out = dim_specs_window_core(&base, &ro, &ff, &lit_branch, &lane_rgb, None, None, 0, 2);
        assert!(!out[1].incoming[0].dim, "lit tail stays bright");
        assert_eq!(out[1].incoming[0].color, red, "tail takes the spoke's traced color");
    }
}
