//! keifu: a TUI tool that shows Git commit graphs

use anyhow::Result;
use clap::Parser;

use keifu::{
    app::{App, AppMode},
    event::{get_key_event, get_mouse_scroll, poll_event},
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

    // Render the first frame immediately
    terminal.draw(|frame| {
        ui::draw(frame, &mut app);
    })?;

    // Main loop
    loop {
        // Exit check
        if app.should_quit {
            break;
        }

        // Wait for input (blocks up to 33ms)
        let event = poll_event()?;

        // Check background operations (cheap — just polls mpsc channels)
        app.update_fetch_status();
        app.update_push_status();
        app.check_auto_refresh();
        app.poll_fs_watcher();

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
        }

        // Render after processing
        terminal.draw(|frame| {
            ui::draw(frame, &mut app);
        })?;
    }

    // Restore terminal
    tui::restore()?;

    Ok(())
}
