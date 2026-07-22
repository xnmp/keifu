//! Graph view widget

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, StatefulWidget},
};
use chrono::Local;

use std::collections::HashSet;

use crate::{
    app::App,
    git::graph::{CellType, GraphNode},
    mouse::ChipHit,
    pr::PrContext,
};

mod badges;
mod chips;
mod geometry;
mod metrics;
mod pixel_dim;
mod row;
mod rows;

pub use badges::pr_for_branch_labels;

pub use chips::REMOTE_ONLY_ICON;

pub use geometry::{
    avatar_overlay_x, avatars_active, effective_graph_width, next_graph_cap, AVATAR_GAP_CELLS,
    AVATAR_IMAGE_CELLS, AVATAR_RESERVED_CELLS, GRAPH_LEADING_COLUMNS,
};
use geometry::{ellipsis_style, graph_truncation};

pub(crate) use metrics::display_width;

pub use pixel_dim::{build_pixel_base_specs, dim_pixel_specs_window};

// `is_base_update_row` lives next to `RowModel` in `row`; imported here so the
// pixel dim pass (`super::is_base_update_row`, a descendant path) and this
// module share one predicate.
use row::{is_base_update_row, render_graph_line_tail, RowFlags, RowRenderCtx};

pub use rows::{visible_nodes, visible_rows, RenderRow};
use rows::visible_row_window;

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

