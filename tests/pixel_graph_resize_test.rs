use crossterm::{event::Event, terminal::WindowSize};
use keifu::{event::dispatch_pixel_graph_resize, ui::graph_pixels::PixelGraphState};

#[test]
fn resize_event_refreshes_pixel_graph_cell_geometry() {
    std::env::set_var("KEIFU_FORCE_PIXEL", "iterm2");
    let mut state = PixelGraphState::new().expect("forced iTerm2 pixel graph state");
    std::env::remove_var("KEIFU_FORCE_PIXEL");

    dispatch_pixel_graph_resize(&Event::Resize(100, 40), Some(&mut state), || {
        Some(WindowSize {
            columns: 100,
            rows: 40,
            width: 2_000,
            height: 1_600,
        })
    });

    assert_eq!(state.cell_size(), (20, 40));
}
