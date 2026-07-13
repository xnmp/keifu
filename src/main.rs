//! keifu: a TUI tool that shows Git commit graphs

use anyhow::Result;
use clap::Parser;

use std::time::{Duration, Instant};

use keifu::{
    app::{App, AppMode},
    event::{get_key_event, get_mouse_scroll, poll_event_with_timeout},
    git::configure_git_extensions,
    keybindings::map_key_to_action,
    tui, ui,
};

#[derive(Parser)]
#[command(name = "keifu")]
#[command(
    version,
    about = "A TUI tool to visualize Git commit graphs with branch genealogy"
)]
struct Cli {}

fn main() -> Result<()> {
    Cli::parse();
    // Restore the terminal on panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = tui::restore();
        original_hook(panic_info);
    }));

    configure_git_extensions()?;

    // Initialize application
    let mut app = App::new()?;

    // Initialize terminal
    let mut terminal = tui::init()?;

    let mut needs_render = true;
    let mut render_deadline: Option<Instant> = None;

    // Main loop
    loop {
        // Render only when state has changed
        if needs_render {
            terminal.draw(|frame| {
                ui::draw(frame, &mut app);
            })?;
            needs_render = false;
        }

        if app.should_quit {
            break;
        }

        // Determine poll timeout — shorter if a render deadline is approaching
        let timeout = match render_deadline {
            Some(dl) => dl
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(33)),
            None => Duration::from_millis(33),
        };

        let event = poll_event_with_timeout(timeout)?;

        // Check if render deadline has elapsed (message expiry)
        if render_deadline.is_some_and(|dl| Instant::now() >= dl) {
            needs_render = true;
        }

        // Process input
        if let Some(event) = event {
            if let Some(key) = get_key_event(&event) {
                if app.debug_keys {
                    app.set_message(format!(
                        "KEY: code={:?} mod={:?}",
                        key.code, key.modifiers
                    ));
                }
                if let Some(action) = map_key_to_action(
                    key,
                    &app.mode,
                    app.focused_panel,
                    app.editing_commit_message,
                    app.files_pane.files_filter_active,
                    app.commit_filter_active,
                ) {
                    if let Err(e) = app.handle_action(action) {
                        app.show_error(format!("{}", e));
                    }
                }
            } else if let Some(scroll) = get_mouse_scroll(&event) {
                let (action, multiplier) = match &app.mode {
                    AppMode::FileDiff { .. } => {
                        let a = if scroll > 0 {
                            keifu::action::Action::ScrollDown
                        } else {
                            keifu::action::Action::ScrollUp
                        };
                        (a, 3)
                    }
                    _ => {
                        let a = if scroll > 0 {
                            keifu::action::Action::MoveDown
                        } else {
                            keifu::action::Action::MoveUp
                        };
                        (a, 1)
                    }
                };
                for _ in 0..multiplier {
                    if let Err(e) = app.handle_action(action.clone()) {
                        app.show_error(format!("{}", e));
                        break;
                    }
                }
            }
            needs_render = true;
        }

        // Poll background operations after input, so the quick diff for a
        // newly selected commit is computed before the frame that renders it
        needs_render |= app.update_diff_cache();
        needs_render |= app.update_fetch_status();
        needs_render |= app.update_push_status();
        needs_render |= app.update_pull_status();
        needs_render |= app.check_auto_refresh();
        needs_render |= app.poll_fs_watcher();

        // Refresh message expiry deadline
        render_deadline = app.message_expiry_time();
    }

    // Restore terminal
    tui::restore()?;

    Ok(())
}
