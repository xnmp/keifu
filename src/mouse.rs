//! Pure mouse geometry: hit-testing, double-click detection, popup/menu
//! placement, and divider-drag math. Kept free of `App` and time sources so it
//! is unit-testable; callers inject the clock and the rendered rectangles.

use std::time::{Duration, Instant};

use ratatui::layout::Rect;

/// Window within which a second click on the same cell counts as a double-click.
pub const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);

/// Whether `(col, row)` falls inside `rect`.
pub fn point_in(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
}

/// The list index a click maps to, given the list's *inner* area (inside the
/// border) and its scroll `offset`. `None` if the click is outside the inner
/// area. The returned index is in the list's own (possibly folded/filtered)
/// row space — the caller maps it to a node/file.
pub fn list_row_index(inner: Rect, offset: usize, col: u16, row: u16) -> Option<usize> {
    if !point_in(inner, col, row) {
        return None;
    }
    Some(offset + (row - inner.y) as usize)
}

/// Top-left position for a popup of size `w`×`h` opened at `anchor`, clamped so
/// it never crosses the screen edges (shifted left/up as needed, then to 0).
pub fn clamp_menu_pos(anchor: (u16, u16), w: u16, h: u16, screen: (u16, u16)) -> (u16, u16) {
    let (ax, ay) = anchor;
    let (sw, sh) = screen;
    let x = ax.min(sw.saturating_sub(w));
    let y = ay.min(sh.saturating_sub(h));
    (x, y)
}

/// What a clickable chip on a commit row points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChipTarget {
    /// The open-PR badge — opens the PR in the browser.
    PrBadge,
    /// A branch label — checks out that branch.
    Branch(String),
}

/// A clickable region on a rendered commit row, in line-column space (columns
/// measured from the start of the row's text, i.e. the panel's inner-left edge).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipHit {
    pub x_start: u16,
    pub x_end: u16,
    pub target: ChipTarget,
}

/// The chip whose `[x_start, x_end)` span contains `line_col`, if any.
pub fn chip_at(chips: &[ChipHit], line_col: u16) -> Option<&ChipHit> {
    chips
        .iter()
        .find(|c| line_col >= c.x_start && line_col < c.x_end)
}

/// Bounds on the graph/detail split ratio (graph-pane percentage).
pub const MIN_SPLIT_RATIO: u16 = 20;
pub const MAX_SPLIT_RATIO: u16 = 80;

/// Clamp a split percentage into the allowed `[MIN_SPLIT_RATIO, MAX_SPLIT_RATIO]`.
pub fn clamp_split_ratio(pct: i32) -> u16 {
    pct.clamp(MIN_SPLIT_RATIO as i32, MAX_SPLIT_RATIO as i32) as u16
}

/// Whether `(col, row)` lands on the divider between the graph and detail
/// panes (within ±1 cell). `graph` is the graph panel's rect. In the stacked
/// layout the divider is the graph's bottom edge; in the side layout the graph
/// sits on the right, so the divider is its left edge.
pub fn on_divider(graph: Rect, side_layout: bool, col: u16, row: u16) -> bool {
    if side_layout {
        let within = row >= graph.y && row < graph.y + graph.height;
        within && (col as i32 - graph.x as i32).abs() <= 1
    } else {
        let boundary = graph.y + graph.height;
        let within = col >= graph.x && col < graph.x + graph.width;
        within && (row as i32 - boundary as i32).abs() <= 1
    }
}

/// The graph-pane percentage implied by dragging the divider to `(col, row)`
/// within `main`, clamped to the allowed range. In the stacked layout the graph
/// is on top, so its share grows as the divider moves down; in the side layout
/// the graph is on the right, so its share grows as the divider moves left.
pub fn divider_ratio(main: Rect, side_layout: bool, col: u16, row: u16) -> u16 {
    if side_layout {
        if main.width == 0 {
            return clamp_split_ratio(MAX_SPLIT_RATIO as i32);
        }
        let from_right = (main.x + main.width).saturating_sub(col) as i32;
        clamp_split_ratio(from_right * 100 / main.width as i32)
    } else {
        if main.height == 0 {
            return clamp_split_ratio(MAX_SPLIT_RATIO as i32);
        }
        let from_top = row.saturating_sub(main.y) as i32;
        clamp_split_ratio(from_top * 100 / main.height as i32)
    }
}

/// A prior click, for double-click detection.
#[derive(Debug, Clone, Copy)]
pub struct LastClick {
    pub col: u16,
    pub row: u16,
    pub at: Instant,
}

