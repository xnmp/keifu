use keifu::ui::graph_pixels::PixelGraphState;

#[test]
fn resize_changes_the_forced_pixel_graph_cell_geometry() {
    std::env::set_var("KEIFU_FORCE_PIXEL", "iterm2");
    let mut state = PixelGraphState::new().expect("forced iTerm2 pixel graph state");
    std::env::remove_var("KEIFU_FORCE_PIXEL");

    state.refresh_font_size((20, 40));

    assert_eq!(state.cell_size(), (20, 40));
}
