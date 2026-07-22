//! Row folding and viewport windowing: turns the filtered graph nodes into the
//! `RenderRow`s the list draws, folding connector rows into the following commit
//! in pixel mode, and computes which rows are worth building for a given scroll
//! position.

use crate::app::App;
use crate::git::graph::{CellType, GraphNode};

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
pub(super) fn fold_rows(base: Vec<(usize, &GraphNode)>, fold_connectors: bool) -> Vec<RenderRow<'_>> {
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
pub(super) fn fold_rows_windowed<'a>(
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
pub(super) fn adjacent_cells(rows: &[RenderRow], i: usize, above: bool) -> Option<Vec<CellType>> {
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
pub(super) fn visible_row_window(
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

#[cfg(test)]
mod tests {
    use super::*;

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

    // ── connector folding (pixel mode) ───────────────────────────────

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

    fn connector_node(cells: Vec<CellType>) -> GraphNode {
        // commit: None + not uncommitted => a connector row.
        node_with_cells(cells, false)
    }

    fn commit_row(cells: Vec<CellType>) -> GraphNode {
        use crate::git::CommitInfo;
        let commit = CommitInfo {
            oid: git2::Oid::zero(),
            short_id: "abc1234".to_string(),
            author_name: "a".to_string(),
            author_email: "a@b".to_string(),
            timestamp: chrono::Local::now(),
            message: "m".to_string(),
            full_message: "m".to_string(),
            parent_oids: vec![git2::Oid::zero(); 2], // 2 parents => a merge
        };
        let mut n = node_with_cells(cells, false);
        n.commit = Some(commit);
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
}
