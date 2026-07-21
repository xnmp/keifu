//! Pixel-rendered commit graph lines via terminal image protocols.
//!
//! Rasterizes each graph row to a transparent RGBA image and hands it to
//! `ratatui-image` for display, giving continuous VSCode-style lines instead of
//! gappy box-drawing glyphs. Everything below `PixelGraphState` is pure and
//! deterministic so it can be unit-tested without a terminal.

use std::collections::{HashMap, HashSet};

use image::{DynamicImage, RgbaImage};
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::Resize;

use crate::git::graph::{CellType, GraphNode};

use super::theme::Theme;

/// How a commit dot is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommitStyle {
    /// Filled disc.
    Normal,
    /// Ring with a small filled centre.
    Head,
    /// Hollow circle (stroke only).
    Uncommitted,
}

/// Resolved shape for one character cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CellShape {
    Empty,
    Pipe,
    Horizontal,
    BranchRight,
    BranchLeft,
    MergeRight,
    MergeLeft,
    HorizontalPipe,
    TeeRight,
    TeeLeft,
    TeeUp,
    Commit {
        connect_up: bool,
        connect_down: bool,
        style: CommitStyle,
    },
}

/// A fully-resolved cell: shape plus concrete RGB colors. `secondary` is the
/// horizontal stroke color for `HorizontalPipe` and the connector (lane) color
/// for `Commit` cells — a HEAD cell's `color` is the star gold while its
/// pass-through connectors keep the lane color. Elsewhere it equals `color`.
/// `dim` fades the primary stroke to a low alpha when branch
/// tracing is active and its edge is not on the selected commit's lineage;
/// `dim_secondary` does the same for the secondary stroke (`HorizontalPipe`'s
/// horizontal), so a crossing can light one direction while the other fades.
/// For every other shape `dim_secondary` mirrors `dim`. Both are part of the
/// hash/eq, so a dimmed variant is a distinct spec and caches separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelCell {
    pub shape: CellShape,
    pub color: [u8; 3],
    pub secondary: [u8; 3],
    pub dim: bool,
    pub dim_secondary: bool,
    /// The stroke crossing this cell's top edge arrives as a neighbour-row
    /// curve landing at this cell's mid-height (the cell above is a pure
    /// corner, `Branch*`), so no straight line is drawn in the top half — the
    /// incoming tail replaces it. Only set on shapes that draw a vertical
    /// (`Pipe`, `HorizontalPipe`, `Commit`), so other cells' cache keys don't
    /// churn.
    pub curved_above: bool,
    /// Same for the bottom edge (the cell below is a `Merge*`/`TeeUp` corner).
    pub curved_below: bool,
}

/// Alpha applied to non-traced graph cells while branch tracing is active.
pub const TRACE_DIM_ALPHA: f32 = 0.28;

/// A fully-resolved, hashable description of one row's pixel content. Two rows
/// with an identical `RowSpec` rasterize to an identical image, so protocols
/// are cached by it.
///
/// `underlay` holds the cells of any connector row(s) folded into this row (in
/// pixel mode, standalone connector rows are collapsed into the following commit
/// row so lanes converge onto the dot the VSCode way). It's drawn *behind*
/// `cells` and is part of the hash, so the protocol cache stays correct.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct RowSpec {
    pub cells: Vec<PixelCell>,
    pub underlay: Vec<PixelCell>,
    /// Tails of the adjacent rows' lane-transition cubics that cross into this
    /// row (transition curves span row-center to row-center, so each boundary
    /// crossing is drawn by both rows and clipped to each frame — the halves
    /// meet pixel-exactly because they are the same cubic). Part of the hash:
    /// a row's image legitimately changes when a neighbour's transitions do.
    pub incoming: Vec<CellCurve>,
}

/// A neighbour row's transition cubic translated into this row's frame, in
/// half-cell units so every coordinate is an exact integer (lane centers are
/// odd, row edges even; this row spans y ∈ [0, 2] with mid-height 1). `col` is
/// the landing lane's cell index and `from_above`/`from_underlay` locate the
/// source cell so the per-frame dim pass can restyle the tail exactly like the
/// neighbour restyles its half.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellCurve {
    pub pts: [(i16, i16); 4],
    pub color: [u8; 3],
    pub dim: bool,
    pub col: u16,
    pub from_above: bool,
    pub from_underlay: bool,
}

/// Whether a cell's line reaches the bottom edge of its box (so a commit below
/// it would connect upward into it).
fn cell_touches_bottom(cell: &CellType) -> bool {
    matches!(
        cell,
        CellType::Pipe(_)
            | CellType::Commit(_)
            | CellType::BranchRight(_)
            | CellType::BranchLeft(_)
            | CellType::HorizontalPipe(_, _)
            | CellType::TeeRight(_)
            | CellType::TeeLeft(_)
    )
}

/// Whether a cell's line reaches the top edge of its box (so a commit above it
/// would connect downward into it).
fn cell_touches_top(cell: &CellType) -> bool {
    matches!(
        cell,
        CellType::Pipe(_)
            | CellType::Commit(_)
            | CellType::MergeRight(_)
            | CellType::MergeLeft(_)
            | CellType::HorizontalPipe(_, _)
            | CellType::TeeRight(_)
            | CellType::TeeLeft(_)
            | CellType::TeeUp(_)
    )
}

/// Map a ratatui `Color` to a concrete RGB triple. Named ANSI colors use
/// standard xterm values; `Indexed` uses the xterm-256 palette formula.
pub fn color_to_rgb(color: Color) -> [u8; 3] {
    match color {
        Color::Rgb(r, g, b) => [r, g, b],
        Color::Black => [0, 0, 0],
        Color::Red => [205, 0, 0],
        Color::Green => [0, 205, 0],
        Color::Yellow => [205, 205, 0],
        Color::Blue => [0, 0, 238],
        Color::Magenta => [205, 0, 205],
        Color::Cyan => [0, 205, 205],
        Color::Gray => [229, 229, 229],
        Color::DarkGray => [127, 127, 127],
        Color::LightRed => [255, 0, 0],
        Color::LightGreen => [0, 255, 0],
        Color::LightYellow => [255, 255, 0],
        Color::LightBlue => [92, 92, 255],
        Color::LightMagenta => [255, 0, 255],
        Color::LightCyan => [0, 255, 255],
        Color::White => [255, 255, 255],
        Color::Indexed(i) => indexed_to_rgb(i),
        Color::Reset => [192, 192, 192],
    }
}

/// xterm-256 palette lookup.
fn indexed_to_rgb(i: u8) -> [u8; 3] {
    match i {
        0..=15 => {
            const SYS: [[u8; 3]; 16] = [
                [0, 0, 0],
                [205, 0, 0],
                [0, 205, 0],
                [205, 205, 0],
                [0, 0, 238],
                [205, 0, 205],
                [0, 205, 205],
                [229, 229, 229],
                [127, 127, 127],
                [255, 0, 0],
                [0, 255, 0],
                [255, 255, 0],
                [92, 92, 255],
                [255, 0, 255],
                [0, 255, 255],
                [255, 255, 255],
            ];
            SYS[i as usize]
        }
        16..=231 => {
            let c = i - 16;
            let comp = |v: u8| -> u8 {
                if v == 0 {
                    0
                } else {
                    55 + 40 * v
                }
            };
            [comp(c / 36), comp((c / 6) % 6), comp(c % 6)]
        }
        232..=255 => {
            let v = 8 + 10 * (i - 232);
            [v, v, v]
        }
    }
}

/// Build a simple cell whose primary and secondary colors match.
fn solid(shape: CellShape, rgb: [u8; 3]) -> PixelCell {
    PixelCell {
        shape,
        color: rgb,
        secondary: rgb,
        dim: false,
        dim_secondary: false,
        curved_above: false,
        curved_below: false,
    }
}

/// Resolve one `CellType` into a `PixelCell`, computing commit-dot connectivity
/// from the effective cells directly above/below this row. For a folded commit
/// row those include the folded connector's cells (see `build_row_spec`).
/// `head_rgb` is the gold used for a HEAD commit dot (drawn as a star).
fn cell_to_pixel(
    cell: &CellType,
    col: usize,
    above: Option<&[CellType]>,
    below: Option<&[CellType]>,
    style: CommitStyle,
    theme: &Theme,
    head_rgb: [u8; 3],
) -> PixelCell {
    let rgb = |ci: usize| color_to_rgb(theme.lane_color(ci));
    let mut px = match *cell {
        CellType::Empty => solid(CellShape::Empty, [0, 0, 0]),
        CellType::Pipe(ci) => solid(CellShape::Pipe, rgb(ci)),
        CellType::Horizontal(ci) => solid(CellShape::Horizontal, rgb(ci)),
        CellType::BranchRight(ci) => solid(CellShape::BranchRight, rgb(ci)),
        CellType::BranchLeft(ci) => solid(CellShape::BranchLeft, rgb(ci)),
        CellType::MergeRight(ci) => solid(CellShape::MergeRight, rgb(ci)),
        CellType::MergeLeft(ci) => solid(CellShape::MergeLeft, rgb(ci)),
        CellType::TeeRight(ci) => solid(CellShape::TeeRight, rgb(ci)),
        CellType::TeeLeft(ci) => solid(CellShape::TeeLeft, rgb(ci)),
        CellType::TeeUp(ci) => solid(CellShape::TeeUp, rgb(ci)),
        CellType::HorizontalPipe(h, p) => PixelCell {
            shape: CellShape::HorizontalPipe,
            color: rgb(p),
            secondary: rgb(h),
            dim: false,
            dim_secondary: false,
            curved_above: false,
            curved_below: false,
        },
        CellType::Commit(ci) => {
            let connect_up = above
                .and_then(|c| c.get(col))
                .is_some_and(cell_touches_bottom);
            let connect_down = below
                .and_then(|c| c.get(col))
                .is_some_and(cell_touches_top);
            // HEAD dots render as a gold star, but their pass-through connector
            // segments keep the lane color (carried in `secondary`) so only the
            // star itself reads gold.
            let lane_rgb = rgb(ci);
            let color = if style == CommitStyle::Head { head_rgb } else { lane_rgb };
            PixelCell {
                shape: CellShape::Commit {
                    connect_up,
                    connect_down,
                    style,
                },
                color,
                secondary: lane_rgb,
                dim: false,
                dim_secondary: false,
                curved_above: false,
                curved_below: false,
            }
        }
    };
    // Pure corners in the adjacent rows (`Branch*` above; `Merge*`/`TeeUp`
    // below) carry no vertical of their own: their transition curve lands at
    // this cell's mid-height, so this cell's straight top/bottom half is
    // replaced by the incoming tail. Tee trunks and pipes keep their straight
    // through-line (a Tee has both a trunk and a curved arm). Only vertical
    // shapes take the flags so other cells' cache keys stay stable.
    if matches!(
        px.shape,
        CellShape::Pipe | CellShape::HorizontalPipe | CellShape::Commit { .. }
    ) {
        px.curved_above = above.and_then(|c| c.get(col)).is_some_and(|c| {
            matches!(c, CellType::BranchLeft(_) | CellType::BranchRight(_))
        });
        px.curved_below = below.and_then(|c| c.get(col)).is_some_and(|c| {
            matches!(
                c,
                CellType::MergeLeft(_) | CellType::MergeRight(_) | CellType::TeeUp(_)
            )
        });
    }
    px
}

/// The drawn content of a row physically adjacent to the one being built,
/// split the same way it rasterizes: folded connector `underlay` behind the
/// row's own `cells`. Used to reconstruct the neighbour's transition curves so
/// their boundary-crossing tails can be drawn into this row (`RowSpec::incoming`).
#[derive(Clone, Copy, Default)]
pub struct NeighborRow<'a> {
    pub underlay: &'a [CellType],
    pub cells: &'a [CellType],
}

/// Reconstruct the transition curves a neighbouring row draws (underlay and
/// node cells separately, exactly mirroring its two `draw_cells` passes),
/// translate them one row height into this row's frame, and keep the ones that
/// cross the shared boundary. Curves are built in half-cell units (`cw = ch =
/// 2`), where every endpoint/control coordinate is an exact integer, so the
/// result is hashable and scales to any cell size at raster time.
fn incoming_curves(
    neighbor: NeighborRow,
    from_above: bool,
    theme: &Theme,
    head_rgb: [u8; 3],
) -> Vec<CellCurve> {
    let mut out = Vec::new();
    for (cells, from_underlay) in [(neighbor.underlay, true), (neighbor.cells, false)] {
        if cells.is_empty() {
            continue;
        }
        let px: Vec<PixelCell> = cells
            .iter()
            .enumerate()
            .map(|(col, c)| cell_to_pixel(c, col, None, None, CommitStyle::Normal, theme, head_rgb))
            .collect();
        let dy = if from_above { -2.0 } else { 2.0 };
        for c in transition_curves(&px, 2.0, 2.0) {
            let pts = [c.p0, c.p1, c.p2, c.p3]
                .map(|(x, y)| (x.round() as i16, (y + dy).round() as i16));
            // Skip curves that stay inside the neighbour's own frame.
            let min_y = pts.iter().map(|p| p.1).min().unwrap_or(0);
            let max_y = pts.iter().map(|p| p.1).max().unwrap_or(0);
            if min_y >= 2 || max_y <= 0 {
                continue;
            }
            // The landing endpoint sits at this row's mid-height (y = 1); its x
            // is a lane center, `x = (col + PAD) * 2 + 1`.
            let Some(land) = [pts[0], pts[3]].into_iter().find(|p| p.1 == 1) else {
                continue;
            };
            let col = ((land.0 - 1) / 2 - PIXEL_LEFT_PAD_CELLS as i16).max(0) as u16;
            out.push(CellCurve {
                pts,
                color: c.color,
                dim: c.dim,
                col,
                from_above,
                from_underlay,
            });
        }
    }
    out
}