/// Whether a click at `(col, row, now)` is a double-click relative to `prev`:
/// same cell, within `window`.
pub fn is_double_click(
    prev: Option<LastClick>,
    col: u16,
    row: u16,
    now: Instant,
    window: Duration,
) -> bool {
    match prev {
        Some(p) => p.col == col && p.row == row && now.duration_since(p.at) <= window,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn point_in_bounds() {
        let r = rect(5, 2, 10, 4); // cols 5..15, rows 2..6
        assert!(point_in(r, 5, 2));
        assert!(point_in(r, 14, 5));
        assert!(!point_in(r, 15, 5), "right edge exclusive");
        assert!(!point_in(r, 14, 6), "bottom edge exclusive");
        assert!(!point_in(r, 4, 3));
    }

    #[test]
    fn list_row_index_maps_with_offset() {
        // Inner area at y=3, height 5 → rows 3,4,5,6,7.
        let inner = rect(1, 3, 20, 5);
        assert_eq!(list_row_index(inner, 0, 2, 3), Some(0));
        assert_eq!(list_row_index(inner, 0, 2, 7), Some(4));
        // With a scroll offset the first visible row is offset+0.
        assert_eq!(list_row_index(inner, 10, 2, 3), Some(10));
        assert_eq!(list_row_index(inner, 10, 2, 5), Some(12));
        // Outside the inner area → None.
        assert_eq!(list_row_index(inner, 0, 2, 8), None);
        assert_eq!(list_row_index(inner, 0, 0, 4), None);
    }

    #[test]
    fn clamp_menu_keeps_it_on_screen() {
        let screen = (80, 24);
        // Fits at the anchor.
        assert_eq!(clamp_menu_pos((10, 5), 20, 8, screen), (10, 5));
        // Near the right edge → shifted left so x+w == sw.
        assert_eq!(clamp_menu_pos((70, 5), 20, 8, screen), (60, 5));
        // Near the bottom → shifted up so y+h == sh.
        assert_eq!(clamp_menu_pos((10, 22), 20, 8, screen), (10, 16));
        // Both edges.
        assert_eq!(clamp_menu_pos((79, 23), 20, 8, screen), (60, 16));
        // Menu larger than screen → clamps to 0.
        assert_eq!(clamp_menu_pos((10, 5), 100, 40, screen), (0, 0));
    }

    #[test]
    fn clamp_split_ratio_bounds() {
        assert_eq!(clamp_split_ratio(65), 65);
        assert_eq!(clamp_split_ratio(5), MIN_SPLIT_RATIO);
        assert_eq!(clamp_split_ratio(95), MAX_SPLIT_RATIO);
        assert_eq!(clamp_split_ratio(-10), MIN_SPLIT_RATIO);
    }

    #[test]
    fn on_divider_detects_the_boundary() {
        // Stacked: graph at y=0, height 10 → divider at row 10, ±1.
        let graph = rect(0, 0, 40, 10);
        assert!(on_divider(graph, false, 5, 10));
        assert!(on_divider(graph, false, 5, 9));
        assert!(on_divider(graph, false, 5, 11));
        assert!(!on_divider(graph, false, 5, 12));
        assert!(!on_divider(graph, false, 41, 10), "outside the columns");
        // Side: graph on the right at x=20 → divider at col 20, ±1.
        let graph = rect(20, 0, 30, 12);
        assert!(on_divider(graph, true, 20, 5));
        assert!(on_divider(graph, true, 19, 5));
        assert!(on_divider(graph, true, 21, 5));
        assert!(!on_divider(graph, true, 23, 5));
        assert!(!on_divider(graph, true, 20, 20), "outside the rows");
    }

    #[test]
    fn divider_ratio_maps_position_to_graph_share() {
        // Stacked: main y=0 height 100. Dragging to row 40 → graph gets 40%.
        let main = rect(0, 0, 100, 100);
        assert_eq!(divider_ratio(main, false, 10, 40), 40);
        assert_eq!(divider_ratio(main, false, 10, 65), 65);
        // Clamped at the extremes.
        assert_eq!(divider_ratio(main, false, 10, 5), MIN_SPLIT_RATIO);
        assert_eq!(divider_ratio(main, false, 10, 95), MAX_SPLIT_RATIO);
        // Side: main x=0 width 100. Graph on the right; dragging to col 30
        // leaves 70 columns to the right → graph 70%.
        assert_eq!(divider_ratio(main, true, 30, 10), 70);
        assert_eq!(divider_ratio(main, true, 40, 10), 60);
    }

    #[test]
    fn chip_at_finds_containing_span() {
        let chips = vec![
            ChipHit {
                x_start: 4,
                x_end: 10,
                target: ChipTarget::Branch("main".into()),
            },
            ChipHit {
                x_start: 11,
                x_end: 16,
                target: ChipTarget::PrBadge,
            },
        ];
        assert_eq!(chip_at(&chips, 4).map(|c| &c.target), Some(&ChipTarget::Branch("main".into())));
        assert_eq!(chip_at(&chips, 9).map(|c| &c.target), Some(&ChipTarget::Branch("main".into())));
        assert_eq!(chip_at(&chips, 10), None, "right edge exclusive; gap between chips");
        assert_eq!(chip_at(&chips, 11).map(|c| &c.target), Some(&ChipTarget::PrBadge));
        assert_eq!(chip_at(&chips, 15).map(|c| &c.target), Some(&ChipTarget::PrBadge));
        assert_eq!(chip_at(&chips, 16), None);
        assert_eq!(chip_at(&chips, 0), None);
    }

    #[test]
    fn double_click_same_cell_within_window() {
        let base = Instant::now();
        let prev = LastClick {
            col: 4,
            row: 6,
            at: base,
        };
        // Same cell, 200ms later → double.
        assert!(is_double_click(
            Some(prev),
            4,
            6,
            base + Duration::from_millis(200),
            DOUBLE_CLICK_WINDOW
        ));
        // Same cell but too slow → not double.
        assert!(!is_double_click(
            Some(prev),
            4,
            6,
            base + Duration::from_millis(500),
            DOUBLE_CLICK_WINDOW
        ));
        // Different cell, in time → not double.
        assert!(!is_double_click(
            Some(prev),
            5,
            6,
            base + Duration::from_millis(100),
            DOUBLE_CLICK_WINDOW
        ));
        // No prior click → not double.
        assert!(!is_double_click(None, 4, 6, base, DOUBLE_CLICK_WINDOW));
    }
}
