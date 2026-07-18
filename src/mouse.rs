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