/// Build the `RowSpec` for `node`. `above`/`below` are the effective cells
/// physically adjacent to this row (used to resolve commit-dot connectivity);
/// in pixel mode they fold in any adjacent connector's cells. `underlay` holds
/// the folded connector cells drawn behind `node`'s own cells (empty when no
/// connector is folded here). `above_row`/`below_row` are the adjacent rows'
/// actual drawn content, used to bring their boundary-crossing transition
/// curves into this row (see `RowSpec::incoming`). Graph strokes always render
/// at full strength — merges are muted only in the message text, never in the
/// graph.
pub fn build_row_spec(
    above: Option<&[CellType]>,
    node: &GraphNode,
    below: Option<&[CellType]>,
    underlay: &[CellType],
    above_row: Option<NeighborRow>,
    below_row: Option<NeighborRow>,
    theme: &Theme,
) -> RowSpec {
    let style = if node.is_uncommitted {
        CommitStyle::Uncommitted
    } else if node.is_head {
        CommitStyle::Head
    } else {
        CommitStyle::Normal
    };
    let head_rgb = color_to_rgb(theme.head_star);
    let cells = node
        .cells
        .iter()
        .enumerate()
        .map(|(col, cell)| cell_to_pixel(cell, col, above, below, style, theme, head_rgb))
        .collect();
    // Folded connector cells carry no commit dots, so their connectivity is
    // irrelevant; resolve them with no neighbours.
    let underlay = underlay
        .iter()
        .enumerate()
        .map(|(col, cell)| {
            cell_to_pixel(cell, col, None, None, CommitStyle::Normal, theme, head_rgb)
        })
        .collect();
    let mut incoming = above_row
        .map(|n| incoming_curves(n, true, theme, head_rgb))
        .unwrap_or_default();
    incoming.extend(
        below_row
            .map(|n| incoming_curves(n, false, theme, head_rgb))
            .unwrap_or_default(),
    );
    RowSpec {
        cells,
        underlay,
        incoming,
    }
}

/// A minimal RGBA canvas with source-over compositing and coverage-based
/// anti-aliasing.
struct Canvas {
    img: RgbaImage,
    w: u32,
    h: u32,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self {
            img: RgbaImage::new(w, h),
            w,
            h,
        }
    }

    fn blend(&mut self, x: i64, y: i64, color: [u8; 3], coverage: f32) {
        if coverage <= 0.0 || x < 0 || y < 0 || x as u32 >= self.w || y as u32 >= self.h {
            return;
        }
        let sa = coverage.clamp(0.0, 1.0);
        let px = self.img.get_pixel_mut(x as u32, y as u32);
        let da = px[3] as f32 / 255.0;
        let out_a = sa + da * (1.0 - sa);
        if out_a <= 0.0 {
            return;
        }
        for i in 0..3 {
            let sc = color[i] as f32;
            let dc = px[i] as f32;
            let oc = (sc * sa + dc * da * (1.0 - sa)) / out_a;
            px[i] = oc.round().clamp(0.0, 255.0) as u8;
        }
        px[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
    }
}

fn dist_to_segment(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len2 = dx * dx + dy * dy;
    let t = if len2 <= 1e-6 {
        0.0
    } else {
        (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0)
    };
    let cx = x0 + t * dx;
    let cy = y0 + t * dy;
    let ex = px - cx;
    let ey = py - cy;
    (ex * ex + ey * ey).sqrt()
}

/// Stroke a line segment of half-width `half` (full width ≈ `2*half`).
fn draw_segment(c: &mut Canvas, x0: f32, y0: f32, x1: f32, y1: f32, half: f32, color: [u8; 3]) {
    let minx = (x0.min(x1) - half - 1.0).floor() as i64;
    let maxx = (x0.max(x1) + half + 1.0).ceil() as i64;
    let miny = (y0.min(y1) - half - 1.0).floor() as i64;
    let maxy = (y0.max(y1) + half + 1.0).ceil() as i64;
    for y in miny..=maxy {
        for x in minx..=maxx {
            let d = dist_to_segment(x as f32 + 0.5, y as f32 + 0.5, x0, y0, x1, y1);
            let cov = (half + 0.5 - d).clamp(0.0, 1.0);
            c.blend(x, y, color, cov);
        }
    }
}


fn fill_disc(c: &mut Canvas, cx: f32, cy: f32, r: f32, color: [u8; 3]) {
    let minx = (cx - r - 1.0).floor() as i64;
    let maxx = (cx + r + 1.0).ceil() as i64;
    let miny = (cy - r - 1.0).floor() as i64;
    let maxy = (cy + r + 1.0).ceil() as i64;
    for y in miny..=maxy {
        for x in minx..=maxx {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            let cov = (r + 0.5 - d).clamp(0.0, 1.0);
            c.blend(x, y, color, cov);
        }
    }
}

/// Fill a five-pointed star (one point up) via 4x4 supersampled coverage.
fn fill_star(c: &mut Canvas, cx: f32, cy: f32, r_outer: f32, color: [u8; 3]) {
    let r_inner = r_outer * 0.42;
    let verts: Vec<(f32, f32)> = (0..10)
        .map(|k| {
            let r = if k % 2 == 0 { r_outer } else { r_inner };
            let a = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::PI / 5.0;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect();
    let inside = |x: f32, y: f32| -> bool {
        let mut inside = false;
        let mut j = verts.len() - 1;
        for i in 0..verts.len() {
            let (xi, yi) = verts[i];
            let (xj, yj) = verts[j];
            if ((yi > y) != (yj > y)) && (x < (xj - xi) * (y - yi) / (yj - yi) + xi) {
                inside = !inside;
            }
            j = i;
        }
        inside
    };
    let minx = (cx - r_outer - 1.0).floor() as i64;
    let maxx = (cx + r_outer + 1.0).ceil() as i64;
    let miny = (cy - r_outer - 1.0).floor() as i64;
    let maxy = (cy + r_outer + 1.0).ceil() as i64;
    const SS: u32 = 4;
    for y in miny..=maxy {
        for x in minx..=maxx {
            let mut hits = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    if inside(px, py) {
                        hits += 1;
                    }
                }
            }
            c.blend(x, y, color, hits as f32 / (SS * SS) as f32);
        }
    }
}

/// Stroke a circle of radius `r` with half-width `half`.
fn stroke_circle(c: &mut Canvas, cx: f32, cy: f32, r: f32, half: f32, color: [u8; 3]) {
    let outer = r + half + 1.0;
    let minx = (cx - outer).floor() as i64;
    let maxx = (cx + outer).ceil() as i64;
    let miny = (cy - outer).floor() as i64;
    let maxy = (cy + outer).ceil() as i64;
    for y in miny..=maxy {
        for x in minx..=maxx {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            let cov = (half + 0.5 - (d - r).abs()).clamp(0.0, 1.0);
            c.blend(x, y, color, cov);
        }
    }
}

/// Extra transparent column rasterized to the LEFT of the graph cells, and
/// covered by the overlay one cell before the graph column. The HEAD star is
/// wider than a cell (left point at ~0.95·r), and a HEAD on lane 0 has no
/// neighbouring column inside the image — without this pad the canvas edge
/// clips the star. The text layer's leading space reserves the terminal cell.
pub const PIXEL_LEFT_PAD_CELLS: u16 = 1;

/// Rasterize a row to a transparent RGBA image of size
/// `(PIXEL_LEFT_PAD_CELLS + n_cells)*cell_w` × `cell_h`; cell `i` draws at
/// x-offset `(PIXEL_LEFT_PAD_CELLS + i)*cell_w`. Pure and deterministic.
pub fn rasterize_row(spec: &RowSpec, cell_w: u32, cell_h: u32) -> RgbaImage {
    rasterize_row_with(spec, cell_w, cell_h, transition_curves)
}

