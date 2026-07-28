use keifu::graph::colors::{get_color_by_index, MAIN_BRANCH_COLOR};
use keifu::ui::theme::Theme;
use ratatui::style::Color;

#[test]
fn dark_theme_renders_the_trunk_as_bright_blue() {
    assert_eq!(
        Theme::dark().lane_color(MAIN_BRANCH_COLOR),
        Color::Rgb(88, 166, 255)
    );
}

#[test]
fn light_theme_renders_the_trunk_as_blue() {
    assert_eq!(Theme::light().lane_color(MAIN_BRANCH_COLOR), Color::Blue);
}

#[test]
fn changing_the_trunk_color_does_not_change_sentinel_colors() {
    assert_eq!(get_color_by_index(usize::MAX), Color::DarkGray);
    assert_eq!(get_color_by_index(usize::MAX - 1), Color::DarkGray);
}