/// Set `dim` on every pixel cell whose edge touches a merged-lane commit (issue
/// #108) — exactly the strokes hide-merged would remove. Mirrors
/// `apply_trace_dim`'s per-edge mapping (a `HorizontalPipe` fades each stroke
/// from its own edge, a `Tee*` its arm from the primary and its trunk from the
/// lane's own secondary edge (#113), a commit dot from either edge, every other
/// shape from its primary edge) but ONLY dims: it never clears a `dim` flag and
/// never recolors, so it composes on top of whatever trace/force-dim already
/// decided.
pub(super) fn apply_merged_lane_dim(
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
    use crate::config::MetadataColumns;
    use crate::pr::{CiStatus, MergeState, PrInfo, ReviewState};
    use std::collections::HashMap;

    use super::badges::merged_badge;
    use super::chips::BranchChip;
    use super::row::{resolve_row_model, RowMessage, RowModel};

    // The pure per-row decision model, as the real render path resolves it (via
    // `render_graph_line`'s prelude) but returned directly so decision tests can
    // assert on `RowModel` fields instead of scanning rendered spans. Mirrors the
    // is-base-update / is-merged-lane derivation `render_graph_line` performs.
    fn model_with_ctx(node: &GraphNode, ctx: &RowRenderCtx) -> RowModel {
        let commit = node.commit.as_ref().expect("commit node");
        let is_base_update = is_base_update_row(
            node,
            ctx.metadata_columns.mute_base_merges,
            ctx.base_update_merges,
        );
        let is_merged_lane = node
            .commit
            .as_ref()
            .is_some_and(|c| ctx.merged_lane_oids.is_some_and(|s| s.contains(&c.oid)));
        resolve_row_model(
            node,
            commit,
            ctx,
            RowFlags {
                is_selected: false,
                is_marked: false,
            },
            is_base_update,
            is_merged_lane,
        )
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

    #[test]
    fn pr_merge_message_is_greyed_data_driven() {
        // Decision test: the PR-merge greying lands on the model's `msg_style`.
        let theme = Theme::dark();
        // PR #9's head is commit 5; a merge whose 2nd parent is 5 is a PR merge.
        let open = open_map(vec![("feat", pr_head(9, 5))]);
        let node = merge_node_full(20, "Merge feat", [1, 5]);
        // mute_merges OFF, so only the PR-merge rule can grey the message.
        let model = model_row(&node, &open, false);
        assert_eq!(
            model.msg_style.fg,
            Some(theme.text_muted),
            "PR merge message is greyed"
        );
        // A plain local merge (2nd parent not a PR head, non-GitHub message) is
        // left at full strength when muting is off.
        let plain = merge_node_full(21, "Merge branch 'x'", [1, 2]);
        let plain_model = model_row(&plain, &open, false);
        assert_ne!(
            plain_model.msg_style.fg,
            Some(theme.text_muted),
            "a plain local merge is not greyed"
        );
    }

    #[test]
    fn pr_merge_message_is_greyed_by_github_format_when_pr_closed() {
        // Decision test on `msg_style`.
        let theme = Theme::dark();
        // No open PRs (a merged PR has left `gh pr list`); the message format
        // alone identifies it as a PR merge.
        let open = HashMap::new();
        let node = merge_node_full(20, "Merge pull request #42 from o/b", [1, 2]);
        let model = model_row(&node, &open, false);
        assert_eq!(model.msg_style.fg, Some(theme.text_muted));
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

    /// The pure [`RowModel`] the tail resolves for a row, mirroring
    /// [`render_row`]'s columns/flags. For decision tests that assert on the
    /// model directly rather than scanning rendered spans.
    fn model_row(node: &GraphNode, open_prs: &HashMap<String, PrInfo>, mute_merges: bool) -> RowModel {
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
        model_row_with(node, open_prs, cols, &HashSet::new())
    }

    /// The pure [`RowModel`] the tail resolves, mirroring [`render_row_with`]'s
    /// explicit columns + base-update set. For decision tests.
    fn model_row_with(
        node: &GraphNode,
        open_prs: &HashMap<String, PrInfo>,
        cols: MetadataColumns,
        base_update_merges: &HashSet<git2::Oid>,
    ) -> RowModel {
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
        model_with_ctx(node, &ctx)
    }

    /// The pure [`RowModel`] with an explicit merged-branch classification set
    /// and the dim-merged-branches setting (issue #106) — the tail's decision
    /// output, so tests assert on chip styles / the merged badge directly.
    fn model_row_with_merged(
        node: &GraphNode,
        merged_branches: &HashSet<String>,
        merged_dim: bool,
    ) -> RowModel {
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
        model_with_ctx(node, &ctx)
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

    /// The branch chip in the resolved model whose label contains `name`.
    /// Panics if absent.
    fn model_chip<'a>(model: &'a RowModel, name: &str) -> &'a BranchChip {
        model
            .branch_chips
            .iter()
            .find(|c| c.label.contains(name))
            .unwrap_or_else(|| panic!("branch chip {name:?} not found in {:?}", model.branch_chips))
    }

    #[test]
    fn merged_branch_row_dims_when_dim_setting_on() {
        let node = branch_tip_node("ancestry commit", &["feature/ancestry-landed"]);
        let merged: HashSet<String> = ["feature/ancestry-landed".to_string()].into_iter().collect();

        let model = model_row_with_merged(&node, &merged, true);
        let chip = model_chip(&model, "feature/ancestry-landed");
        assert!(
            chip.style.add_modifier.contains(Modifier::DIM),
            "merged chip dims when the setting is on: {chip:?}"
        );
        assert_eq!(
            model.merged_badge,
            Some(merged_badge()),
            "merged badge shown when the setting is on"
        );
    }

    #[test]
    fn merged_branch_row_renders_normally_when_dim_setting_off() {
        let node = branch_tip_node("ancestry commit", &["feature/ancestry-landed"]);
        let merged: HashSet<String> = ["feature/ancestry-landed".to_string()].into_iter().collect();

        let model = model_row_with_merged(&node, &merged, false);
        let chip = model_chip(&model, "feature/ancestry-landed");
        assert!(
            !chip.style.add_modifier.contains(Modifier::DIM),
            "dim setting off renders the branch chip normally: {chip:?}"
        );
        assert_eq!(
            model.merged_badge, None,
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

        let model = model_row_with_merged(&node, &merged, true);
        let chip = model_chip(&model, "feature/squash-me");
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

        let model = model_row_with_merged(&node, &merged, false);
        let chip = model_chip(&model, "feature/squash-me");
        assert!(
            !chip.style.add_modifier.contains(Modifier::DIM),
            "squash-merged chip renders normally when the setting is off: {chip:?}"
        );
        assert_eq!(
            model.merged_badge, None,
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
            let model = model_row_with_merged(&node, &empty, dim);
            let chip = model_chip(&model, "feature/still-open");
            assert!(
                !chip.style.add_modifier.contains(Modifier::DIM),
                "unmerged branch never dims (dim setting = {dim}): {chip:?}"
            );
            assert_eq!(
                model.merged_badge, None,
                "unmerged branch never shows the merged badge (dim setting = {dim})"
            );
        }
    }

    // ── merged-lane dimming: whole rows grey (#108) ──────────────────────

    /// The pure [`RowModel`] with a set of merged-LANE commit OIDs (#108) and
    /// every metadata column on, so tests can assert the message and the
    /// hash/author/date column styles. `lane_oids = None` resolves the feature
    /// off (the settings toggle held off / hide on).
    fn model_row_with_merged_lane(
        node: &GraphNode,
        lane_oids: Option<&HashSet<git2::Oid>>,
    ) -> RowModel {
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
        model_with_ctx(node, &ctx)
    }

    #[test]
    fn merged_lane_row_greys_message_and_metadata() {
        // A commit exclusive to a merged branch's lane reads grey across the
        // whole row — message DIM+greyed, and the metadata columns greyed (#92).
        let theme = Theme::dark();
        let node = commit_node(7, "landed feature work", &[]);
        let lane: HashSet<git2::Oid> = [oid(7)].into_iter().collect();

        let model = model_row_with_merged_lane(&node, Some(&lane));

        assert_eq!(
            model.msg_style.fg,
            Some(theme.text_muted),
            "merged-lane message greyed: {:?}",
            model.msg_style
        );
        assert!(
            model.msg_style.add_modifier.contains(Modifier::DIM),
            "merged-lane message DIM: {:?}",
            model.msg_style
        );
        // author_color != text_muted in dark, so the greying is observable.
        assert_eq!(
            model.author_style.fg,
            Some(theme.text_muted),
            "merged-lane author column greyed: {:?}",
            model.author_style
        );
    }

    #[test]
    fn merged_lane_row_renders_normally_when_feature_off() {
        // `None` = dim setting off (or hide on): the same row renders exactly
        // like an ordinary commit — no grey, no DIM.
        let theme = Theme::dark();
        let node = commit_node(7, "landed feature work", &[]);

        let model = model_row_with_merged_lane(&node, None);

        assert_eq!(model.msg_style.fg, None, "feature off: message not greyed: {:?}", model.msg_style);
        assert!(
            !model.msg_style.add_modifier.contains(Modifier::DIM),
            "feature off: message not DIM: {:?}",
            model.msg_style
        );
        assert_eq!(
            model.author_style.fg,
            Some(theme.author_color),
            "feature off: author column keeps its own color: {:?}",
            model.author_style
        );
    }

    #[test]
    fn commit_outside_the_merged_lane_stays_bright_when_feature_on() {
        // Only commits IN the lane set dim — a trunk commit rendered while the
        // feature is on keeps full-strength styling.
        let theme = Theme::dark();
        let trunk = commit_node(3, "trunk work", &[]);
        let lane: HashSet<git2::Oid> = [oid(7)].into_iter().collect(); // trunk is oid(3)

        let model = model_row_with_merged_lane(&trunk, Some(&lane));

        assert_eq!(model.msg_style.fg, None, "non-lane commit not greyed: {:?}", model.msg_style);
        assert_eq!(
            model.author_style.fg,
            Some(theme.author_color),
            "non-lane author keeps its own color: {:?}",
            model.author_style
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

    #[test]
    fn muted_merge_dims_message_text_not_the_graph() {
        // Decision test: the muted-merge message style lands on `msg_style`.
        let theme = Theme::dark();
        let node = merge_node("merge-branch-into-main");
        // Toggle ON: the merge's message text is dimmed AND explicitly greyed.
        // DIM alone isn't reliable — terminal DIM support is inconsistent, so
        // muted merges must pair it with an explicit `text_muted` fg (#92).
        let muted = model_row(&node, &HashMap::new(), true).msg_style;
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
        let normal = model_row(&node, &HashMap::new(), false).msg_style;
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
        // `text_muted` foreground. Decided on the model's per-column styles.
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

        let merge = merge_node("merge-branch-into-main");
        let model = model_row_with(&merge, &HashMap::new(), cols, &HashSet::new());
        assert_eq!(
            model.author_style.fg,
            Some(theme.text_muted),
            "muted merge author column should be greyed: {:?}",
            model.author_style
        );
        assert_eq!(
            model.hash_style.fg,
            Some(theme.text_muted),
            "muted merge hash column should be greyed: {:?}",
            model.hash_style
        );

        // A normal (non-merge) row keeps its ordinary per-column metadata
        // colors (not the shared muted grey). Note: in the dark theme
        // `hash_color`/`date_color` happen to equal `text_muted`'s value, so
        // "not muted" is asserted via the actual expected color rather than a
        // simple inequality against `text_muted`.
        let normal = commit_node(1, "normal commit", &[]);
        let normal_model = model_row_with(&normal, &HashMap::new(), cols, &HashSet::new());
        assert_eq!(
            normal_model.author_style.fg,
            Some(theme.author_color),
            "normal row author column should use author_color, not be greyed: {:?}",
            normal_model.author_style
        );
        assert_eq!(
            normal_model.hash_style.fg,
            Some(theme.hash_color),
            "normal row hash column should use hash_color: {:?}",
            normal_model.hash_style
        );
    }

    #[test]
    fn head_merge_is_never_muted() {
        let mut node = merge_node("head-merge");
        node.is_head = true;
        // Even with muting on, the HEAD commit's message stays legible.
        let style = model_row(&node, &HashMap::new(), true).msg_style;
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
        // Decision test: the strong base-update mute lands on `msg_style`.
        let theme = Theme::dark();
        let node = merge_node_full(30, "Merge main into feature", [1, 2]);
        let mut set = HashSet::new();
        set.insert(oid(30));
        // Option ON + this commit is in the set → strong mute (muted fg + DIM).
        let style = model_row_with(&node, &HashMap::new(), merge_cols(false, true, false), &set).msg_style;
        assert_eq!(style.fg, Some(theme.text_muted), "strong-muted fg");
        assert!(style.add_modifier.contains(Modifier::DIM), "strong-muted DIM");
    }

    #[test]
    fn base_update_merge_is_not_muted_when_option_off_or_not_in_set() {
        // Decision test on `msg_style`.
        let node = merge_node_full(30, "Merge main into feature", [1, 2]);
        let mut set = HashSet::new();
        set.insert(oid(30));
        // Option OFF → not muted even though the commit is in the set.
        let s_off =
            model_row_with(&node, &HashMap::new(), merge_cols(false, false, false), &set).msg_style;
        assert_eq!(s_off.fg, None);
        assert!(!s_off.add_modifier.contains(Modifier::DIM));
        // Option ON but commit NOT in the set → not muted.
        let s_empty =
            model_row_with(&node, &HashMap::new(), merge_cols(false, true, false), &HashSet::new())
                .msg_style;
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
        // Decision test: a collapsed merge selects `RowMessage::Collapse` (the
        // bare glyph, not the message text) and mutes the message style.
        let node = merge_node_full(31, "Merge branch 'topic'", [1, 2]);
        let model = model_row_with(&node, &HashMap::new(), merge_cols(false, false, true), &HashSet::new());
        assert_eq!(model.message, RowMessage::Collapse, "collapse selects the merge glyph");
        // Collapse implies muting: the message style is dimmed.
        assert!(model.msg_style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn collapse_leaves_non_merge_commits_alone() {
        // A non-merge commit keeps its raw message even with collapse on.
        let mut node = merge_node("real message");
        node.commit.as_mut().unwrap().parent_oids = vec![git2::Oid::zero()]; // 1 parent
        let model = model_row_with(&node, &HashMap::new(), merge_cols(false, false, true), &HashSet::new());
        assert_eq!(
            model.message,
            RowMessage::Raw {
                text: "real message".to_string()
            },
            "non-merge keeps its message"
        );
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
        // Decision test: a PR-merge subject selects `RowMessage::PrSubject` with
        // the title only (#101: the number — sometimes an issue ref — is dropped).
        let node = merge_node_with_body(
            40,
            "Merge pull request #123 from owner/feat",
            "Add the frobnicator",
            [1, 2],
        );
        let model = model_row_with(&node, &HashMap::new(), cols_pr_subjects(true, false), &HashSet::new());
        assert_eq!(
            model.message,
            RowMessage::PrSubject {
                title: "Add the frobnicator".to_string()
            },
            "rewritten to the title only, no number, no raw merge subject"
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
        let model = model_row_with(&node, &HashMap::new(), cols_pr_subjects(false, false), &HashSet::new());
        assert_eq!(
            model.message,
            RowMessage::Raw {
                text: "Merge pull request #123 from owner/feat".to_string()
            },
            "raw subject kept when the toggle is off (no rewrite)"
        );
    }

    #[test]
    fn squash_commit_subject_rewritten_to_icon_and_title() {
        let node = commit_node(50, "Add the frobnicator (#456)", &[]);
        let model = model_row_with(&node, &HashMap::new(), cols_pr_subjects(true, false), &HashSet::new());
        assert_eq!(
            model.message,
            RowMessage::PrSubject {
                title: "Add the frobnicator".to_string()
            },
            "rewritten squash subject to the title only, number (#456) hidden"
        );
    }

    #[test]
    fn squash_commit_keeps_raw_subject_when_toggle_off() {
        let node = commit_node(51, "Add the frobnicator (#456)", &[]);
        let model = model_row_with(&node, &HashMap::new(), cols_pr_subjects(false, false), &HashSet::new());
        assert_eq!(
            model.message,
            RowMessage::Raw {
                text: "Add the frobnicator (#456)".to_string()
            },
            "raw subject kept when the toggle is off"
        );
    }

    #[test]
    fn collapse_merges_wins_over_pr_subject_rewrite_on_the_same_row() {
        // Precedence: Collapse beats a PR-subject rewrite when both qualify.
        let node = merge_node_with_body(
            42,
            "Merge pull request #123 from owner/feat",
            "Add the frobnicator",
            [1, 2],
        );
        let model = model_row_with(&node, &HashMap::new(), cols_pr_subjects(true, true), &HashSet::new());
        assert_eq!(
            model.message,
            RowMessage::Collapse,
            "collapse wins over the pr-subject rewrite"
        );
    }

    #[test]
    fn row_message_precedence_collapse_beats_pr_subject_beats_raw() {
        // One PR-merge node under three settings exercises the full message
        // precedence on the same row: Collapse > PrSubject > Raw.
        let node = merge_node_with_body(
            43,
            "Merge pull request #123 from owner/feat",
            "Add the frobnicator",
            [1, 2],
        );
        // Collapse ON (+ pr-subjects ON): Collapse wins outright.
        let collapse =
            model_row_with(&node, &HashMap::new(), cols_pr_subjects(true, true), &HashSet::new());
        assert_eq!(collapse.message, RowMessage::Collapse);
        // Collapse OFF, pr-subjects ON: rewrites to the title only.
        let subject =
            model_row_with(&node, &HashMap::new(), cols_pr_subjects(true, false), &HashSet::new());
        assert_eq!(
            subject.message,
            RowMessage::PrSubject {
                title: "Add the frobnicator".to_string()
            }
        );
        // Both OFF: the raw subject.
        let raw =
            model_row_with(&node, &HashMap::new(), cols_pr_subjects(false, false), &HashSet::new());
        assert_eq!(
            raw.message,
            RowMessage::Raw {
                text: "Merge pull request #123 from owner/feat".to_string()
            }
        );
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

    // ── mute matrix: every muted category greys the whole row (#92) ──────

    #[test]
    fn mute_matrix_greys_all_metadata_uniformly() {
        // Each mute category must set `row_is_muted` and grey the hash, author,
        // and date column styles to `text_muted` — one uniform treatment.
        let theme = Theme::dark();

        let base_update = {
            let node = merge_node_full(60, "Merge main into feature", [1, 2]);
            let set: HashSet<git2::Oid> = [oid(60)].into_iter().collect();
            model_row_with(&node, &HashMap::new(), merge_cols(false, true, false), &set)
        };
        let pr_merge = {
            let node = merge_node_full(61, "Merge pull request #42 from o/b", [1, 2]);
            model_row(&node, &HashMap::new(), false)
        };
        let muted_merge = {
            let node = merge_node("merge branch into main");
            model_row(&node, &HashMap::new(), true)
        };
        let collapse_merge = {
            let node = merge_node_full(62, "Merge branch 'topic'", [1, 2]);
            model_row_with(&node, &HashMap::new(), merge_cols(false, false, true), &HashSet::new())
        };
        let merged_lane = {
            let node = commit_node(63, "landed feature work", &[]);
            let set: HashSet<git2::Oid> = [oid(63)].into_iter().collect();
            model_row_with_merged_lane(&node, Some(&set))
        };

        for (name, model) in [
            ("base-update", base_update),
            ("pr-merge", pr_merge),
            ("muted-merge", muted_merge),
            ("collapse-merge", collapse_merge),
            ("merged-lane", merged_lane),
        ] {
            assert!(model.row_is_muted, "{name}: row_is_muted");
            assert_eq!(model.hash_style.fg, Some(theme.text_muted), "{name}: hash greyed");
            assert_eq!(model.author_style.fg, Some(theme.text_muted), "{name}: author greyed");
            assert_eq!(model.date_style.fg, Some(theme.text_muted), "{name}: date greyed");
        }
    }

    #[test]
    fn mute_precedence_msg_style() {
        // A row that is BOTH a base-update back-merge AND a PR-merge (its subject
        // is a GitHub merge message). Base-update is the stronger mute, so it
        // wins the message style: muted fg + DIM.
        let theme = Theme::dark();
        let node = merge_node_full(64, "Merge pull request #42 from o/b", [1, 2]);
        let set: HashSet<git2::Oid> = [oid(64)].into_iter().collect();
        let both =
            model_row_with(&node, &HashMap::new(), merge_cols(false, true, false), &set).msg_style;
        assert_eq!(both.fg, Some(theme.text_muted), "base-update greys the fg");
        assert!(both.add_modifier.contains(Modifier::DIM), "base-update adds DIM");

        // The same PR-merge subject WITHOUT the base-update classification: greyed
        // fg but NO DIM (PR-merge is the weaker mute).
        let pr_only = model_row(&node, &HashMap::new(), false).msg_style;
        assert_eq!(pr_only.fg, Some(theme.text_muted), "pr-merge greys the fg");
        assert!(
            !pr_only.add_modifier.contains(Modifier::DIM),
            "pr-merge alone carries no DIM: {pr_only:?}"
        );
    }

    #[test]
    fn head_is_immune_to_every_mute_category() {
        // Four categories exclude HEAD in their own definitions, so a HEAD row
        // that otherwise qualifies is never muted.
        let base_update = {
            let mut node = merge_node_full(70, "Merge main into feature", [1, 2]);
            node.is_head = true;
            let set: HashSet<git2::Oid> = [oid(70)].into_iter().collect();
            model_row_with(&node, &HashMap::new(), merge_cols(false, true, false), &set)
        };
        let pr_merge = {
            let mut node = merge_node_full(71, "Merge pull request #42 from o/b", [1, 2]);
            node.is_head = true;
            model_row(&node, &HashMap::new(), false)
        };
        let muted_merge = {
            let mut node = merge_node("merge into main");
            node.is_head = true;
            model_row(&node, &HashMap::new(), true)
        };
        let collapse_merge = {
            let mut node = merge_node_full(72, "Merge branch 'topic'", [1, 2]);
            node.is_head = true;
            model_row_with(&node, &HashMap::new(), merge_cols(false, false, true), &HashSet::new())
        };
        for (name, model) in [
            ("base-update", base_update),
            ("pr-merge", pr_merge),
            ("muted-merge", muted_merge),
            ("collapse-merge", collapse_merge),
        ] {
            assert!(!model.row_is_muted, "{name}: HEAD is never muted");
        }

        // FINDING (item 8): the merged-lane category does NOT exclude HEAD in its
        // `is_merged_lane` derivation, so a HEAD commit that lands in the
        // merged-lane set IS muted — unlike the four categories above. Pinning
        // actual behavior rather than the "HEAD immune" expectation.
        let mut head_lane = commit_node(73, "landed feature work", &[]);
        head_lane.is_head = true;
        let set: HashSet<git2::Oid> = [oid(73)].into_iter().collect();
        let lane_model = model_row_with_merged_lane(&head_lane, Some(&set));
        assert!(
            lane_model.row_is_muted,
            "merged-lane mutes even a HEAD row (current behavior)"
        );
    }

    /// The resolved model for `node` with the given columns and an explicit
    /// selection flag — the only decision helper that exercises `is_selected`
    /// (the `model_*` family hardwires it false).
    fn model_row_selected(node: &GraphNode, cols: MetadataColumns, is_selected: bool) -> RowModel {
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
            merged_dim: true,
            merged_lane_oids: None,
            base_update_merges: &HashSet::new(),
            metadata_columns: cols,
            graph_width: 4,
            total_width: 200,
            selected_branch_name: None,
            trace: None,
        };
        let commit = node.commit.as_ref().expect("commit node");
        let is_base_update = is_base_update_row(node, cols.mute_base_merges, ctx.base_update_merges);
        resolve_row_model(
            node,
            commit,
            &ctx,
            RowFlags {
                is_selected,
                is_marked: false,
            },
            is_base_update,
            false,
        )
    }

    #[test]
    fn selection_bold_only_when_unmuted() {
        let theme = Theme::dark();

        // An ordinary (unmuted) row: selection adds BOLD to the message style.
        let normal = commit_node(80, "ordinary work", &[]);
        let selected = model_row_selected(&normal, merge_cols(false, false, false), true).msg_style;
        assert!(
            selected.add_modifier.contains(Modifier::BOLD),
            "selected unmuted row is BOLD: {selected:?}"
        );
        let unselected =
            model_row_selected(&normal, merge_cols(false, false, false), false).msg_style;
        assert!(
            !unselected.add_modifier.contains(Modifier::BOLD),
            "unselected unmuted row is not BOLD: {unselected:?}"
        );

        // A muted merge row: the mute wins at the model level — selecting it does
        // NOT add BOLD to `msg_style` (the widget layer promotes BOLD later; the
        // model keeps the mute style).
        let merge = merge_node("merge into main");
        let muted = model_row_selected(&merge, merge_cols(true, false, false), true).msg_style;
        assert!(
            !muted.add_modifier.contains(Modifier::BOLD),
            "selected muted row keeps the mute style, no model-level BOLD: {muted:?}"
        );
        assert_eq!(muted.fg, Some(theme.text_muted), "muted fg retained under selection");
        assert!(
            muted.add_modifier.contains(Modifier::DIM),
            "muted DIM retained under selection: {muted:?}"
        );
    }
}