/// `rasterize_row` with an injectable transition-curve builder. Only the dev
/// before/after harness passes a non-default `curve_fn`; everything else goes
/// through `rasterize_row` (which pins `transition_curves`).
fn rasterize_row_with(spec: &RowSpec, cell_w: u32, cell_h: u32, curve_fn: CurveFn) -> RgbaImage {
    let n = spec.cells.len().max(1) as u32 + PIXEL_LEFT_PAD_CELLS as u32;
    let mut bright = Canvas::new(n * cell_w, cell_h);
    let mut dim = Canvas::new(n * cell_w, cell_h);
    let cw = cell_w as f32;
    let ch = cell_h as f32;
    // HEAD stars are deferred and drawn after every cell so they sit on top of
    // horizontal strokes from neighbouring connector columns. Each carries its
    // gold fill color.
    let mut stars: Vec<(f32, f32, f32, [u8; 3], bool)> = Vec::new();

    // Folded connector cells first (behind), then the neighbours' incoming
    // curve tails, then the row's own cells on top (so verticals and dots
    // stay above the tails, mirroring how a row's own curves layer).
    draw_cells(&mut bright, &mut dim, &spec.underlay, cw, ch, &mut stars, curve_fn);
    let half = stroke_half(ch);
    for c in &spec.incoming {
        let canvas: &mut Canvas = if c.dim { &mut dim } else { &mut bright };
        // Half-cell units → pixels; the parts outside this row clip away.
        let p = |i: usize| {
            (
                f32::from(c.pts[i].0) * cw / 2.0,
                f32::from(c.pts[i].1) * ch / 2.0,
            )
        };
        let curve = Curve {
            p0: p(0),
            p1: p(1),
            p2: p(2),
            p3: p(3),
            color: c.color,
            dim: c.dim,
        };
        draw_cubic(canvas, &curve, half);
    }
    draw_cells(&mut bright, &mut dim, &spec.cells, cw, ch, &mut stars, curve_fn);

    for (cx, cy, r, color, is_dim) in stars {
        let canvas = if is_dim { &mut dim } else { &mut bright };
        fill_star(canvas, cx, cy, r, color);
    }

    // Dim by fading the finished dim layer once, then compositing the bright
    // layer on top. Dimming per stroke instead would re-brighten wherever
    // draws overlap — source-over accumulates alpha, so a curve sampled as 48
    // overlapping segments (or a disc over its connector) came out nearly
    // opaque despite the per-stroke fade.
    let mut out = dim.img;
    for px in out.pixels_mut() {
        px[3] = (px[3] as f32 * TRACE_DIM_ALPHA).round() as u8;
    }
    for (x, y, sp) in bright.img.enumerate_pixels() {
        let sa = sp[3] as f32 / 255.0;
        if sa <= 0.0 {
            continue;
        }
        let dp = out.get_pixel_mut(x, y);
        let da = dp[3] as f32 / 255.0;
        let out_a = sa + da * (1.0 - sa);
        for i in 0..3 {
            let oc = (sp[i] as f32 * sa + dp[i] as f32 * da * (1.0 - sa)) / out_a;
            dp[i] = oc.round().clamp(0.0, 255.0) as u8;
        }
        dp[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// One endpoint of a lane transition, at an exact lane center / row-edge point.
///
/// *Hubs* sit at row mid-height: a commit dot anchoring the run, or a
/// fork-connector trunk `Tee`. *Spokes* sit at the **adjacent row's mid-height**
/// (a half-row past the row edge): a `Merge*`/`TeeUp` continues up to the lane
/// or dot centered in the row above, a `Branch*` down to the row below. The
/// out-of-row part clips away here and is drawn by the neighbouring row as an
/// *incoming* tail of the same cubic (see `RowSpec::incoming`), so the S spans
/// the full dot-to-dot distance instead of ending squished at the row edge.
/// Every endpoint is met with a *vertical* tangent (see `cubic_between`), so the
/// hub/spoke distinction only decides *where* an endpoint sits, never how the
/// curve leaves it.
#[derive(Clone, Copy)]
struct Endpoint {
    x: f32,
    y: f32,
    color: [u8; 3],
    dim: bool,
}

/// A resolved transition curve: a cubic bezier plus its color / dim layer.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Curve {
    p0: (f32, f32),
    p1: (f32, f32),
    p2: (f32, f32),
    p3: (f32, f32),
    color: [u8; 3],
    dim: bool,
}

/// Builds a row's transition curves from its cells (given cell width/height).
/// Production always uses `transition_curves`; injecting it lets a dev harness
/// swap in the pre-Bézier legacy builder for a before/after render.
type CurveFn = fn(&[PixelCell], f32, f32) -> Vec<Curve>;

/// Whether a shape contributes a horizontal component to a transition run.
fn has_horizontal(shape: CellShape) -> bool {
    matches!(
        shape,
        CellShape::Horizontal
            | CellShape::HorizontalPipe
            | CellShape::BranchLeft
            | CellShape::BranchRight
            | CellShape::MergeLeft
            | CellShape::MergeRight
            | CellShape::TeeLeft
            | CellShape::TeeRight
            | CellShape::TeeUp
    )
}

/// The horizontal edge's color and dim for a run cell (a `HorizontalPipe`
/// carries its horizontal in `secondary`/`dim_secondary`; every other
/// horizontal-family shape in its primary `color`/`dim`).
fn run_style(cell: &PixelCell) -> ([u8; 3], bool) {
    if cell.shape == CellShape::HorizontalPipe {
        (cell.secondary, cell.dim_secondary)
    } else {
        (cell.color, cell.dim)
    }
}

/// The cubic connecting two transition endpoints, shaped exactly like a VSCode
/// Git Graph lane connector: both control points sit at the endpoints' shared
/// y-midpoint, i.e. `(a.x, ymid)` and `(b.x, ymid)`. That makes the curve leave
/// and enter *every* endpoint travelling vertically, so its tangent matches the
/// straight vertical lane pipe above and below the transition — the join is
/// C1-continuous, with no kink and no flat spot. Endpoints are exact (lane
/// centers on row edges / dot centers), so rows still tile seamlessly and the
/// curve meets the dot it belongs to.
///
/// When both endpoints share a y (a flat dot↔dot / stub run, `dy == 0`) the two
/// control points collapse onto the endpoint line and the cubic degenerates to a
/// straight horizontal segment — the correct shape for a horizontal bridge.
///
/// **Wide arms** (many lanes of dx over one row of dy — fork fans) get their
/// control points pushed symmetrically *past* the midpoint: end curvature is
/// κ₀ = ⅔·dx/(h+e)², so extending the handles by `e` visibly enlarges the
/// elbow radius that otherwise reads as a hard "7" corner (#86). The push is 0
/// for arms up to ~2 lanes (the classic VSCode S is untouched), ramps with
/// dx/h, and caps at 0.6·h so the boundary crossing keeps real vertical speed
/// (at e = h the curve would run flat along the row seam). Symmetric handles
/// keep the crossing at the x-midpoint and both end tangents vertical, so the
/// two rows' clipped halves still tile exactly.
fn cubic_between(a: Endpoint, b: Endpoint) -> [(f32, f32); 4] {
    let ymid = (a.y + b.y) / 2.0;
    let h = ((b.y - a.y) / 2.0).abs();
    let e = if h > 0.0 {
        let ratio = (b.x - a.x).abs() / h;
        (0.2 * (ratio - 2.0)).clamp(0.0, 0.6) * h
    } else {
        0.0
    };
    let dir = (b.y - a.y).signum();
    [
        (a.x, a.y),
        (a.x, ymid + dir * e),
        (b.x, ymid - dir * e),
        (b.x, b.y),
    ]
}

/// Reconstruct a row's lane transitions from its cells into smooth cubic curves.
///
/// A *run* is a maximal span of horizontal-family cells. Its endpoints are the
/// corners/Tees inside it plus any commit dot immediately flanking it:
/// - *hubs* (row mid-height): a flanking commit dot, or a `TeeRight`/`TeeLeft`
///   trunk that stays on the lane.
/// - *spokes* (a row edge): `Merge*`/`TeeUp` (turn up to the top edge), `Branch*`
///   (turn down to the bottom edge).
///
/// A curve is drawn from the run's primary hub to each spoke (a branch/merge is
/// the 2-endpoint case → one curve; a fork connector fans several); with no hub,
/// spokes are chained; a lone hub with no spoke falls back to a straight bridge
/// (e.g. a trailing dot→horizontal stub). Each spoke curve takes the spoke's own
/// color/dim, so a multi-color fork connector colors each arm correctly.
///
/// Every curve is a VSCode S-curve (`cubic_between`): its control points sit at
/// the two endpoints' shared y-midpoint, so it leaves each endpoint vertically.
/// A hub→up-spoke curve therefore rises out of the dot, a hub→down-spoke curve
/// descends — curves sharing a hub separate by direction on their own, with no
/// hand-tuned hub tilt. Pure over `cells`, so the drawing stays a deterministic
/// function of the `RowSpec`.
fn transition_curves(cells: &[PixelCell], cw: f32, ch: f32) -> Vec<Curve> {
    let cx = |i: usize| (i + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw + cw / 2.0;
    let cy = ch / 2.0;
    // Spoke endpoints sit at the adjacent row's mid-height, a half-row past the
    // edge, so the S spans row-center to row-center (`cubic_between` then puts
    // the control points exactly on the row boundary). The out-of-row part is
    // clipped here and re-drawn by the neighbour as an incoming tail.
    let above_cy = -ch / 2.0;
    let below_cy = ch + ch / 2.0;
    let mut curves = Vec::new();
    let n = cells.len();
    let mut i = 0;
    while i < n {
        if !has_horizontal(cells[i].shape) {
            i += 1;
            continue;
        }
        let l = i;
        let mut r = i;
        while r + 1 < n && has_horizontal(cells[r + 1].shape) {
            r += 1;
        }

        let mut hubs: Vec<Endpoint> = Vec::new();
        let mut spokes: Vec<Endpoint> = Vec::new();
        // The run's own horizontal color/dim (from its leftmost cell), used for
        // flanking-dot hubs and the no-endpoint / lone-hub fallbacks.
        let (run_color, run_dim) = run_style(&cells[l]);

        // Commit dots immediately flanking the run anchor it at mid-height.
        let mut has_dot = false;
        if l > 0 && matches!(cells[l - 1].shape, CellShape::Commit { .. }) {
            hubs.push(Endpoint { x: cx(l - 1), y: cy, color: run_color, dim: run_dim });
            has_dot = true;
        }
        if r + 1 < n && matches!(cells[r + 1].shape, CellShape::Commit { .. }) {
            hubs.push(Endpoint { x: cx(r + 1), y: cy, color: run_color, dim: run_dim });
            has_dot = true;
        }

        for (c, cell) in cells.iter().enumerate().take(r + 1).skip(l) {
            let (color, dim) = (cell.color, cell.dim);
            match cell.shape {
                CellShape::MergeLeft | CellShape::MergeRight => {
                    spokes.push(Endpoint { x: cx(c), y: above_cy, color, dim });
                }
                CellShape::BranchLeft | CellShape::BranchRight => {
                    spokes.push(Endpoint { x: cx(c), y: below_cy, color, dim });
                }
                CellShape::TeeUp => {
                    spokes.push(Endpoint { x: cx(c), y: above_cy, color, dim });
                }
                CellShape::TeeRight | CellShape::TeeLeft => {
                    // A Tee's trunk continues DOWN to a not-yet-shown parent (the
                    // `was_existing && !already_shown` case in
                    // `build_row_cells_with_colors`). When a commit dot flanks the
                    // run this Tee is that commit's parent-connector: its arm must
                    // sweep DOWN into the descending trunk (a spoke at the bottom
                    // edge), mirroring how `Merge*` sweeps up to a parent above.
                    // Left flat at mid-height it made a commit whose parent stays
                    // on the trunk render as a dead-horizontal line (the reported
                    // bug). With no flanking dot the Tee is a fork connector's
                    // trunk hub, which stays at mid-height so its up-arms fan out.
                    if has_dot {
                        spokes.push(Endpoint { x: cx(c), y: below_cy, color, dim });
                    } else {
                        hubs.push(Endpoint { x: cx(c), y: cy, color, dim });
                    }
                }
                _ => {} // Horizontal / HorizontalPipe: run body.
            }
        }

        let push = |curves: &mut Vec<Curve>, a: Endpoint, b: Endpoint, color: [u8; 3], dim: bool| {
            let [p0, p1, p2, p3] = cubic_between(a, b);
            curves.push(Curve { p0, p1, p2, p3, color, dim });
        };

        if let Some(&primary) = hubs.first() {
            // Each arm is a VSCode S-curve leaving the shared hub vertically
            // toward its spoke's edge, so up-arms and down-arms peel apart by
            // direction with no hand-tuned hub tilt.
            for &s in &spokes {
                push(&mut curves, primary, s, s.color, s.dim);
            }
            // Extra hubs (e.g. dot ↔ dot, or a TeeLeft opposite the main): join
            // them to the primary hub with the run's own color.
            for &h in hubs.iter().skip(1) {
                push(&mut curves, primary, h, run_color, run_dim);
            }
            if spokes.is_empty() && hubs.len() == 1 {
                // Lone dot with a trailing horizontal and no far anchor: bridge
                // straight to the run's far edge so the stub still meets the dot.
                let far_x = if primary.x < cx(l) {
                    (r + 1 + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw
                } else {
                    (l + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw
                };
                let end = Endpoint { x: far_x, y: cy, color: run_color, dim: run_dim };
                push(&mut curves, primary, end, run_color, run_dim);
            }
        } else if spokes.len() >= 2 {
            // No hub: chain the spokes (e.g. a lane shifting via two corners).
            let spokes_owned = spokes.clone();
            for w in spokes_owned.windows(2) {
                push(&mut curves, w[0], w[1], w[1].color, w[1].dim);
            }
        } else if let Some(&s) = spokes.first() {
            // Lone spoke with no anchor: bridge to the run's far edge at cy.
            let far_x = if s.x <= cx(l) {
                (r + 1 + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw
            } else {
                (l + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw
            };
            let end = Endpoint { x: far_x, y: cy, color: s.color, dim: s.dim };
            push(&mut curves, end, s, s.color, s.dim);
        } else {
            // No endpoints at all (a bare horizontal run): draw it straight.
            let a = Endpoint {
                x: (l + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw,
                y: cy,
                color: run_color,
                dim: run_dim,
            };
            let b = Endpoint {
                x: (r + 1 + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw,
                y: cy,
                color: run_color,
                dim: run_dim,
            };
            push(&mut curves, a, b, run_color, run_dim);
        }

        i = r + 1;
    }
    curves
}

/// Stroke a transition cubic in the curve's own flat colour onto its own
/// dim/bright layer. Each curve keeps its per-spoke colour for its entire
/// sweep; curves stay legible against each other because `transition_curves`
/// makes each leave the shared hub vertically toward its own spoke edge, so
/// up-arms and down-arms separate by direction instead of coinciding along the
/// trunk — which previously let a bright traced curve bury a dim sibling's
/// lead-in under the bright layer (the "pink lead-in to a green branch" bug).
fn draw_cubic(canvas: &mut Canvas, curve: &Curve, half: f32) {
    let (p0, p1, p2, p3) = (curve.p0, curve.p1, curve.p2, curve.p3);
    // Tessellation density scales with the control-polygon length (an upper
    // bound on arc length), targeting ~1.5px chords. A fixed step count left
    // wide fork arms — many lanes of dx squeezed into one row of dy — with
    // ~12°-per-chord elbows that read as polygonal "7" corners (#86); the
    // tight-radius transition needs sub-pixel chords to render round.
    let ctrl_len = {
        let d = |a: (f32, f32), b: (f32, f32)| ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        d(p0, p1) + d(p1, p2) + d(p2, p3)
    };
    let steps = ((ctrl_len / 1.5).ceil() as usize).clamp(24, 400);
    let mut prev = p0;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let mt = 1.0 - t;
        let (a, b, cc, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
        let x = a * p0.0 + b * p1.0 + cc * p2.0 + d * p3.0;
        let y = a * p0.1 + b * p1.1 + cc * p2.1 + d * p3.1;
        draw_segment(canvas, prev.0, prev.1, x, y, half, curve.color);
        prev = (x, y);
    }
}

/// Stroke one row of `cells` onto the bright or dim layer. Lane transitions are
/// reconstructed row-locally into smooth cubic curves (VSCode-style S-curves)
/// drawn first; the straight verticals (lane pipes, crossed pipes, Tee trunks,
/// commit connectors) and the dots draw on top. HEAD commit stars are deferred
/// into `stars` (drawn last, on top). Shared by the folded underlay and the
/// row's own cells so they rasterize identically.
/// Stroke half-width shared by every straight segment and curve, so all lines
/// in a row (and the incoming neighbour tails) render at the same weight.
fn stroke_half(ch: f32) -> f32 {
    (ch / 10.0).max(2.0) / 2.0
}

fn draw_cells(
    bright: &mut Canvas,
    dim: &mut Canvas,
    cells: &[PixelCell],
    cw: f32,
    ch: f32,
    stars: &mut Vec<(f32, f32, f32, [u8; 3], bool)>,
    curve_fn: CurveFn,
) {
    // Stroke half-width, derived from the cell height (matches every straight
    // segment so curves and pipes are the same weight).
    let half = stroke_half(ch);
    let dot_base = cw.min(ch * 2.0 / 5.0).max(3.0);
    let dot_r = (dot_base * 0.6).min(cw * 0.65);

    // Phase A: smooth lane-transition curves (branch/merge/fork arms, crossings'
    // horizontal). Drawn beneath the verticals so a crossed pipe stays on top.
    // Flat per-arm colours; curves sharing a hub leave it vertically toward
    // their own edge, so up-arms and down-arms separate by direction. The
    // curve builder is injected (`curve_fn`) so a dev harness can render the
    // legacy algorithm for before/after comparison; production always passes
    // `transition_curves`.
    for c in curve_fn(cells, cw, ch) {
        let canvas: &mut Canvas = if c.dim { dim } else { bright };
        draw_cubic(canvas, &c, half);
    }

    // Phase B: verticals and dots, on top of the curves.
    let cy = ch / 2.0;
    for (i, cell) in cells.iter().enumerate() {
        let ox = (i + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw;
        let cx = ox + cw / 2.0;
        let color = cell.color;
        match cell.shape {
            // The crossing's vertical pipe draws on top of the transition curve
            // that sweeps under it, and dims independently of the horizontal.
            // A `curved_above`/`curved_below` half is covered by a neighbour's
            // incoming curve landing at mid-height, so the straight vertical
            // only spans the remaining half.
            CellShape::HorizontalPipe => {
                let v = if cell.dim { &mut *dim } else { &mut *bright };
                let y0 = if cell.curved_above { cy } else { 0.0 };
                let y1 = if cell.curved_below { cy } else { ch };
                draw_segment(v, cx, y0, cx, y1, half, color);
            }
            CellShape::Pipe => {
                let canvas = if cell.dim { &mut *dim } else { &mut *bright };
                let y0 = if cell.curved_above { cy } else { 0.0 };
                let y1 = if cell.curved_below { cy } else { ch };
                draw_segment(canvas, cx, y0, cx, y1, half, color);
            }
            // A Tee's trunk is the straight through-line; its arm is a curve.
            CellShape::TeeRight | CellShape::TeeLeft => {
                let canvas = if cell.dim { &mut *dim } else { &mut *bright };
                draw_segment(canvas, cx, 0.0, cx, ch, half, color);
            }
            CellShape::Commit {
                connect_up,
                connect_down,
                style,
            } => {
                let canvas: &mut Canvas = if cell.dim { dim } else { bright };
                // Connectors use the lane color (`secondary`); only the dot or
                // star itself takes the cell color (gold for HEAD). A curved
                // half's straight stub is replaced by the neighbour's incoming
                // curve, which lands exactly at the dot center.
                if connect_up && !cell.curved_above {
                    draw_segment(canvas, cx, 0.0, cx, cy, half, cell.secondary);
                }
                if connect_down && !cell.curved_below {
                    draw_segment(canvas, cx, cy, cx, ch, half, cell.secondary);
                }
                // Dots may spill slightly into the adjacent connector column;
                // lanes are two columns wide, so the overlap is harmless.
                let r = dot_r;
                match style {
                    CommitStyle::Normal => fill_disc(canvas, cx, cy, r, color),
                    CommitStyle::Head => {
                        // A point-up star spans cy-r..cy+0.81r, so it sits
                        // optically high; nudging it down recenters it and,
                        // with the larger clamp margin, keeps the top point
                        // clear of the row boundary even when the terminal
                        // places the image a pixel or two off.
                        let r_s = (r * 1.7).min(ch / 2.0 - 2.5).max(3.0);
                        stars.push((cx, cy + r_s * 0.09, r_s, color, cell.dim));
                    }
                    CommitStyle::Uncommitted => {
                        stroke_circle(canvas, cx, cy, r, half, color);
                    }
                }
            }
            // Horizontal-family shapes are drawn as curves in Phase A.
            CellShape::Empty
            | CellShape::Horizontal
            | CellShape::BranchRight
            | CellShape::BranchLeft
            | CellShape::MergeRight
            | CellShape::MergeLeft
            | CellShape::TeeUp => {}
        }
    }
}

/// Soft cap on cached protocols. On overflow the cache is pruned down to the
/// specs referenced by the current frame (see `sync_frame`) rather than cleared
/// wholesale, so the hot set survives.
const MAX_CACHED_PROTOCOLS: usize = 1024;

/// Consecutive protocol-creation failures that mark the state dead. Once dead,
/// the app falls back to the Unicode renderer instead of re-encoding every
/// frame forever.
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// Whether a detected protocol honours image transparency. Only Kitty and
/// iTerm2 qualify: Halfblocks is a non-graphics fallback, and Sixel's encoder
/// drops alpha (`to_rgb8`), which would paint black boxes over the selection
/// highlight.
fn is_supported_protocol(pt: ProtocolType) -> bool {
    matches!(pt, ProtocolType::Kitty | ProtocolType::Iterm2)
}

/// Evict the least-recently-used half of `map` once it reaches `cap`.
/// `last_used` extracts each entry's use tick. LRU beats the old
/// prune-to-current-frame policy: trace dimming mints a few new spec variants
/// per selection move, so the cap is reached routinely, and nuking all but the
/// on-screen window forced a full re-encode when scrolling back through
/// recently-visited rows. Keeping the newest half preserves that history.
fn prune_lru<V>(map: &mut HashMap<RowSpec, V>, cap: usize, last_used: impl Fn(&V) -> u64) {
    if map.len() < cap {
        return;
    }
    let mut ticks: Vec<u64> = map.values().map(&last_used).collect();
    ticks.sort_unstable();
    // Newest half survives: evict everything strictly older than the median.
    let cutoff = ticks[ticks.len() / 2];
    map.retain(|_, v| last_used(v) >= cutoff);
}

/// The half-open row range `[start, end)` whose protocols should be rasterized
/// and transmitted this frame: the visible window `[offset, offset+viewport)`
/// padded by two viewport-heights on each side, clamped to `[0, total]`. Only
/// this slice is encoded; specs outside it aren't touched until scrolled near.
pub fn protocol_window(offset: usize, viewport: usize, total: usize) -> (usize, usize) {
    window_with_pad(offset, viewport, total, viewport.saturating_mul(2))
}

/// Scroll margin of [`trace_window`], in rows.
const TRACE_WINDOW_PAD: usize = 4;

/// The sync window while selection-dependent (branch-trace) dimming is active:
/// the visible rows plus a small scroll margin. `protocol_window`'s ±2-viewport
/// pre-warm is counterproductive here — every selection move restyles rows and
/// invalidates their cached protocols, so eagerly rasterizing/encoding four
/// extra viewports of off-screen rows per keypress multiplied the invalidation
/// cost instead of amortizing it.
pub fn trace_window(offset: usize, viewport: usize, total: usize) -> (usize, usize) {
    window_with_pad(offset, viewport, total, TRACE_WINDOW_PAD)
}

fn window_with_pad(offset: usize, viewport: usize, total: usize, pad: usize) -> (usize, usize) {
    let start = offset.saturating_sub(pad).min(total);
    let end = offset
        .saturating_add(viewport)
        .saturating_add(pad)
        .min(total);
    (start, end.max(start))
}

/// Where a row's avatar image comes from.
#[derive(Debug, Clone)]
pub enum AvatarSource {
    /// A downloaded PNG/JPEG in the disk cache, decoded and circle-cropped.
    Ready(std::path::PathBuf),
    /// No avatar available — draw a deterministic colored disc.
    Fallback,
}

/// A per-row avatar to prepare: keyed by author `email`, with the `color` used
/// for the fallback disc (also if a downloaded file fails to decode).
#[derive(Debug, Clone)]
pub struct AvatarReq {
    pub email: String,
    pub source: AvatarSource,
    pub color: [u8; 3],
}

/// Live state for pixel graph rendering: the detected picker plus a cache of
/// transmitted protocols keyed by `RowSpec`.
pub struct PixelGraphState {
    picker: Picker,
    font_size: (u16, u16),
    /// Transmitted row protocols with the tick of their last frame use, for
    /// LRU eviction (`prune_lru`).
    protocols: HashMap<RowSpec, (Protocol, u64)>,
    /// Transmitted avatar protocols, keyed by author email.
    avatar_protocols: HashMap<String, Protocol>,
    consecutive_failures: u32,
    poisoned: bool,
    /// Monotonic frame counter stamped onto every protocol used in a frame.
    tick: u64,
}

impl PixelGraphState {
    /// Query the terminal for a graphics protocol and font cell size. Returns
    /// `None` unless a protocol that honours image transparency is available —
    /// only Kitty and iTerm2 qualify. Halfblocks is a non-graphics fallback,
    /// and Sixel drops the alpha channel (its encoder calls `to_rgb8`), which
    /// would paint black boxes over the selection highlight.
    ///
    /// Must be called once at startup after raw mode is enabled and before the
    /// event loop starts polling, so the terminal's query reply isn't consumed
    /// by crossterm's reader.
    pub fn new() -> Option<Self> {
        // Debug/measurement escape hatch: force a protocol without querying the
        // terminal (headless harnesses can't answer). `KEIFU_FORCE_PIXEL=kitty`
        // (or `iterm2`) exercises the full spec/rasterize/encode pipeline under
        // the debug server; the escapes land on a PTY nobody renders.
        if let Ok(forced) = std::env::var("KEIFU_FORCE_PIXEL") {
            let pt = match forced.as_str() {
                "kitty" => ProtocolType::Kitty,
                "iterm2" => ProtocolType::Iterm2,
                _ => return None,
            };
            // A fixed font size without a terminal query — deliberate: this
            // path exists precisely because no terminal will answer.
            #[allow(deprecated)]
            let mut picker = Picker::from_fontsize((10, 20));
            picker.set_protocol_type(pt);
            return Some(Self::from_picker(picker));
        }
        let picker = Picker::from_query_stdio().ok()?;
        if !is_supported_protocol(picker.protocol_type()) {
            return None;
        }
        let font_size = picker.font_size();
        if font_size.0 == 0 || font_size.1 == 0 {
            return None;
        }
        Some(Self::from_picker(picker))
    }

    fn from_picker(picker: Picker) -> Self {
        let font_size = picker.font_size();
        Self {
            picker,
            font_size,
            protocols: HashMap::new(),
            avatar_protocols: HashMap::new(),
            consecutive_failures: 0,
            poisoned: false,
            tick: 0,
        }
    }

    /// Whether the state can still render. Becomes false after
    /// `MAX_CONSECUTIVE_FAILURES` protocol-creation failures in a row.
    pub fn is_active(&self) -> bool {
        !self.poisoned
    }

    /// Prepare every protocol referenced by the current frame. Prunes the cache
    /// to the current spec set on overflow (item: bounded, no thrash), then
    /// ensures each spec. Stops early once poisoned so a persistent failure
    /// can't trigger a per-frame rasterize/encode storm.
    pub fn sync_frame(&mut self, specs: &[RowSpec]) {
        prune_lru(&mut self.protocols, MAX_CACHED_PROTOCOLS, |(_, t)| *t);
        self.tick += 1;
        let mut encoded = 0usize;
        for spec in specs {
            if self.poisoned {
                break;
            }
            encoded += self.ensure_protocol(spec) as usize;
        }
        if encoded > 0 {
            // Measurement hook (issue #79): with tracing on, this should stay
            // at the handful of rows whose lit-state the selection move
            // actually changed — the RowSpec-keyed cache covers the rest.
            tracing::debug!(encoded, window = specs.len(), "sync_frame rasterized+encoded rows");
        }
    }

    /// Rasterize and transmit the protocol for `spec` if not already cached
    /// (stamping its LRU tick either way); returns whether an encode happened.
    /// Tracks consecutive failures so a broken protocol poisons the state.
    fn ensure_protocol(&mut self, spec: &RowSpec) -> bool {
        if let Some((_, t)) = self.protocols.get_mut(spec) {
            *t = self.tick;
            return false;
        }
        let (cw, ch) = self.font_size;
        let img = rasterize_row(spec, cw as u32, ch as u32);
        let n = spec.cells.len().max(1) as u16 + PIXEL_LEFT_PAD_CELLS;
        let dyn_img = DynamicImage::ImageRgba8(img);
        // The image is already at exactly n*cell pixels, so Fit performs no
        // scaling — it just places the pixels 1:1 in the n×1 cell area.
        match self
            .picker
            .new_protocol(dyn_img, Rect::new(0, 0, n, 1), Resize::Fit(None))
        {
            Ok(proto) => {
                self.protocols.insert(spec.clone(), (proto, self.tick));
                self.consecutive_failures = 0;
                true
            }
            Err(_) => {
                self.consecutive_failures += 1;
                if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    self.poisoned = true;
                }
                false
            }
        }
    }

    /// Look up a cached protocol.
    pub fn get(&self, spec: &RowSpec) -> Option<&Protocol> {
        self.protocols.get(spec).map(|(p, _)| p)
    }

    /// Prepare an avatar protocol for each request (deduped by email). Prunes to
    /// the current request set on overflow, mirroring `sync_frame`.
    pub fn sync_avatars(&mut self, reqs: &[AvatarReq]) {
        if self.avatar_protocols.len() >= MAX_CACHED_PROTOCOLS {
            let keep: HashSet<&str> = reqs.iter().map(|r| r.email.as_str()).collect();
            self.avatar_protocols.retain(|k, _| keep.contains(k.as_str()));
        }
        for req in reqs {
            if self.poisoned {
                break;
            }
            self.ensure_avatar(req);
        }
    }

    /// Decode/generate, circle-crop, and transmit the avatar for `req` if not
    /// already cached. A decode failure falls back to the colored disc.
    fn ensure_avatar(&mut self, req: &AvatarReq) {
        if self.avatar_protocols.contains_key(&req.email) {
            return;
        }
        let (cw, ch) = self.font_size;
        let w = cw as u32 * super::graph_view::AVATAR_IMAGE_CELLS as u32;
        let h = ch as u32;
        let img = match &req.source {
            AvatarSource::Ready(path) => image::open(path)
                .ok()
                .map(|d| crate::avatar::circle_crop(&d.to_rgba8(), w, h))
                .unwrap_or_else(|| crate::avatar::fallback_disc(req.color, w, h)),
            AvatarSource::Fallback => crate::avatar::fallback_disc(req.color, w, h),
        };
        let dyn_img = DynamicImage::ImageRgba8(img);
        match self.picker.new_protocol(
            dyn_img,
            Rect::new(0, 0, super::graph_view::AVATAR_IMAGE_CELLS, 1),
            Resize::Fit(None),
        ) {
            Ok(proto) => {
                self.avatar_protocols.insert(req.email.clone(), proto);
                self.consecutive_failures = 0;
            }
            Err(_) => {
                self.consecutive_failures += 1;
                if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    self.poisoned = true;
                }
            }
        }
    }

    /// Look up a cached avatar protocol by author email.
    pub fn get_avatar(&self, email: &str) -> Option<&Protocol> {
        self.avatar_protocols.get(email)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CW: u32 = 10;
    const CH: u32 = 20;

    fn alpha(img: &RgbaImage, x: u32, y: u32) -> u8 {
        img.get_pixel(x, y)[3]
    }

    fn spec(cells: Vec<PixelCell>) -> RowSpec {
        RowSpec {
            cells,
            underlay: Vec::new(),
            incoming: Vec::new(),
        }
    }

    fn pipe(color: [u8; 3]) -> PixelCell {
        solid(CellShape::Pipe, color)
    }

    const PAD_X: u32 = PIXEL_LEFT_PAD_CELLS as u32 * CW;

    fn commit(up: bool, down: bool, style: CommitStyle, color: [u8; 3]) -> PixelCell {
        solid(
            CellShape::Commit {
                connect_up: up,
                connect_down: down,
                style,
            },
            color,
        )
    }

    #[test]
    fn pipe_touches_top_and_bottom_edges() {
        let img = rasterize_row(&spec(vec![pipe([255, 0, 0])]), CW, CH);
        let cx = PAD_X + CW / 2;
        assert!(alpha(&img, cx, 0) > 0, "pipe should touch the top edge");
        assert!(
            alpha(&img, cx, CH - 1) > 0,
            "pipe should touch the bottom edge"
        );
    }

    #[test]
    fn commit_draws_connectors_and_a_dot_wider_than_the_line() {
        let img = rasterize_row(
            &spec(vec![commit(true, true, CommitStyle::Normal, [0, 255, 0])]),
            CW,
            CH,
        );
        let cx = PAD_X + CW / 2;
        let mid = CH / 2;
        // Connectors reach both edges.
        assert!(alpha(&img, cx, 0) > 0, "connect_up should reach the top");
        assert!(
            alpha(&img, cx, CH - 1) > 0,
            "connect_down should reach the bottom"
        );
        // The dot is wider than the ~2px line: a pixel three cells off-centre on
        // the mid row is covered by the disc but never by the vertical line.
        assert!(
            alpha(&img, cx + 3, mid) > 0,
            "dot should be wider than the line"
        );
    }

    #[test]
    fn head_commit_rasterizes_a_star_in_its_cell_color_bigger_than_the_dot() {
        // build_row_spec sets the HEAD cell's color to the theme gold; the
        // rasterizer draws the star in that color. No connectors, so every
        // opaque pixel belongs to the star itself.
        let gold = [255, 200, 50];
        let img = rasterize_row(
            &spec(vec![commit(false, false, CommitStyle::Head, gold)]),
            CW,
            CH,
        );
        let cx = PAD_X + CW / 2;
        let mid = CH / 2;
        // Centre is the solid gold cell color (not a lane color).
        let centre = img.get_pixel(cx, mid);
        assert!(centre[3] > 200, "star centre should be opaque");
        assert!(
            centre[0] > 230 && centre[2] < 90,
            "star centre should be gold, got {:?}",
            centre
        );
        // The top spike reaches beyond a normal dot's radius.
        let base = (CW as f32).min(CH as f32 * 2.0 / 5.0).max(3.0);
        let dot_r = (base * 0.6).min(CW as f32 * 0.65);
        let probe_y = mid - dot_r.ceil() as u32 - 1;
        assert!(
            alpha(&img, cx, probe_y) > 0,
            "star should extend above the normal dot radius"
        );
    }

    #[test]
    fn wide_cells_leave_no_gap_between_dot_and_horizontal() {
        // With wide cells the dot radius (capped by cell height) ends short
        // of the cell edge; the neighbouring horizontal overshoots its cell
        // to meet the dot instead of leaving a hairline gap.
        const WCW: u32 = 14;
        let img = rasterize_row(
            &spec(vec![
                commit(false, false, CommitStyle::Normal, [0, 255, 0]),
                solid(CellShape::Horizontal, [0, 255, 0]),
            ]),
            WCW,
            CH,
        );
        let mid = CH / 2;
        let pad = PIXEL_LEFT_PAD_CELLS as u32 * WCW;
        let dot_cx = pad + WCW / 2;
        let horiz_cx = pad + WCW + WCW / 2;
        let gaps: Vec<u32> = (dot_cx..=horiz_cx)
            .filter(|&x| alpha(&img, x, mid) == 0)
            .collect();
        assert!(
            gaps.is_empty(),
            "transparent pixels between dot and horizontal at x={gaps:?}"
        );
    }

    #[test]
    fn head_star_row_tiles_seamlessly_at_many_font_sizes() {
        // Regression (Task 1): a HEAD-star row that connects both up and down
        // must paint its lane column continuously from the top edge to the
        // bottom edge, so it tiles against the rows above/below with no
        // hairline gap beneath the star. The star is deferred/opaque on top of
        // the connectors, so this checks the connectors reach both edges under
        // it across a range of terminal cell metrics.
        let gold = [255, 200, 50];
        for (cw, ch) in [
            (6u32, 12u32),
            (7, 14),
            (8, 16),
            (9, 18),
            (10, 20),
            (12, 24),
            (14, 28),
            (8, 20),
            (10, 24),
            (7, 21),
        ] {
            let mut cell = commit(true, true, CommitStyle::Head, gold);
            cell.secondary = [0, 0, 255];
            let img = rasterize_row(&spec(vec![cell]), cw, ch);
            let cx = PIXEL_LEFT_PAD_CELLS as u32 * cw + cw / 2;
            // Both edges are opaque…
            assert!(
                alpha(&img, cx, 0) > 0,
                "star row must reach the top edge at {cw}x{ch}"
            );
            assert!(
                alpha(&img, cx, ch - 1) > 0,
                "star row must reach the bottom edge at {cw}x{ch}"
            );
            // …and the whole lane column in between is covered (no interior gap).
            let gaps: Vec<u32> = (0..ch).filter(|&y| alpha(&img, cx, y) == 0).collect();
            assert!(
                gaps.is_empty(),
                "star row lane column has gaps at y={gaps:?} ({cw}x{ch})"
            );
        }
    }

    #[test]
    fn head_star_connectors_keep_the_lane_color() {
        // A HEAD on a blue lane: the star is gold, but the vertical connector
        // passing through the cell stays lane-blue — only the star reads gold.
        let mut cell = commit(true, true, CommitStyle::Head, [255, 200, 50]);
        cell.secondary = [0, 0, 255];
        let img = rasterize_row(&spec(vec![cell]), CW, CH);
        let cx = PAD_X + CW / 2;
        let top = img.get_pixel(cx, 0);
        assert!(top[3] > 0, "connector should reach the top edge");
        assert!(
            top[2] > 200 && top[0] < 90,
            "connector should be lane blue, got {top:?}"
        );
        let centre = img.get_pixel(cx, CH / 2);
        assert!(
            centre[0] > 230 && centre[2] < 90,
            "star centre should be gold, got {centre:?}"
        );
    }

    #[test]
    fn branch_right_touches_right_and_bottom_edges() {
        let img = rasterize_row(
            &spec(vec![solid(CellShape::BranchRight, [0, 0, 255])]),
            CW,
            CH,
        );
        // Right edge, vertically centred.
        let right_touch =
            (CH / 2 - 2..=CH / 2 + 2).any(|y| alpha(&img, PAD_X + CW - 1, y) > 0);
        assert!(right_touch, "arc should reach the right edge");
        // Bottom edge, horizontally centred.
        let cx = PAD_X + CW / 2;
        let bottom_touch = (cx - 2..=cx + 2).any(|x| alpha(&img, x, CH - 1) > 0);
        assert!(bottom_touch, "arc should reach the bottom edge");
    }

    #[test]
    fn dimmed_cells_never_exceed_trace_dim_alpha() {
        // Regression: dimming was a per-stroke alpha multiplier, so overlapping
        // draws (a curve sampled as 48 segments, a disc over its connector)
        // re-accumulated toward opaque and dimmed arcs stayed bright.
        let mut arc = solid(CellShape::BranchRight, [0, 0, 255]);
        arc.dim = true;
        let mut node = commit(true, true, CommitStyle::Normal, [0, 255, 0]);
        node.dim = true;
        let img = rasterize_row(&spec(vec![arc, node]), CW, CH);
        let cap = (TRACE_DIM_ALPHA * 255.0).round() as u8;
        let max_a = img.pixels().map(|p| p[3]).max().unwrap();
        assert!(
            max_a <= cap,
            "dimmed row alpha {max_a} exceeds the {cap} cap"
        );
    }

    #[test]
    fn horizontal_pipe_dims_each_direction_independently() {
        // A crossing where the vertical pipe is off-lineage but the horizontal
        // stroke is traced: each direction fades from its own edge instead of
        // the whole cell being all-or-nothing.
        let mut cross = solid(CellShape::HorizontalPipe, [255, 0, 0]);
        cross.secondary = [0, 255, 0];
        cross.dim = true; // vertical pipe off-lineage
        cross.dim_secondary = false; // horizontal stroke traced
        let img = rasterize_row(&spec(vec![cross]), CW, CH);
        let cap = (TRACE_DIM_ALPHA * 255.0).round() as u8;
        let cx = PAD_X + CW / 2;
        let mid = CH / 2;
        // Vertical-only pixel (top edge, centre column): dimmed.
        let v = alpha(&img, cx, 0);
        assert!(v > 0 && v <= cap, "vertical stroke should be dim, got {v}");
        // Horizontal-only pixel (left edge, mid row): full strength.
        let h = alpha(&img, PAD_X, mid);
        assert!(h > 200, "horizontal stroke should stay bright, got {h}");
    }

    #[test]
    fn dim_and_bright_cells_fade_independently() {
        // A traced (bright) pipe next to a dimmed pipe: the bright one stays
        // opaque, the dimmed one fades.
        let bright_pipe = pipe([255, 0, 0]);
        let mut dim_pipe = pipe([255, 0, 0]);
        dim_pipe.dim = true;
        let img = rasterize_row(&spec(vec![bright_pipe, dim_pipe]), CW, CH);
        let mid = CH / 2;
        assert!(alpha(&img, PAD_X + CW / 2, mid) > 200, "traced pipe opaque");
        let dim_a = alpha(&img, PAD_X + CW + CW / 2, mid);
        let cap = (TRACE_DIM_ALPHA * 255.0).round() as u8;
        assert!(
            dim_a > 0 && dim_a <= cap,
            "dimmed pipe should fade, got alpha {dim_a}"
        );
    }

    #[test]
    fn branch_right_leaves_the_far_corner_transparent() {
        let img = rasterize_row(
            &spec(vec![solid(CellShape::BranchRight, [0, 0, 255])]),
            CW,
            CH,
        );
        // Top-left corner is opposite the bottom-right arc.
        assert_eq!(
            alpha(&img, PAD_X, 0),
            0,
            "far corner must stay transparent"
        );
    }

    #[test]
    fn lane0_head_star_spills_into_the_pad_without_touching_the_image_edge() {
        // Regression: a HEAD on lane 0 used to have its left points clipped by
        // the canvas edge. The pad column absorbs the spill.
        let img = rasterize_row(
            &spec(vec![commit(false, false, CommitStyle::Head, [255, 200, 50])]),
            CW,
            CH,
        );
        let pad_hit =
            (0..CH).any(|y| (0..PAD_X).any(|x| alpha(&img, x, y) > 0));
        assert!(pad_hit, "star should spill left into the pad column");
        let edge_clear = (0..CH).all(|y| alpha(&img, 0, y) == 0);
        assert!(edge_clear, "image left edge must stay clear (star uncut)");
    }

    #[test]
    fn identical_rows_hash_equal_and_connect_bits_matter() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let hash = |s: &RowSpec| {
            let mut h = DefaultHasher::new();
            s.hash(&mut h);
            h.finish()
        };

        let a = spec(vec![commit(true, true, CommitStyle::Normal, [1, 2, 3])]);
        let b = spec(vec![commit(true, true, CommitStyle::Normal, [1, 2, 3])]);
        let c = spec(vec![commit(true, false, CommitStyle::Normal, [1, 2, 3])]);

        assert_eq!(a, b);
        assert_eq!(hash(&a), hash(&b));
        assert_ne!(a, c, "differing connect bits must not compare equal");

        // A cell's color is part of the spec's identity, so the protocol cache
        // (keyed by RowSpec) re-rasterizes when a cell's color changes.
        let recolored = spec(vec![commit(true, true, CommitStyle::Normal, [9, 9, 9])]);
        assert_ne!(a, recolored, "color must change spec identity");
        assert_ne!(hash(&a), hash(&recolored));
    }

    /// A commit-only node with a single `Commit(0)` cell.
    fn commit_node() -> GraphNode {
        GraphNode {
            commit: None,
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

    #[test]
    fn build_row_spec_reads_connectivity_from_neighbours() {
        let theme = Theme::dark();
        let node = commit_node();

        // A pipe above and below touches the shared edges → connects both ways.
        let s = build_row_spec(
            Some(&[CellType::Pipe(0)]),
            &node,
            Some(&[CellType::Pipe(0)]),
            &[],
            None,
            None,
            &theme,
        );
        assert_eq!(
            s.cells[0].shape,
            CellShape::Commit {
                connect_up: true,
                connect_down: true,
                style: CommitStyle::Normal,
            }
        );

        // A merge glyph above touches its own top, not its bottom, so the
        // commit below must NOT connect up into it.
        let s2 = build_row_spec(Some(&[CellType::MergeRight(0)]), &node, None, &[], None, None, &theme);
        assert_eq!(
            s2.cells[0].shape,
            CellShape::Commit {
                connect_up: false,
                connect_down: false,
                style: CommitStyle::Normal,
            }
        );

        // TeeUp above likewise doesn't reach down.
        let s3 = build_row_spec(Some(&[CellType::TeeUp(0)]), &node, None, &[], None, None, &theme);
        assert_eq!(
            s3.cells[0].shape,
            CellShape::Commit {
                connect_up: false,
                connect_down: false,
                style: CommitStyle::Normal,
            }
        );
    }

    #[test]
    fn build_row_spec_colors_head_gold() {
        let theme = Theme::dark();
        // A HEAD commit's dot cell takes the theme gold (drawn as the star).
        let n = GraphNode {
            commit: None,
            lane: 0,
            color_index: 0,
            branch_names: Vec::new(),
            tag_names: Vec::new(),
            is_head: true,
            is_uncommitted: false,
            is_stash: false,
            stash_label: None,
            uncommitted_count: None,
            cells: vec![CellType::Commit(0)],
            cell_oids: Vec::new(),
        };
        let head = build_row_spec(None, &n, None, &[], None, None, &theme);
        assert_eq!(head.cells[0].color, color_to_rgb(theme.head_star));
    }

    #[test]
    fn build_row_spec_folds_connector_cells_into_underlay() {
        let theme = Theme::dark();
        let node = commit_node();
        // A fork connector (TeeRight on the main lane) folded into this row.
        let connector = [CellType::TeeRight(0), CellType::Empty];
        let s = build_row_spec(None, &node, None, &connector, None, None, &theme);
        assert_eq!(s.cells.len(), 1, "the row's own cells are unchanged");
        assert_eq!(s.underlay.len(), 2, "connector cells become the underlay");
        assert_eq!(s.underlay[0].shape, CellShape::TeeRight);
    }

    #[test]
    fn underlay_is_part_of_the_spec_identity() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let hash = |s: &RowSpec| {
            let mut h = DefaultHasher::new();
            s.hash(&mut h);
            h.finish()
        };
        // Two rows with identical cells but different folded underlays must not
        // collide in the protocol cache.
        let base = spec(vec![commit(true, true, CommitStyle::Normal, [1, 2, 3])]);
        let mut folded = base.clone();
        folded.underlay = vec![solid(CellShape::TeeRight, [4, 5, 6])];
        assert_ne!(base, folded, "differing underlay must change identity");
        assert_ne!(hash(&base), hash(&folded));
    }

    #[test]
    fn folded_connector_underlay_rasterizes_behind_the_dot() {
        // A row whose dot has no connectors of its own, but whose folded
        // connector underlay carries a pipe in the neighbouring column: that
        // column paints even though the row's own cell there is Empty.
        let s = RowSpec {
            cells: vec![
                commit(false, false, CommitStyle::Normal, [0, 255, 0]),
                solid(CellShape::Empty, [0, 0, 0]),
            ],
            underlay: vec![
                solid(CellShape::Empty, [0, 0, 0]),
                pipe([255, 0, 0]),
            ],
            incoming: Vec::new(),
        };
        let img = rasterize_row(&s, CW, CH);
        // Column 1's pipe (from the underlay) reaches the top edge.
        let cx = PAD_X + CW + CW / 2;
        assert!(alpha(&img, cx, 0) > 0, "underlay pipe should paint column 1");
    }

    #[test]
    fn indexed_palette_matches_known_values() {
        assert_eq!(indexed_to_rgb(0), [0, 0, 0]);
        assert_eq!(indexed_to_rgb(15), [255, 255, 255]);
        // 6x6x6 cube corner (index 231) is pure white.
        assert_eq!(indexed_to_rgb(231), [255, 255, 255]);
        // Grayscale ramp start.
        assert_eq!(indexed_to_rgb(232), [8, 8, 8]);
    }

    #[test]
    fn only_kitty_and_iterm2_are_supported() {
        // Transparency-preserving protocols.
        assert!(is_supported_protocol(ProtocolType::Kitty));
        assert!(is_supported_protocol(ProtocolType::Iterm2));
        // Halfblocks isn't graphics; Sixel drops alpha → black boxes.
        assert!(!is_supported_protocol(ProtocolType::Halfblocks));
        assert!(!is_supported_protocol(ProtocolType::Sixel));
    }

    #[test]
    fn prune_lru_evicts_the_stale_half_at_cap() {
        // Under cap: nothing is pruned regardless of age.
        let mut map: HashMap<RowSpec, u64> = HashMap::new();
        for c in 0..5u8 {
            map.insert(spec(vec![pipe([c, 0, 0])]), c as u64);
        }
        prune_lru(&mut map, 100, |t| *t);
        assert_eq!(map.len(), 5, "under cap: no eviction");

        // At cap: the recently-used half survives, the stale half goes — so
        // scrolling back through recent history stays cache-warm (unlike the
        // old prune-to-current-frame policy, which re-encoded everything).
        prune_lru(&mut map, 5, |t| *t);
        assert!(map.len() < 5, "at cap: something evicted");
        assert!(
            map.contains_key(&spec(vec![pipe([4, 0, 0])])),
            "most recent survives"
        );
        assert!(
            !map.contains_key(&spec(vec![pipe([0, 0, 0])])),
            "oldest is evicted"
        );
    }

    #[test]
    fn truncating_a_spec_caps_width_and_preserves_hashability() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let hash = |s: &RowSpec| {
            let mut h = DefaultHasher::new();
            s.hash(&mut h);
            h.finish()
        };

        let full = spec(vec![
            pipe([1, 0, 0]),
            commit(true, true, CommitStyle::Normal, [2, 0, 0]),
            pipe([3, 0, 0]),
            pipe([4, 0, 0]),
        ]);

        // Truncate to fewer cells than present.
        let mut narrow = full.clone();
        narrow.cells.truncate(2);
        assert_eq!(narrow.cells.len(), 2);

        // A spec built directly with the same prefix cells is equal and hashes
        // equal — so a truncated spec is a stable cache key, and truncating to a
        // width >= len is a no-op.
        let expected = spec(vec![pipe([1, 0, 0]), commit(true, true, CommitStyle::Normal, [2, 0, 0])]);
        assert_eq!(narrow, expected);
        assert_eq!(hash(&narrow), hash(&expected));

        let mut wide = full.clone();
        wide.cells.truncate(10);
        assert_eq!(wide, full, "truncate beyond len is a no-op");
    }

    // ── windowed encoding ──────────────────────────────────────────────

    #[test]
    fn protocol_window_pads_and_clamps_to_the_edges() {
        // Middle: visible [50,60) padded by 2*10 on each side.
        assert_eq!(protocol_window(50, 10, 100), (30, 80));
        // Top edge: start clamps to 0.
        assert_eq!(protocol_window(0, 10, 100), (0, 30));
        // Bottom edge: end clamps to total.
        assert_eq!(protocol_window(90, 10, 100), (70, 100));
        // Window wider than the whole list.
        assert_eq!(protocol_window(0, 10, 5), (0, 5));
        // Empty list.
        assert_eq!(protocol_window(0, 10, 0), (0, 0));
    }

    /// While trace dimming is active the sync window shrinks to the viewport
    /// plus a small margin — every selection move restyles rows, so the big
    /// pre-warm pad would re-rasterize off-screen rows per keypress.
    #[test]
    fn trace_window_is_viewport_plus_small_margin() {
        // Middle: visible [50,60) padded by the fixed small margin.
        assert_eq!(trace_window(50, 10, 100), (46, 64));
        // Clamps like protocol_window.
        assert_eq!(trace_window(0, 10, 100), (0, 14));
        assert_eq!(trace_window(95, 10, 100), (91, 100));
        assert_eq!(trace_window(0, 10, 0), (0, 0));
        // Strictly inside the pre-warm window, and the viewport itself is
        // always covered.
        let (ts, te) = trace_window(50, 10, 100);
        let (ps, pe) = protocol_window(50, 10, 100);
        assert!(ps <= ts && te <= pe);
        assert!(ts <= 50 && 60 <= te);
    }

    #[test]
    fn prune_lru_keeps_newer_ticks_over_older() {
        // Four specs with ascending use ticks; prune at cap keeps the newer
        // half (median cutoff) — ties at the cutoff survive.
        let all: Vec<RowSpec> = (0..4).map(|i| spec(vec![pipe([i, 0, 0])])).collect();
        let mut map: HashMap<RowSpec, u64> = all
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, s)| (s, i as u64))
            .collect();
        prune_lru(&mut map, 4, |t| *t);
        assert_eq!(map.len(), 2);
        assert!(map.contains_key(&all[2]) && map.contains_key(&all[3]));
        assert!(!map.contains_key(&all[0]) && !map.contains_key(&all[1]));
    }

    // ── S-curve transition reconstruction (Task 2) ──────────────────────

    /// A solid cell of the given shape.
    fn sh(shape: CellShape) -> PixelCell {
        solid(shape, [10, 20, 30])
    }

    /// The lane-center x of cell index `i` at width `CW`.
    fn cell_cx(i: usize) -> f32 {
        (i + PIXEL_LEFT_PAD_CELLS as usize) as f32 * CW as f32 + CW as f32 / 2.0
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    /// Every transition curve must begin/end exactly on a lane center — at row
    /// mid-height (a hub) or at the *adjacent row's* mid-height (a spoke, a
    /// half-row past the edge). The tiling contract: the neighbouring row draws
    /// the same cubic translated (`RowSpec::incoming`), so the shared endpoint
    /// must sit exactly where the neighbour's dot/lane center is.
    #[test]
    fn transition_curve_endpoints_sit_on_lane_centers_at_adjacent_row_mid() {
        // A commit on lane 2 merges left to lane 0: [╰ H H H commit].
        let cells = vec![
            sh(CellShape::MergeRight),
            sh(CellShape::Horizontal),
            sh(CellShape::Horizontal),
            sh(CellShape::Horizontal),
            commit(false, false, CommitStyle::Normal, [9, 9, 9]),
        ];
        let curves = transition_curves(&cells, CW as f32, CH as f32);
        assert_eq!(curves.len(), 1, "one dot→corner merge curve");
        let c = curves[0];
        // Endpoints: the corner (lane 0, row-above mid-height) and the dot
        // (lane 2, this row's mid-height).
        let ends = [c.p0, c.p3];
        let corner = ends
            .iter()
            .find(|p| approx(p.1, -(CH as f32) / 2.0))
            .expect("one endpoint at the row above's mid-height (the ╰ turning up)");
        assert!(
            approx(corner.0, cell_cx(0)),
            "corner endpoint sits on lane-0 center, got x={}",
            corner.0
        );
        let dot = ends
            .iter()
            .find(|p| approx(p.1, CH as f32 / 2.0))
            .expect("one endpoint at the dot (mid-height)");
        assert!(
            approx(dot.0, cell_cx(4)),
            "dot endpoint sits on the commit's lane center, got x={}",
            dot.0
        );
    }

    /// A branch spawning downward extends to the row BELOW's mid-height; a
    /// merge to the row ABOVE's — so the full S spans dot-center to dot/lane
    /// center and each row draws its clipped half.
    #[test]
    fn branch_extends_to_row_below_mid_merge_to_row_above_mid() {
        // Branch: [commit H ╮] — lane 1 spawns downward.
        let branch = vec![
            commit(false, false, CommitStyle::Normal, [9, 9, 9]),
            sh(CellShape::Horizontal),
            sh(CellShape::BranchLeft),
        ];
        let bc = transition_curves(&branch, CW as f32, CH as f32);
        assert_eq!(bc.len(), 1);
        let spoke = [bc[0].p0, bc[0].p3]
            .into_iter()
            .find(|p| approx(p.0, cell_cx(2)))
            .expect("a spoke on lane 1");
        assert!(
            approx(spoke.1, CH as f32 * 1.5),
            "branch spoke extends to the row below's mid-height"
        );

        // Merge: [╯ ... ] on the right of a commit — [commit H ╯] lane1 merges up.
        let merge = vec![
            commit(false, false, CommitStyle::Normal, [9, 9, 9]),
            sh(CellShape::Horizontal),
            sh(CellShape::MergeLeft),
        ];
        let mc = transition_curves(&merge, CW as f32, CH as f32);
        assert_eq!(mc.len(), 1);
        let spoke = [mc[0].p0, mc[0].p3]
            .into_iter()
            .find(|p| approx(p.0, cell_cx(2)))
            .expect("a spoke on lane 1");
        assert!(
            approx(spoke.1, -(CH as f32) / 2.0),
            "merge spoke extends to the row above's mid-height"
        );
    }

    /// A fork connector `├─┴─╯` fans one curve from the main lane to each merging
    /// lane, and each curve takes that merging lane's own color.
    #[test]
    fn fork_connector_fans_one_curve_per_merging_lane() {
        let mut tee = sh(CellShape::TeeRight);
        tee.color = [1, 1, 1];
        let mut up = sh(CellShape::TeeUp);
        up.color = [2, 2, 2];
        let mut ml = sh(CellShape::MergeLeft);
        ml.color = [3, 3, 3];
        let cells = vec![
            tee,
            sh(CellShape::Horizontal),
            up,
            sh(CellShape::Horizontal),
            ml,
        ];
        let curves = transition_curves(&cells, CW as f32, CH as f32);
        assert_eq!(curves.len(), 2, "one curve per merging lane (┴ and ╯)");
        // Both curves start at the main lane (TeeRight) mid-height point.
        let main = (cell_cx(0), CH as f32 / 2.0);
        for c in &curves {
            let starts_at_main = approx(c.p0.0, main.0) && approx(c.p0.1, main.1);
            let ends_at_main = approx(c.p3.0, main.0) && approx(c.p3.1, main.1);
            assert!(starts_at_main || ends_at_main, "a fork arm touches the main lane");
        }
        // Colors: the arms carry the merging lanes' colors, not the trunk's.
        let colors: std::collections::HashSet<[u8; 3]> = curves.iter().map(|c| c.color).collect();
        assert!(colors.contains(&[2, 2, 2]), "┴ arm keeps its lane color");
        assert!(colors.contains(&[3, 3, 3]), "╯ arm keeps its lane color");
    }

    /// Regression: a commit whose parent stays on the trunk emits a `Tee` on the
    /// trunk lane while the commit sits out on its own — `[TeeRight H commit]`.
    /// The Tee's arm must sweep DOWN into the descending trunk (a spoke at the
    /// BOTTOM edge), not run flat at mid-height. The flat dot↔Tee run was the
    /// "trunk merges render as a dead-horizontal line" bug.
    #[test]
    fn commit_to_continuing_trunk_tee_turns_down_not_flat() {
        // Commit on lane 1, its parent continues down the trunk on lane 0 (┤/├).
        let cells = vec![
            sh(CellShape::TeeRight),
            sh(CellShape::Horizontal),
            commit(false, true, CommitStyle::Normal, [9, 9, 9]),
        ];
        let curves = transition_curves(&cells, CW as f32, CH as f32);
        assert_eq!(curves.len(), 1, "one dot→trunk curve");
        let ends = [curves[0].p0, curves[0].p3];
        // The dot end is at mid-height on lane 1.
        let dot = ends
            .iter()
            .find(|p| approx(p.1, CH as f32 / 2.0))
            .expect("dot endpoint at mid-height");
        assert!(approx(dot.0, cell_cx(2)), "dot sits on lane-1 center");
        // The trunk (Tee) end turns DOWN to the bottom edge on lane 0 — NOT flat
        // at mid-height, which is what made the connector look horizontal.
        let trunk = ends
            .iter()
            .find(|p| approx(p.0, cell_cx(0)))
            .expect("trunk endpoint on lane 0");
        assert!(
            approx(trunk.1, CH as f32 * 1.5),
            "trunk-side endpoint sweeps down to the row below's mid-height, got y={} (flat would be {})",
            trunk.1,
            CH as f32 / 2.0
        );
    }

    /// A fork connector's `Tee` (no commit dot flanking the run) must still be a
    /// mid-height hub, so its up-arms fan out from the trunk. Guards the
    /// `has_dot` distinction added for the flat-trunk fix above.
    #[test]
    fn fork_connector_tee_without_a_flanking_dot_stays_a_midheight_hub() {
        // [├ H ╯] connector row: no commit dot anywhere in the run.
        let cells = vec![
            sh(CellShape::TeeRight),
            sh(CellShape::Horizontal),
            sh(CellShape::MergeLeft),
        ];
        let curves = transition_curves(&cells, CW as f32, CH as f32);
        assert_eq!(curves.len(), 1, "one trunk→merging-lane arm");
        // The trunk (Tee) end stays at mid-height; the merge end turns up.
        let ends = [curves[0].p0, curves[0].p3];
        let trunk = ends
            .iter()
            .find(|p| approx(p.0, cell_cx(0)))
            .expect("trunk endpoint on lane 0");
        assert!(
            approx(trunk.1, CH as f32 / 2.0),
            "fork-connector trunk hub stays at mid-height, got y={}",
            trunk.1
        );
        let merge = ends
            .iter()
            .find(|p| approx(p.0, cell_cx(2)))
            .expect("merge endpoint on lane 1");
        assert!(
            approx(merge.1, -(CH as f32) / 2.0),
            "merge arm sweeps up to the row above's mid-height"
        );
    }

    /// A crossing (`HorizontalPipe`) is treated as run body: the transition curve
    /// sweeps through it, and the crossed vertical pipe is still painted on top.
    #[test]
    fn crossing_keeps_the_curve_and_the_crossed_pipe() {
        // [╰ H ┼ commit] — merge from lane… crossing a pipe at cell 2.
        let mut cross = solid(CellShape::HorizontalPipe, [0, 0, 0]);
        cross.color = [200, 30, 30]; // crossed pipe (vertical)
        cross.secondary = [30, 200, 30]; // the horizontal (curve) color
        let cells = vec![
            sh(CellShape::MergeRight),
            sh(CellShape::Horizontal),
            cross,
            commit(false, false, CommitStyle::Normal, [9, 9, 9]),
        ];
        // The run spans the HorizontalPipe (a single curve, not split by it).
        let curves = transition_curves(&cells, CW as f32, CH as f32);
        assert_eq!(curves.len(), 1, "crossing does not split the transition run");

        // Rasterized: the crossed pipe's column is a continuous vertical line
        // (top edge to bottom edge), drawn on top of the sweeping curve.
        let img = rasterize_row(&spec(cells), CW, CH);
        let px = PIXEL_LEFT_PAD_CELLS as u32 * CW + 2 * CW + CW / 2; // cell 2 center
        assert!(alpha(&img, px, 0) > 0, "crossed pipe reaches the top edge");
        assert!(alpha(&img, px, CH - 1) > 0, "crossed pipe reaches the bottom edge");
        let top = img.get_pixel(px, 0);
        assert!(
            top[0] > 150 && top[1] < 90,
            "crossed pipe stays its own (red) color on top, got {top:?}"
        );
    }

    /// Branch-tracing regression ("pink lead-in to a green branch"): when a
    /// bright traced curve and a dim sibling share a hub, the dim sibling's
    /// lead-in must stay visible. Each VSCode S-curve homes on its own spoke
    /// column, so an arm keeps its own flat colour there and neither buries the
    /// other — the traced arm may legitimately sweep through the sibling's
    /// column, but the sibling's own dim stroke must survive in it.
    #[test]
    fn traced_fork_arm_does_not_bury_a_dim_sibling_arm() {
        let green = [0, 255, 0];
        let magenta = [255, 0, 255];
        // Fork underlay: blue trunk (TeeRight) → NEAR arm (green TeeUp, cell 2) →
        // FAR arm (magenta MergeLeft, cell 4), joined by shared horizontals.
        // Tracing the magenta (far) branch: its cells are lit → bright; the
        // green near arm is off-lineage → dim.
        let mut hub = solid(CellShape::TeeRight, [0, 0, 255]);
        hub.dim = true;
        let mut near = solid(CellShape::TeeUp, green);
        near.dim = true; // off-lineage: dim
        let far = solid(CellShape::MergeLeft, magenta); // traced: bright (dim = false)
        let cells = vec![
            hub,
            solid(CellShape::Horizontal, magenta),
            near,
            solid(CellShape::Horizontal, magenta),
            far,
        ];
        let img = rasterize_row(&spec(cells), CW, CH);
        let cap = (TRACE_DIM_ALPHA * 255.0).round() as u8;
        // Arms cross the top row boundary at the midpoint of their hub and
        // spoke lane centers (hub cell 0 at x=15): the green near arm (spoke
        // cell 2, x=35) crosses at x=25, the magenta far arm (spoke cell 4,
        // x=55) at x=35 — separated columns, so each keeps its own colour at
        // the seam. The near arm's dim green must survive at its crossing,
        // not be buried under the far arm's bright sweep.
        let has_dim_green = (0..2).any(|y| {
            (21..28).any(|x| {
                let p = img.get_pixel(x, y).0;
                p[3] > 0 && p[3] <= cap && p[1] > 150 && p[0] < 100 && p[2] < 100
            })
        });
        assert!(has_dim_green, "near arm should keep its dim green sweep at the seam");
        // The traced far arm still renders bright magenta at its own crossing.
        let far_bright_magenta = (0..2).any(|y| {
            (33..38).any(|x| {
                let p = img.get_pixel(x, y).0;
                p[3] > cap && p[0] > 150 && p[2] > 150 && p[1] < 120
            })
        });
        assert!(far_bright_magenta, "traced far arm should stay bright magenta");
    }

    /// Cross-layer variant of the lead-in bug, reproduced from keifu's own
    /// history: the row's own cells carry a bright traced down-branch while
    /// the folded underlay carries a dim up-merge over the SAME columns (two
    /// separate runs). Because each VSCode S-curve leaves the shared dot
    /// vertically toward its own spoke edge, the down-branch descends and the
    /// up-merge rises — they part immediately, so the dim underlay lead-in
    /// stays visible above the trunk.
    #[test]
    fn bright_cells_curve_does_not_bury_dim_underlay_curve() {
        let blue = [0, 0, 255];
        let green = [0, 255, 0];
        let magenta = [255, 0, 255];
        // cells: blue dot, bright magenta run turning down (BranchLeft).
        let cells = vec![
            commit(true, true, CommitStyle::Normal, blue),
            solid(CellShape::Horizontal, magenta),
            solid(CellShape::BranchLeft, magenta),
        ];
        // underlay: dim green lead-in turning up (MergeLeft) over the same
        // columns, anchored at a dim blue TeeRight under the dot.
        let mut tee = solid(CellShape::TeeRight, blue);
        tee.dim = true;
        let mut g_run = solid(CellShape::Horizontal, green);
        g_run.dim = true;
        let mut g_turn = solid(CellShape::MergeLeft, green);
        g_turn.dim = true;
        let spec = RowSpec {
            cells,
            underlay: vec![tee, g_run, g_turn],
            incoming: Vec::new(),
        };
        let img = rasterize_row(&spec, CW, CH);
        let cap = (TRACE_DIM_ALPHA * 255.0).round() as u8;
        // The dim green lead-in survives in the SHARED run column (col 1),
        // clear of the trunk line: the up-merge S-curve rises out of the dot
        // into the top half while the bright magenta branch descends, so the
        // green is not buried under the magenta.
        let mut saw_dim_green = false;
        for x in PAD_X + CW..PAD_X + 2 * CW {
            for y in 1..CH / 2 - 2 {
                let p = img.get_pixel(x, y).0;
                if p[3] > 0 && p[3] <= cap && p[1] > 150 && p[0] < 100 && p[2] < 100 {
                    saw_dim_green = true;
                }
            }
        }
        assert!(
            saw_dim_green,
            "dim underlay lead-in should stay visible beside the bright curve in the shared run column"
        );
        // And the bright magenta branch is present in the lower half.
        let saw_bright_magenta = (PAD_X..PAD_X + 3 * CW).any(|x| {
            (CH / 2..CH).any(|y| {
                let p = img.get_pixel(x, y).0;
                p[3] > cap && p[0] > 150 && p[2] > 150 && p[1] < 120
            })
        });
        assert!(saw_bright_magenta, "bright branch curve should render");
    }

    /// A hub→spoke curve's control points sit at the two endpoints' shared
    /// y-midpoint (the VSCode S-curve). With spokes at the adjacent row's
    /// mid-height, that midpoint is exactly the row boundary — the hub-side
    /// handle stands vertically on the shared edge, giving a vertical tangent
    /// at the hub and making up-arms and down-arms sharing a dot part by
    /// direction without any hand-tuned tilt.
    #[test]
    fn hub_handle_sits_at_the_endpoint_ymidpoint() {
        // One dot fanning to an up-merge and a down-branch on the same side.
        let cells = vec![
            commit(true, true, CommitStyle::Normal, [9, 9, 9]),
            sh(CellShape::Horizontal),
            sh(CellShape::BranchLeft),
            sh(CellShape::Horizontal),
            sh(CellShape::MergeLeft),
        ];
        let curves = transition_curves(&cells, CW as f32, CH as f32);
        let down = curves
            .iter()
            .find(|c| approx(c.p3.1, CH as f32 * 1.5))
            .expect("down-branch curve");
        let up = curves
            .iter()
            .find(|c| approx(c.p3.1, -(CH as f32) / 2.0))
            .expect("up-merge curve");
        // Both control points share the hub's x (vertical tangent). A narrow
        // arm's handle sits exactly on the row boundary (the endpoints'
        // y-midpoint); a wide arm's extends past it by the elbow-widening `e`
        // (see `cubic_between`) — here the down-branch spans 2 lanes (e = 0)
        // and the up-merge 4 lanes (ratio 4 → e = 0.2·(4−2)·h = 0.4·h).
        assert!(approx(down.p1.0, down.p0.0), "down hub handle is vertical over the dot");
        assert!(
            approx(down.p1.1, CH as f32),
            "narrow down arm's handle on the bottom row boundary: {:?}",
            down.p1
        );
        assert!(approx(up.p1.0, up.p0.0), "up hub handle is vertical over the dot");
        let h = CH as f32 / 2.0;
        assert!(
            approx(up.p1.1, -0.4 * h),
            "wide up arm's handle extends 0.4h past the top boundary: {:?}",
            up.p1
        );
    }

    /// A curve keeps its own flat colour for its entire sweep: an elevated
    /// fork arm flying over a differently-coloured sibling's column must not
    /// pick up the column's colour — striping a sweep with sibling colours is
    /// a milder form of the lead-in bug. Verified both traced (arm bright,
    /// sibling dim) and untraced (all bright).
    #[test]
    fn elevated_fork_arm_keeps_its_own_color_over_a_sibling_column() {
        let green = [0, 255, 0];
        let magenta = [255, 0, 255];
        // hub (TeeRight) → a green branch turning DOWN at cell 4 (its riser lives
        // in the BOTTOM half) → a magenta merge turning UP at cell 6. Cell 4 sits
        // in the merge arm's far half, so the arm flies over it elevated, in the
        // TOP half — isolated from the down-going green riser.
        let build = |dim_siblings: bool| {
            let mut hub = solid(CellShape::TeeRight, [0, 0, 255]);
            let mut branch = solid(CellShape::BranchLeft, green);
            if dim_siblings {
                hub.dim = true;
                branch.dim = true;
            }
            vec![
                hub,
                solid(CellShape::Horizontal, magenta),
                solid(CellShape::Horizontal, magenta),
                solid(CellShape::Horizontal, magenta),
                branch,
                solid(CellShape::Horizontal, magenta),
                solid(CellShape::MergeLeft, magenta),
            ]
        };
        let cap = (TRACE_DIM_ALPHA * 255.0).round() as u8;
        // With full-height S-curves the merge arm crosses the top boundary at
        // the hub↔spoke x-midpoint ((15 + 75) / 2 = 45, over cell 3): sample
        // the top band there — the green branch dives the other way and never
        // enters it.
        let cx_over = PAD_X + 3 * CW + CW / 2;
        for traced in [false, true] {
            let img = rasterize_row(&spec(build(traced)), CW, CH);
            // Top band above the trunk corridor: only the elevated magenta
            // merge arm passes here.
            let mut saw_arm = false;
            for y in 0..(CH / 2 - 3) {
                for x in cx_over - 1..=cx_over + 1 {
                    let p = img.get_pixel(x, y).0;
                    if p[3] == 0 {
                        continue;
                    }
                    saw_arm = true;
                    let is_green = p[1] > 150 && p[0] < 100 && p[2] < 100;
                    assert!(
                        !is_green,
                        "elevated merge arm striped green (sibling colour) at ({x},{y}) traced={traced}: {p:?}"
                    );
                    // What is there is the magenta arm.
                    assert!(
                        p[0] > 120 && p[2] > 120,
                        "elevated segment should be the arm's own magenta at ({x},{y}) traced={traced}: {p:?}"
                    );
                }
            }
            assert!(saw_arm, "expected the elevated merge arm at its boundary crossing (traced={traced})");
            let _ = cap;
        }
    }

    /// The reconstructed run reaches the flanking commit dot's exact center, so
    /// the transition meets the dot with no hairline gap (subsumes the old
    /// horizontal-overshoot fix for the curve path).
    #[test]
    fn transition_curve_meets_the_flanking_dot_center() {
        let cells = vec![
            commit(false, false, CommitStyle::Normal, [9, 9, 9]),
            sh(CellShape::Horizontal),
            sh(CellShape::BranchLeft),
        ];
        let curves = transition_curves(&cells, CW as f32, CH as f32);
        let hub = [curves[0].p0, curves[0].p3]
            .into_iter()
            .find(|p| approx(p.1, CH as f32 / 2.0))
            .expect("hub at mid-height");
        assert!(
            approx(hub.0, cell_cx(0)),
            "curve meets the dot at its lane center"
        );
    }

    /// Seam continuity (the key acceptance criterion): a transition curve spans
    /// row-center to row-center, so the two rows draw the *same* cubic — the
    /// branch row's clipped lower edge and the target row's incoming tail must
    /// paint the same x-band at the shared boundary (no jump, no kink), the
    /// curve must cross the boundary at the hub↔spoke x-midpoint, and the
    /// target commit must NOT draw the old straight stub above its dot.
    #[test]
    fn seam_curve_continues_across_rows_without_stub() {
        let theme = Theme::dark();
        // Row A: commit on lane 0 spawning a branch down to lane 1 (cell 2).
        // Row B: the branch's commit on lane 1 directly below.
        let cells_a = vec![
            CellType::Commit(0),
            CellType::Horizontal(1),
            CellType::BranchLeft(1),
        ];
        let mut node_a = commit_node();
        node_a.cells = cells_a.clone();
        let cells_b = vec![CellType::Empty, CellType::Empty, CellType::Commit(1)];
        let mut node_b = commit_node();
        node_b.cells = cells_b.clone();

        let spec_a = build_row_spec(
            None,
            &node_a,
            Some(&cells_b),
            &[],
            None,
            Some(NeighborRow { underlay: &[], cells: &cells_b }),
            &theme,
        );
        let spec_b = build_row_spec(
            Some(&cells_a),
            &node_b,
            None,
            &[],
            Some(NeighborRow { underlay: &[], cells: &cells_a }),
            None,
            &theme,
        );

        // Row B carries exactly the branch curve's tail, landing on its dot.
        assert_eq!(spec_b.incoming.len(), 1, "one incoming tail from the row above");
        let tail = spec_b.incoming[0];
        assert!(tail.from_above && !tail.from_underlay);
        assert_eq!(tail.col, 2, "tail lands on the commit's lane");
        // The commit's straight up-stub is replaced by the tail.
        assert!(spec_b.cells[2].curved_above, "dot's top half is curve-fed");

        let img_a = rasterize_row(&spec_a, CW, CH);
        let img_b = rasterize_row(&spec_b, CW, CH);
        let band = |img: &RgbaImage, y: u32| -> (u32, u32) {
            let xs: Vec<u32> = (0..img.width()).filter(|&x| alpha(img, x, y) > 0).collect();
            assert!(!xs.is_empty(), "seam row y={y} must be painted");
            (*xs.iter().min().unwrap(), *xs.iter().max().unwrap())
        };
        // The shared cubic crosses the boundary at the hub↔spoke x-midpoint
        // ((15 + 35) / 2 = 25), not at the spoke lane center (35).
        let (a_min, a_max) = band(&img_a, CH - 1);
        let (b_min, b_max) = band(&img_b, 0);
        assert!(
            a_min <= 25 && 25 <= a_max,
            "row A's bottom edge band [{a_min},{a_max}] must straddle the crossing x=25"
        );
        // Both rows sample the same cubic one pixel-row apart, so B's band is
        // A's shifted by the crossing slope (dx/dy = 2 here) — they overlap
        // and stay in lockstep. A gap or jump would mean the halves diverged.
        assert!(
            b_min <= a_max && a_min <= b_max,
            "seam bands must overlap: A=[{a_min},{a_max}] vs B=[{b_min},{b_max}]"
        );
        assert!(
            a_min.abs_diff(b_min) <= 3 && a_max.abs_diff(b_max) <= 3,
            "seam bands must track the same cubic: A=[{a_min},{a_max}] vs B=[{b_min},{b_max}]"
        );
        // No straight stub on the lane column above the dot: the old geometry
        // painted (35, 0); the full-height S enters at the midpoint instead.
        let lane_cx = PAD_X + 2 * CW + CW / 2;
        assert_eq!(alpha(&img_b, lane_cx, 0), 0, "no straight stub at the lane center");
        assert_eq!(alpha(&img_b, lane_cx, 1), 0, "no straight stub at the lane center");
    }

    // ── dev before/after harness ────────────────────────────────────────
    //
    // A frozen copy of the pre-Bézier ("legacy") transition builder lives here
    // so the harness can render the OLD algorithm for side-by-side comparison
    // without keeping the dead code in the production path. It is a verbatim
    // snapshot of `transition_curves`/`cubic_between` as they were before this
    // change (horizontal-tangent hubs + `HUB_TILT`/`FAN_EXTRA` lift), producing
    // the same `Curve` type so `rasterize_row_with` can drive it.

    #[derive(Clone, Copy, PartialEq)]
    enum LegacyTangent {
        Horizontal,
        Vertical,
    }

    #[derive(Clone, Copy)]
    struct LegacyEndpoint {
        x: f32,
        y: f32,
        tan: LegacyTangent,
        color: [u8; 3],
        dim: bool,
    }

    const LEGACY_CURVE_EASE: f32 = 0.5;
    const LEGACY_HUB_TILT: f32 = 0.3;
    const LEGACY_FAN_EXTRA: f32 = 0.55;

    fn legacy_cubic(a: LegacyEndpoint, b: LegacyEndpoint, lift: f32) -> [(f32, f32); 4] {
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        let p1 = match a.tan {
            LegacyTangent::Vertical => (a.x, a.y + LEGACY_CURVE_EASE * dy),
            LegacyTangent::Horizontal => (a.x + LEGACY_CURVE_EASE * dx, a.y + lift * dy),
        };
        let p2 = match b.tan {
            LegacyTangent::Vertical => (b.x, b.y - LEGACY_CURVE_EASE * dy),
            LegacyTangent::Horizontal => (b.x - LEGACY_CURVE_EASE * dx, b.y - lift * dy),
        };
        [(a.x, a.y), p1, p2, (b.x, b.y)]
    }

    /// Frozen pre-Bézier transition builder. Dev harness only.
    fn legacy_curves(cells: &[PixelCell], cw: f32, ch: f32) -> Vec<Curve> {
        let cx = |i: usize| (i + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw + cw / 2.0;
        let cy = ch / 2.0;
        let mut curves = Vec::new();
        let n = cells.len();
        let mut i = 0;
        while i < n {
            if !has_horizontal(cells[i].shape) {
                i += 1;
                continue;
            }
            let l = i;
            let mut r = i;
            while r + 1 < n && has_horizontal(cells[r + 1].shape) {
                r += 1;
            }
            let mut hubs: Vec<LegacyEndpoint> = Vec::new();
            let mut spokes: Vec<LegacyEndpoint> = Vec::new();
            let (run_color, run_dim) = run_style(&cells[l]);
            let mut has_dot = false;
            if l > 0 && matches!(cells[l - 1].shape, CellShape::Commit { .. }) {
                hubs.push(LegacyEndpoint { x: cx(l - 1), y: cy, tan: LegacyTangent::Horizontal, color: run_color, dim: run_dim });
                has_dot = true;
            }
            if r + 1 < n && matches!(cells[r + 1].shape, CellShape::Commit { .. }) {
                hubs.push(LegacyEndpoint { x: cx(r + 1), y: cy, tan: LegacyTangent::Horizontal, color: run_color, dim: run_dim });
                has_dot = true;
            }
            for (c, cell) in cells.iter().enumerate().take(r + 1).skip(l) {
                let (color, dim) = (cell.color, cell.dim);
                match cell.shape {
                    CellShape::MergeLeft | CellShape::MergeRight | CellShape::TeeUp => {
                        spokes.push(LegacyEndpoint { x: cx(c), y: 0.0, tan: LegacyTangent::Vertical, color, dim });
                    }
                    CellShape::BranchLeft | CellShape::BranchRight => {
                        spokes.push(LegacyEndpoint { x: cx(c), y: ch, tan: LegacyTangent::Vertical, color, dim });
                    }
                    CellShape::TeeRight | CellShape::TeeLeft => {
                        if has_dot {
                            spokes.push(LegacyEndpoint { x: cx(c), y: ch, tan: LegacyTangent::Vertical, color, dim });
                        } else {
                            hubs.push(LegacyEndpoint { x: cx(c), y: cy, tan: LegacyTangent::Horizontal, color, dim });
                        }
                    }
                    _ => {}
                }
            }
            let push = |curves: &mut Vec<Curve>, a: LegacyEndpoint, b: LegacyEndpoint, color: [u8; 3], dim: bool, lift: f32| {
                let [p0, p1, p2, p3] = legacy_cubic(a, b, lift);
                curves.push(Curve { p0, p1, p2, p3, color, dim });
            };
            if let Some(&primary) = hubs.first() {
                let side_max = |sign: bool| {
                    spokes.iter().map(|s| s.x - primary.x).filter(|dx| (*dx > 0.0) == sign).map(f32::abs).fold(0.0f32, f32::max)
                };
                let (max_r, max_l) = (side_max(true), side_max(false));
                for &s in &spokes {
                    let dx = s.x - primary.x;
                    let side_span = if dx > 0.0 { max_r } else { max_l };
                    let fan_extra = if side_span > 0.0 { LEGACY_FAN_EXTRA * (1.0 - dx.abs() / side_span) } else { 0.0 };
                    push(&mut curves, primary, s, s.color, s.dim, LEGACY_HUB_TILT + fan_extra);
                }
                for &h in hubs.iter().skip(1) {
                    push(&mut curves, primary, h, run_color, run_dim, 0.0);
                }
                if spokes.is_empty() && hubs.len() == 1 {
                    let far_x = if primary.x < cx(l) {
                        (r + 1 + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw
                    } else {
                        (l + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw
                    };
                    let end = LegacyEndpoint { x: far_x, y: cy, tan: LegacyTangent::Horizontal, color: run_color, dim: run_dim };
                    push(&mut curves, primary, end, run_color, run_dim, 0.0);
                }
            } else if spokes.len() >= 2 {
                let spokes_owned = spokes.clone();
                for w in spokes_owned.windows(2) {
                    push(&mut curves, w[0], w[1], w[1].color, w[1].dim, 0.0);
                }
            } else if let Some(&s) = spokes.first() {
                let far_x = if s.x <= cx(l) {
                    (r + 1 + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw
                } else {
                    (l + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw
                };
                let end = LegacyEndpoint { x: far_x, y: cy, tan: LegacyTangent::Horizontal, color: s.color, dim: s.dim };
                push(&mut curves, end, s, s.color, s.dim, 0.0);
            } else {
                let a = LegacyEndpoint { x: (l + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw, y: cy, tan: LegacyTangent::Horizontal, color: run_color, dim: run_dim };
                let b = LegacyEndpoint { x: (r + 1 + PIXEL_LEFT_PAD_CELLS as usize) as f32 * cw, y: cy, tan: LegacyTangent::Horizontal, color: run_color, dim: run_dim };
                push(&mut curves, a, b, run_color, run_dim, 0.0);
            }
            i = r + 1;
        }
        curves
    }

    /// Representative connectors (task set): a one-lane S, a wide multi-lane S,
    /// a merge-into-trunk curve (the red one), and a fork fan. Each scene is a
    /// short stack of rows — an incoming pipe row, the transition row, an
    /// outgoing pipe row — so the before/after PNGs show whether the curve
    /// blends into the straight vertical lane pipes at the seam.
    fn harness_scenes() -> Vec<Vec<Vec<PixelCell>>> {
        let cyan = [0, 200, 220];
        let green = [0, 200, 80];
        let orange = [230, 150, 0];
        let magenta = [200, 0, 200];
        let blue = [40, 120, 255];
        let red = [220, 50, 47];
        let e = || sh(CellShape::Empty);
        vec![
            // One-lane S: lane 0 → lane 1.
            vec![
                vec![pipe(cyan), e()],
                vec![solid(CellShape::MergeRight, cyan), solid(CellShape::BranchLeft, cyan)],
                vec![e(), pipe(cyan)],
            ],
            // Wide multi-lane S: lane 0 → lane 4.
            vec![
                vec![pipe(green), e(), e(), e(), e()],
                vec![
                    solid(CellShape::MergeRight, green),
                    solid(CellShape::Horizontal, green),
                    solid(CellShape::Horizontal, green),
                    solid(CellShape::Horizontal, green),
                    solid(CellShape::BranchLeft, green),
                ],
                vec![e(), e(), e(), e(), pipe(green)],
            ],
            // Merge into the descending trunk (the 1e48dba red case): an orange
            // dot on lane 2 whose parent stays on the red trunk (lane 0).
            vec![
                vec![pipe(red), e(), e()],
                vec![
                    solid(CellShape::TeeRight, red),
                    solid(CellShape::Horizontal, red),
                    commit(false, false, CommitStyle::Normal, orange),
                ],
                vec![pipe(red), e(), e()],
            ],
            // Fork fan: blue trunk fanning up to lane 2 (green) and lane 4 (magenta).
            vec![
                vec![e(), e(), pipe(green), e(), pipe(magenta)],
                vec![
                    solid(CellShape::TeeRight, blue),
                    solid(CellShape::Horizontal, green),
                    solid(CellShape::TeeUp, green),
                    solid(CellShape::Horizontal, magenta),
                    solid(CellShape::MergeLeft, magenta),
                ],
                vec![pipe(blue), e(), e(), e(), e()],
            ],
        ]
    }

    /// Composite every scene's rows onto a dark opaque background at cell size
    /// `cw`×`ch`, using `curve_fn` to build the transition curves.
    fn render_scenes(scenes: &[Vec<Vec<PixelCell>>], cw: u32, ch: u32, curve_fn: CurveFn) -> RgbaImage {
        let bg = image::Rgba([13u8, 17, 23, 255]);
        let max_cells = scenes.iter().flatten().map(|r| r.len()).max().unwrap_or(1);
        let w = (max_cells as u32 + PIXEL_LEFT_PAD_CELLS as u32) * cw;
        let gap = ch / 2;
        let rows: u32 = scenes.iter().map(|s| s.len() as u32).sum();
        let h = rows * ch + gap * (scenes.len() as u32 + 1);
        let mut out = RgbaImage::from_pixel(w, h, bg);
        let mut y = gap;
        for scene in scenes {
            for row in scene {
                let s = RowSpec {
                    cells: row.clone(),
                    underlay: Vec::new(),
                    incoming: Vec::new(),
                };
                let img = rasterize_row_with(&s, cw, ch, curve_fn);
                for (px, py, p) in img.enumerate_pixels() {
                    let a = p[3] as f32 / 255.0;
                    if a <= 0.0 {
                        continue;
                    }
                    let d = out.get_pixel_mut(px, y + py);
                    for k in 0..3 {
                        d[k] = (p[k] as f32 * a + d[k] as f32 * (1.0 - a)).round() as u8;
                    }
                }
                y += ch;
            }
            y += gap;
        }
        out
    }

    /// Dev harness (ignored by default): rasterizes the representative connector
    /// set with the legacy algorithm and the new VSCode-Bézier algorithm and
    /// writes `curves-before{,-1x}.png` / `curves-after{,-1x}.png` for human
    /// review. The 4x files are nearest-neighbour upscales for easy comparison.
    ///
    /// Run with: `cargo test --lib dump_curve_comparison_pngs -- --ignored`.
    /// Output dir defaults to the OS temp dir; override with
    /// `KEIFU_CURVE_HARNESS_DIR`. No PNG-encoder dependency is added — the
    /// `image` crate (already a dependency, with the `png` feature) encodes it.
    #[test]
    #[ignore = "dev harness: writes before/after curve PNGs for human review"]
    fn dump_curve_comparison_pngs() {
        let scenes = harness_scenes();
        let (cw, ch) = (12u32, 26u32); // realistic terminal cell metrics
        let dir = std::env::var("KEIFU_CURVE_HARNESS_DIR").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("keifu-curve-harness")
                .to_string_lossy()
                .into_owned()
        });
        std::fs::create_dir_all(&dir).expect("create output dir");
        for (name, curve_fn) in [
            ("before", legacy_curves as CurveFn),
            ("after", transition_curves as CurveFn),
        ] {
            let img = render_scenes(&scenes, cw, ch, curve_fn);
            let p1 = format!("{dir}/curves-{name}-1x.png");
            img.save(&p1).expect("save 1x png");
            let up = image::imageops::resize(
                &img,
                img.width() * 4,
                img.height() * 4,
                image::imageops::FilterType::Nearest,
            );
            let p4 = format!("{dir}/curves-{name}.png");
            up.save(&p4).expect("save 4x png");
            eprintln!("wrote {p1}\nwrote {p4}");
        }
    }
}
