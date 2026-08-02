use std::time::Duration;

use keifu::ui::graph_pixels::PixelGraphState;
use ratatui_image::picker::{Picker, ProtocolType};

#[test]
fn startup_query_bounds_unresponsive_terminals_and_keeps_existing_fallbacks() {
    let mut observed_timeout = None;
    let fallback = PixelGraphState::from_startup_query(|options| {
        observed_timeout = Some(options.timeout);
        Ok::<_, ()>(Picker::halfblocks())
    });

    assert_eq!(
        observed_timeout,
        Some(Duration::from_millis(250)),
        "the startup query reaches the Unicode fallback promptly"
    );
    assert!(
        fallback.is_none(),
        "unsupported picker responses retain the existing Unicode fallback"
    );

    let detected = PixelGraphState::from_startup_query(|_| {
        #[allow(deprecated)]
        let mut picker = Picker::from_fontsize((10, 20));
        picker.set_protocol_type(ProtocolType::Kitty);
        Ok::<_, ()>(picker)
    });
    assert!(
        detected.is_some(),
        "supported picker responses still enable the pixel graph"
    );
}
