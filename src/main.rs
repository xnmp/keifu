//! keifu: a TUI tool that shows Git commit graphs

use anyhow::Result;
use clap::Parser;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use keifu::{
    app::App,
    debug_server,
    event::{get_key_event, get_mouse_event, poll_event_with_timeout},
    git::configure_git_extensions,
    keybindings::{map_key_to_action, map_mouse_to_action},
    logging, tui, ui,
};

#[derive(Parser)]
#[command(name = "keifu")]
#[command(
    version,
    about = "A TUI tool to visualize Git commit graphs with branch genealogy"
)]
struct Cli {
    /// Append debug logs and a perf summary on exit to this file
    /// (level via KEIFU_LOG, default "debug")
    #[arg(long, value_name = "PATH")]
    log_file: Option<PathBuf>,

    /// Listen for debug commands (NDJSON over TCP, e.g. 127.0.0.1:7167)
    #[arg(long, value_name = "ADDR")]
    debug_listen: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(path) = &cli.log_file {
        logging::init(path)?;
    }
    let debug_rx = match &cli.debug_listen {
        Some(addr) => Some(debug_server::spawn(addr)?),
        None => None,
    };

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

    // Detect a terminal graphics protocol for pixel-rendered graph lines. This
    // must run after raw mode is enabled (above) and before the event loop
    // starts polling, so crossterm's reader doesn't swallow the query reply.
    if app.config.ui.graph_renderer != keifu::config::GraphRenderer::Unicode {
        app.pixel_graph = keifu::ui::graph_pixels::PixelGraphState::new();
    }

    let mut needs_render = true;
    let mut render_deadline: Option<Instant> = None;

    // Main loop
    loop {
        // Render only when state has changed
        if needs_render {
            let draw_started = Instant::now();
            terminal.draw(|frame| {
                ui::draw(frame, &mut app);
            })?;
            app.perf.record("draw", draw_started.elapsed());
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
            } else if let Some(mouse) = get_mouse_event(&event) {
                if let Some(action) = map_mouse_to_action(mouse) {
                    if let Err(e) = app.handle_action(action) {
                        app.show_error(format!("{}", e));
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
        needs_render |= app.update_open_prs();
        needs_render |= app.update_check_status();
        needs_render |= app.update_thread_status();
        needs_render |= app.update_pr_action_status();
        needs_render |= app.update_avatars();
        needs_render |= app.maybe_autoload_commits();

        // Process pending debug commands (only when --debug-listen is active).
        // Injected keys/mouse go through the same mapping as real input, so a
        // mutating command must trigger a redraw of the live terminal.
        if let Some(rx) = &debug_rx {
            while let Ok(command) = rx.try_recv() {
                let size = terminal.size()?;
                let mutates = matches!(
                    command.request,
                    debug_server::DebugRequest::Keys { .. }
                        | debug_server::DebugRequest::Mouse { .. }
                );
                let response = debug_server::handle_request(
                    &mut app,
                    size.width,
                    size.height,
                    command.request,
                );
                let _ = command.reply.send(response);
                needs_render |= mutates;
                if app.should_quit {
                    break;
                }
            }
        }

        // Drop expired toasts (redraw only when the visible set actually changes,
        // so an active-but-unexpired toast doesn't force per-frame repaints).
        needs_render |= app.toasts.evict(Instant::now());

        // Wake precisely at the next message/toast expiry (the poll timeout is
        // still capped at 33ms, so the idle loop never busy-spins).
        render_deadline = app.next_render_deadline();
    }

    // Log an aggregate perf summary (only visible with --log-file)
    app.perf.log_summary();

    // Restore terminal
    tui::restore()?;

    Ok(())
}
