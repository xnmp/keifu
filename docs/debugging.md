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
  <home> <end> <pgup> <pgdn> <backspace> <c-x>` (Ctrl+x), `<a-x>` (Alt+x), and
  `<c-a-x>` (Ctrl+Alt+x), and `<c-s-x>` (Ctrl+Shift+x). Graph navigation uses
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
printf '%s\n' '{"cmd":"keys","keys":"<down> <down>"}' | nc -N 127.0.0.1 7167
printf '%s\n' '{"cmd":"dump","width":100,"height":30}' | nc -N 127.0.0.1 7167
printf '%s\n' '{"cmd":"keys","keys":"<c-q>"}' | nc -N 127.0.0.1 7167  # Ctrl+Q always quits
```

`<c-q>` (Ctrl+Q) force-quits from any mode; `<esc>` quits from the graph pane
once nothing is pending to dismiss. Quitting cleanly (not a killed process) is
what flushes the exit-time `perf summary` to the log.

### Verifying a graph action in the live app

For a graph-panel keybinding that starts an asynchronous operation, query the
state before and immediately after injecting the key. The focused panel and
selection should remain unchanged, while the operation-specific state flag
should become active. Use a clean local fixture with a configured upstream so
the completion result is deterministic. This verifies that lowercase `l`
routes to Pull rather than graph navigation:

```bash
set -euo pipefail

cargo build
keifu_bin="$PWD/target/debug/keifu"
test -x "$keifu_bin"

tmp_dir=$(mktemp -d /tmp/keifu-debug.XXXXXX)
git init -q -b main "$tmp_dir/seed"
git -C "$tmp_dir/seed" config user.name Harness
git -C "$tmp_dir/seed" config user.email harness@example.com
git -C "$tmp_dir/seed" commit --allow-empty -qm initial
git clone -q --bare "$tmp_dir/seed" "$tmp_dir/remote.git"
git clone -q "$tmp_dir/remote.git" "$tmp_dir/clone"

(cd "$tmp_dir/clone" && nohup script -qec \
  "$keifu_bin --debug-listen 127.0.0.1:7169" /tmp/keifu-debug.session \
  >/tmp/keifu-debug.pty 2>&1 &)
for _ in $(seq 30); do
  nc -z 127.0.0.1 7169 2>/dev/null && break
  sleep 1
done
nc -z 127.0.0.1 7169

before=$(printf '%s\n' '{"cmd":"state"}' | nc -N 127.0.0.1 7169)
immediate=$(printf '%s\n' '{"cmd":"keys","keys":"l"}' '{"cmd":"state"}' \
  | nc -N 127.0.0.1 7169 | tail -n 1)
before_index=$(jq -r '.selected_index' <<<"$before")
after_index=$(jq -r '.selected_index' <<<"$immediate")
jq -e '(.mode == "normal" and .focused_panel == "graph")' <<<"$before" >/dev/null
jq -e '(.mode == "normal" and .focused_panel == "graph")' <<<"$immediate" >/dev/null
test "$before_index" = "$after_index"
# A very fast local pull may already be complete in the immediate response;
# the final state and rendered result below are the completion assertion.
immediate_started=$(jq -r '.is_pulling' <<<"$immediate")

for _ in $(seq 30); do
  settled=$(printf '%s\n' '{"cmd":"state"}' | nc -N 127.0.0.1 7169)
  jq -e '.is_pulling == false' <<<"$settled" >/dev/null && break
  sleep 1
done
jq -e '.is_pulling == false' <<<"$settled" >/dev/null
screen=$(printf '%s\n' '{"cmd":"dump","width":120,"height":35}' \
  | nc -N 127.0.0.1 7169 | jq -r '.screen')
grep -Fq 'Pulled' <<<"$screen"
printf '%s\n' '{"cmd":"keys","keys":"<c-q>"}' \
  | nc -N 127.0.0.1 7169 >/dev/null
rm -rf "$tmp_dir"
```

The assertions prove startup readiness, Normal + Graph focus before and after
the key, unchanged selection, completion (`is_pulling == false`), and the
observable successful Pull result (`Pulled` in the rendered screen). The
immediate response also records whether the async operation was still active
(`is_pulling == true`) or completed within the request round-trip.

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
