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
