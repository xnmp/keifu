---
name: debug-tui
description: Drive and debug the real keifu TUI autonomously via its remote-control debug server (--debug-listen) — launch headlessly, inject keys/mouse, dump the rendered screen as text, and inspect app state. Use this whenever a change affects TUI behavior, rendering, keybindings, mouse handling, focus, scrolling, or async loading states, when reproducing a user-reported UI bug, or when you need to confirm "does it actually work on screen" — cargo test alone cannot verify what the user sees. Reproduce the issue through this workflow before fixing, and re-verify after.
---

# Debugging the keifu TUI headlessly

keifu has a built-in remote-control server. You can run the real app without a
human at the terminal: send key/mouse input through the same code paths as real
input, dump the rendered screen as plain text, and read the app state as JSON.
The reliable workflow is always: **drive → dump → assert**, both to reproduce a
bug and to prove the fix.

## Launch

```bash
cargo build
PORT=7167   # pick a fresh port per run to avoid stale instances
timeout 120 script -qec "./target/debug/keifu --debug-listen 127.0.0.1:$PORT --log-file /tmp/keifu.log" /dev/null >/dev/null 2>&1 &
sleep 2
```

- `script` allocates a PTY; keifu cannot enable raw mode without one. The
  `timeout` wrapper guarantees stray instances die even if you forget to quit.
- The `script` PTY reports size 0x0, so the main loop skips real rendering.
  Consequence: pane layout for mouse hit-testing is only recorded when a render
  happens — **always send a `dump` with explicit width/height before any
  `mouse` command**, and give mouse coordinates in that dump's space.
- keifu operates on the repository of its working directory. To exercise
  staging/commit/push, launch it inside a throwaway repo (`mktemp -d` +
  `git init`), never the real working repo.

## Protocol

Newline-delimited JSON over TCP; each request line gets one JSON response line.

```bash
printf '%s\n' '{"cmd":"state"}' | nc -q1 127.0.0.1 $PORT
```

| Request | Effect |
| --- | --- |
| `{"cmd":"keys","keys":"<down> <down> <enter>"}` | Inject key input (normal keybinding layer) |
| `{"cmd":"mouse","kind":"click","x":5,"y":3}` | `click` / `right_click` / `scroll_up` / `scroll_down` at 0-based cell |
| `{"cmd":"dump","width":110,"height":30}` | Render current state to plain text at that size |
| `{"cmd":"state"}` | Mode, focused panel, selection, HEAD, async status |

`state` fields: `mode`, `focused_panel` (`graph`/`files`/`commit_detail`),
`selected_index`, `selected_commit` (short id), `selected_branches`, `head`,
`node_count`, `commit_count`, `editing_commit_message`, `is_fetching`,
`is_pushing`, `is_pulling`.

For "feels slow" reports, use the log: ops over 10ms are written live as
`slow operation`, and quitting writes a per-op `perf summary` (count/avg/max).
Reproduce → quit → grep the log file.

Every response is one JSON line. `dump` returns the screen as an escaped
string in the `screen` field — pipe through `jq -r .screen` to read it. For
single requests prefer `nc -q1` (closes after the response); only multi-line
batches need plain `nc` under `timeout`.

Key token syntax: whitespace-separated; single chars as-is (uppercase implies
Shift); special keys `<enter> <esc> <tab> <backtab> <space> <up> <down> <left>
<right> <home> <end> <pgup> <pgdn> <backspace> <c-x>` (Ctrl+x). To type a word
in an input dialog, space-separate the letters: `c f i x <space> b u g <enter>`.

**Graph navigation uses the arrow keys, not `j`/`k`.** Move with `<up>`/`<down>`,
`G`/`g` for bottom/top; `<tab>` cycles pane focus, `?` opens help, `<esc>` quits
from the graph pane, `<c-q>` force-quits from anywhere. When unsure of a
binding, open `?` and read it off the dump.

Full protocol details: `docs/debugging.md`. Implementation: `src/debug_server.rs`.

## Gotchas that will waste your time

- **Double-click** = two clicks on the same cell within the double-click
  window. Separate `nc` invocations are too slow — send both clicks (plus the
  leading `dump` that records the layout) in ONE connection:

  ```bash
  printf '%s\n%s\n%s\n%s\n' \
    '{"cmd":"dump","width":110,"height":30}' \
    '{"cmd":"mouse","kind":"click","x":60,"y":24}' \
    '{"cmd":"mouse","kind":"click","x":60,"y":24}' \
    '{"cmd":"state"}' | timeout 4 nc 127.0.0.1 $PORT
  ```

- Commands are processed after the event-poll tick, so responses can lag up to
  ~200 ms; wrap `nc` in `timeout` and don't interpret slowness as a hang.
- A held-open `nc` may exit non-zero via `timeout` even after delivering the
  response — check the output, not the exit code.
- **Injected input bypasses the terminal's input layer.** `keys`/`mouse`
  commands go straight into the app, so they cannot verify anything that
  depends on terminal modes (mouse tracking escapes, the graphics protocol for
  pixel-rendered graph lines). Changes to `src/tui.rs` need a human in a real
  terminal.
- **Quitting.** `<c-q>` (Ctrl+Q) is `ForceQuit` — it always exits, from any
  mode. `<esc>` quits from the graph pane, but only once nothing is pending: it
  first closes a popup, returns pane focus, or clears a pending compare mark.
  For a guaranteed clean exit send `{"cmd":"keys","keys":"<c-q>"}` and confirm:
  a follow-up `nc` connection must be refused. A clean exit is also what flushes
  the `perf summary` to the log — a killed process (`timeout` SIGTERM) writes
  none.

## Verification loop

1. `dump` and confirm the precondition is on screen (e.g. the row you'll click).
2. `keys` / `mouse` to act.
3. `state` + `dump`, then assert: grep the dump for expected text, compare
   `selected_index` / `mode` / `focused_panel` in the state JSON.
4. Quit, and read `/tmp/keifu.log` for the tracing trail
   (`KEIFU_LOG=trace` for more detail; useful for async diff-load issues).

A fix is not verified until step 3 shows the corrected behavior on a dump that
previously showed the bug.
