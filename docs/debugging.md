# Debugging keifu

keifu ships two debugging facilities aimed at both humans and AI agents:
file-based logging and a remote control server.

## Logging

```bash
keifu --log-file /tmp/keifu.log
```

Appends `tracing` logs to the file. The level filter is read from the
`KEIFU_LOG` environment variable using `RUST_LOG` syntax (default: `debug`).

```bash
KEIFU_LOG=trace keifu --log-file /tmp/keifu.log
```

The log rotates once it exceeds 5 MB (one `.old` generation is kept).

### Measuring performance with the log alone

The log is enough to diagnose slowness — no debug server needed:

```bash
keifu --log-file /tmp/keifu.log
# ...reproduce the slow interaction, then quit with q...
tail -30 /tmp/keifu.log
```

- Operations slower than 10 ms are logged live as `slow operation`
  (`draw`, `draw.dump`, `refresh`, ...).
- On exit, a `perf summary` line per operation reports count/avg/max.

## Remote control server

```bash
keifu --debug-listen 127.0.0.1:7167
```

Listens for newline-delimited JSON commands over TCP. Each request line gets
exactly one JSON response line. Only bind to loopback addresses; the protocol
is unauthenticated.

Injected key/mouse events go through the same mapping as real input
(`map_key_to_action` / `map_mouse_to_action` → `handle_action`), so behavior is
identical to a human at the terminal.

### Commands

| Request | Response |
| --- | --- |
| `{"cmd":"keys","keys":"<down> <down> <enter>"}` | `{"ok":true}` |
| `{"cmd":"mouse","kind":"click","x":5,"y":3}` | `{"ok":true}` |
| `{"cmd":"dump"}` | `{"ok":true,"width":…,"height":…,"screen":"…"}` |
| `{"cmd":"dump","width":100,"height":30}` | same, rendered at the given size |
| `{"cmd":"state"}` | `{"ok":true,"mode":…,"selected_index":…,…}` |

- `keys` — whitespace-separated tokens fed through the normal keybinding
  layer. Single characters are sent as-is (uppercase implies Shift). Special
  keys: `<enter> <esc> <tab> <backtab> <space> <up> <down> <left> <right>
  <home> <end> <pgup> <pgdn> <backspace> <c-x>` (Ctrl+x). Graph navigation uses
  the arrow keys (`<up>`/`<down>`), `G`/`g` for bottom/top, not `j`/`k`; when
  unsure, open the in-app help with `?`.
- `mouse` — `kind` is `click`, `right_click`, `scroll_up`, or `scroll_down`;
  `x`/`y` are screen coordinates (0-based).
- `dump` — renders the current state to plain text. Without `width`/`height`
  the real terminal size is used (falling back to sane bounds when headless).
- `state` — mode, focused panel, selection, HEAD, async operation status:
  `mode`, `focused_panel` (`graph`/`files`/`commit_detail`), `selected_index`,
  `selected_commit` (short id), `selected_branches`, `head`, `node_count`,
  `commit_count`, `editing_commit_message`, `is_fetching`, `is_pushing`,
  `is_pulling`.

For performance questions, use the log instead (see above).

### Example session

```bash
script -qec "keifu --debug-listen 127.0.0.1:7167" /dev/null &
sleep 2
printf '%s\n' '{"cmd":"keys","keys":"<down> <down>"}' | nc -q1 127.0.0.1 7167
printf '%s\n' '{"cmd":"dump","width":100,"height":30}' | nc -q1 127.0.0.1 7167
printf '%s\n' '{"cmd":"keys","keys":"<c-q>"}' | nc -q1 127.0.0.1 7167  # Ctrl+Q always quits
```

`<c-q>` (Ctrl+Q) force-quits from any mode; `<esc>` quits from the graph pane
once nothing is pending to dismiss. Quitting cleanly (not a killed process) is
what flushes the exit-time `perf summary` to the log.

## Pixel-graph debugging (headless PNG rendering)

The debug server cannot exercise graphics-protocol output. To reproduce
pixel-mode bug reports, `examples/raster_debug.rs` renders real graph rows
through the real rasterizer + trace logic into a PNG:

```bash
cargo run --example raster_debug -- <repo> <commit_prefix> <cell_w> <cell_h> out.png [rows]
DUMP_CELLS=1 ... # also dumps CellType rows and crossing dim flags
```

To exercise the full spec → rasterize → encode pipeline under the headless
debug server (where no terminal answers the protocol query), force a protocol:

```bash
KEIFU_FORCE_PIXEL=kitty KEIFU_LOG=debug keifu --debug-listen ... --log-file ...
```

The escapes land on the PTY unrendered, but the pipeline runs for real; each
frame that rasterizes+encodes rows logs `sync_frame rasterized+encoded rows
encoded=N window=M` at debug level — the per-keypress `encoded` count is the
measure of protocol-cache churn (with tracing on it should stay at the handful
of rows whose lit-state the selection move changed).

`examples/gap_scan.rs` scans such a PNG for hairline gaps (short background
runs between strokes) and can crop+magnify a region:

```bash
cargo run --example gap_scan -- out.png 5            # report gaps ≤5px
cargo run --example gap_scan -- out.png crop X Y W H SCALE zoom.png
```
