# Configuration

keifu can be configured via `~/.config/keifu/config.toml`. All settings are optional.

## Auto-refresh

By default, keifu automatically refreshes the commit graph every 10 seconds and fetches from origin every 60 seconds.

```toml
[refresh]
# Enable auto-refresh for local state (default: true)
auto_refresh = true

# Interval in seconds for local refresh (default: 10, minimum: 1)
refresh_interval = 10

# Enable auto-fetch from origin (default: true)
auto_fetch = true

# Interval in seconds for remote fetch (default: 60, minimum: 10)
fetch_interval = 60
```

### Options

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `auto_refresh` | bool | `true` | Enable auto-refresh for local state (commits, branches, working tree) |
| `refresh_interval` | integer | `10` | Interval in seconds for local refresh (minimum: 1) |
| `auto_fetch` | bool | `true` | Enable auto-fetch from origin |
| `fetch_interval` | integer | `60` | Interval in seconds for remote fetch (minimum: 10) |

### Disabling auto-refresh

To disable automatic updates entirely:

```toml
[refresh]
auto_refresh = false
auto_fetch = false
```

You can still manually refresh with `R` and fetch with `f`.

## UI

```toml
[ui]
# Theme: "auto" (detect from terminal background), "dark", or "light"
theme = "auto"

# Commit graph line rendering:
#   "auto"    — pixel rendering when the terminal supports a graphics protocol,
#               otherwise Unicode box-drawing glyphs (default)
#   "unicode" — always use Unicode glyphs
#   "pixel"   — force pixel rendering (falls back to Unicode if unsupported)
graph_renderer = "auto"
```

### Options

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `theme` | string | `"auto"` | `"auto"`, `"dark"`, or `"light"` |
| `graph_renderer` | string | `"auto"` | `"auto"`, `"unicode"`, or `"pixel"` |

Pixel rendering draws the graph lines as transparent images via the terminal's
image protocol (detected once at startup). It requires a graphics-capable
terminal such as WezTerm, Kitty, or iTerm2; on any other terminal keifu
silently uses the Unicode renderer.

## Keyboard shortcuts

Add a `[keymap]` table to replace shortcuts for individual actions. Action
names are stable, kebab-case identifiers shared with Keifu's command registry.
An action that is not listed keeps all of its current defaults.

You can also edit these bindings in Keifu: open Settings with `Ctrl+,`, then
press `Ctrl+K`. Select an action, press Enter to capture its replacement, use
`u` to explicitly unassign it, and press `Ctrl+S` to save. The editor previews
the effective binding and conflict warning before it writes `config.toml`; Esc
cancels capture or discards unsaved edits.

```toml
[keymap]
# Replace Pull's default `l` binding.
pull = ["Ctrl+Alt+P"]

# Every listed alternative triggers the action.
open-command-palette = ["Ctrl+P", "Ctrl+Alt+P", "F2"]

# An empty list deliberately leaves an action without a shortcut.
toggle-debug-keys = []
```

Common identifiers include `fetch`, `pull`, `push`, `refresh`,
`open-commit-menu`, `open-command-palette`, `open-settings`, `toggle-help`,
`move-up`, `move-down`, `menu-select`, `confirm`, `cancel`, `toggle-stage`,
`stage-all`, `open-file-diff`, `start-editing`, `commit-changes`, and the
descriptive kebab-case names shown by the command palette. Navigation and
modal actions use one identifier in every context where that action is
available. The compatibility aliases `command-palette` and `open-palette`
currently resolve to `open-command-palette`.

Bindings contain one base key and any of `Ctrl`, `Alt`, and `Shift`, joined by
`+`. Modifiers can be combined. Named keys include `Enter`, `Esc`, `Tab`,
`BackTab`, `Backspace`, `Delete`, `Insert`, the four arrows, `Home`, `End`,
`PageUp`, `PageDown`, `Space`, and `F1` through `F24`.

```toml
[keymap]
full-update = ["Ctrl+Shift+F5"]
open-issue-list = ["Alt+Shift+I"]
menu-select = ["Enter", "F12"]
```

Multi-key sequences such as `Ctrl+K Ctrl+C` are not supported. If an action
name or binding is malformed, Keifu keeps that action's defaults, applies the
other valid entries, and reports the rejected entry in a startup error toast
and the log.

The same shortcut can be reused in mutually exclusive contexts, such as one
graph-panel action and one files-panel action. When two actions that can be
active together resolve to the same shortcut, the later entry in the TOML
table wins. Keifu reports both action names and the winning binding at startup
instead of silently shadowing the earlier action. The Help popup and command
palette always show the effective startup configuration, including multiple
alternatives and `Unassigned` actions.
