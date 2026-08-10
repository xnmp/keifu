//! Regression coverage for shared scrollbar state at clamped offsets.

use keifu::ui::{render_scrollbar_for_test, theme::Theme};
use ratatui::{backend::TestBackend, layout::Rect, Terminal};

#[test]
fn final_scroll_offset_places_thumb_at_track_bottom() {
    let area = Rect::new(0, 0, 10, 8);
    let theme = Theme::dark();

    let mut top = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    top.draw(|frame| render_scrollbar_for_test(frame, &theme, area, 8, 6, 0))
        .unwrap();
    assert_ne!(
        top.backend().buffer()[(area.width - 1, area.height - 2)].symbol(),
        "█",
        "further scrollability leaves track visible below the thumb"
    );

    let mut bottom = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
    bottom
        .draw(|frame| render_scrollbar_for_test(frame, &theme, area, 8, 6, 2))
        .unwrap();
    assert_eq!(
        bottom.backend().buffer()[(area.width - 1, area.height - 2)].symbol(),
        "█",
        "the final reachable offset puts the thumb at the track bottom"
    );
}
