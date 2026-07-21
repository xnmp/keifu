//! keifu: a TUI tool that shows Git commit graphs

use anyhow::Result;
use clap::Parser;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use keifu::{
    app::App,
    debug_server,
    event::{get_key_event, get_mouse_event, get_paste_event, poll_event_with_timeout},
    external_edit::{self, ExternalEditTarget},
    git::configure_git_extensions,
    keybindings::{map_key_to_action, map_mouse_to_action},
    logging,
    toast::ToastKind,
    tui, ui,
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

/// Pop the current compose buffer out into the user's `$EDITOR`. main.rs is the
/// sole owner of the terminal, so it fully suspends the TUI, runs the editor
/// (which inherits stdio), then restores the terminal exactly as at startup and
/// forces a full repaint. On spawn failure or a non-zero editor exit the
/// original text is kept and the error is surfaced as a toast.
fn run_external_edit(
    terminal: &mut tui::Tui,
    app: &mut App,
    target: ExternalEditTarget,
) -> Result<()> {
    let source = app.external_edit_source_text(target);
    // Suspend the TUI before handing the screen to the child editor.
    tui::restore()?;
    let outcome = external_edit::edit_text(&source);
    // Restore the terminal to the startup state and force a full redraw over
    // whatever the editor left on screen.
    tui::resume()?;
    terminal.clear()?;
    match outcome {
        Ok(edited) => app.apply_external_edit(target, edited),
        Err(e) => app.toast(ToastKind::Error, format!("Editor: {e}")),
    }
    Ok(())
}

/// Cap on extra buffered events processed before a draw, so a pathological
/// input flood cannot starve rendering entirely.
const MAX_COALESCED_EVENTS: usize = 64;

/// Route one input event through the same key/mouse/paste mapping the loop has
/// always used. Returns whether the caller may keep draining buffered events:
/// `false` after quit is requested (draw/exit promptly) or after an external
/// editor ran (it consumed the terminal, so buffered input is stale).
fn handle_input_event(
    terminal: &mut tui::Tui,
    app: &mut App,
    event: crossterm::event::Event,
) -> Result<bool> {
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
            // External-editor pop-out: the compose handler set a pending
            // request; main.rs owns the terminal, so it runs here (real
            // input path only — never for debug-injected keys).
            if let Some(target) = app.pending_external_edit.take() {
                run_external_edit(terminal, app, target)?;
                return Ok(false);
            }
        }
    } else if let Some(mouse) = get_mouse_event(&event) {
        if let Some(action) = map_mouse_to_action(mouse) {
            if let Err(e) = app.handle_action(action) {
                app.show_error(format!("{}", e));
            }
        }
    } else if let Some(text) = get_paste_event(&event) {
        // Bracketed paste: routed by mode (credential prompt, other
        // inputs, or a compose editor) inside the App.
        if let Err(e) = app.handle_paste(text) {
            app.show_error(format!("{}", e));
        }
    }
    Ok(!app.should_quit)
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
            let had_pixel_graph = app.pixel_graph.is_some();
            terminal.draw(|frame| {
                ui::draw(frame, &mut app);
            })?;
            app.perf.record("draw", draw_started.elapsed());
            // Pixel rendering poisoned itself during this draw (protocol
            // failures): the frame on screen has no graph column. Redraw
            // immediately so the Unicode fallback appears without waiting
            // for the next input event.
            needs_render = had_pixel_graph && app.pixel_graph.is_none();
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

        // Process input. After the first event, drain whatever else is already
        // buffered (capped) before drawing, so a held key or fast scroll wheel
        // becomes one frame per batch instead of one full frame per event —
        // without this the input queue outruns the draw rate and the graph
        // keeps scrolling after the finger stops.
        if let Some(event) = event {
            let mut keep_draining = handle_input_event(&mut terminal, &mut app, event)?;
            let mut drained = 0;
            while keep_draining && drained < MAX_COALESCED_EVENTS {
                match poll_event_with_timeout(Duration::ZERO)? {
                    Some(ev) => {
                        keep_draining = handle_input_event(&mut terminal, &mut app, ev)?;
                        drained += 1;
                    }
                    None => break,
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
        needs_render |= app.update_merged_prs();
        needs_render |= app.update_merged_classification();
        needs_render |= app.update_check_status();
        needs_render |= app.update_thread_status();
        needs_render |= app.update_pr_action_status();
        needs_render |= app.update_issue_status();
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
                        | debug_server::DebugRequest::Paste { .. }
                );
                let response = debug_server::handle_request(
                    &mut app,
                    size.width,
                    size.height,
                    command.request,
                );
                let _ = command.reply.send(response);
                // An external editor can't run headlessly (no interactive tty
                // for the child), so a debug-injected Ctrl+E must not suspend the
                // terminal. Drop any request the injected key produced.
                app.pending_external_edit = None;
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
