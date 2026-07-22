//! Graph-column geometry: leading columns, avatar reservations, effective graph
//! width / cap arithmetic, and the per-row truncation budget shared by the
//! unicode and pixel renderers.

use ratatui::style::{Modifier, Style};

use crate::config::MetadataColumns;
use crate::ui::theme::Theme;

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
pub(super) fn graph_truncation(n: usize, graph_width: usize) -> (usize, bool) {
    if n > graph_width {
        (graph_width.saturating_sub(1), true)
    } else {
        (n, false)
    }
}

/// Cells drawn in a pixel row's image: the `graph_width` truncation budget,
/// further bounded by what fits the panel (`panel_available`). The `…` marker,
/// when truncating, is drawn by the text layer, so it's excluded here.
pub(super) fn pixel_row_cells(n: usize, graph_width: usize, panel_available: usize) -> usize {
    graph_truncation(n, graph_width).0.min(panel_available)
}

/// Dim style for the truncation `…` marker.
pub(super) fn ellipsis_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.text_muted)
        .add_modifier(Modifier::DIM)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
