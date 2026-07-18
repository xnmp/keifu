# 🧬 keifu

[![Crate Status](https://img.shields.io/crates/v/keifu.svg)](https://crates.io/crates/keifu)
[![Built With Ratatui](https://img.shields.io/badge/Built_With-Ratatui-000?logo=ratatui&logoColor=fff&labelColor=000&color=fff)](https://ratatui.rs)

[日本語版はこちら](docs/README_JA.md)

keifu (系譜, /keːɸɯ/) is a terminal UI tool that visualizes Git commit graphs. It shows a colored commit graph, commit details, and a summary of changed files, and lets you perform basic branch operations.

![Screenshot](docs/win_terminal.png)

## Motivation

- **Readable commit graph** — `git log --graph` is hard to read; keifu renders a cleaner, color-coded graph
- **Fast branch switching** — With the rise of vibe coding, working on multiple branches in parallel has become common. keifu makes branch switching quick and visual
- **Keep it simple** — Only basic Git operations are supported; this is not a full-featured Git client
- **Narrow terminal friendly** — Works well in split panes and small windows
- **No image protocol required** — Works on any terminal with Unicode support

## Features

- Unicode commit graph with per-branch colors; tags render as refs next to branch labels
- Optional pixel-rendered graph lines (continuous VSCode-style curves) on terminals with a graphics protocol (Kitty/iTerm2); falls back to Unicode automatically (`ui.graph_renderer`)
- Commit list with branch/tag labels, relative date, author, short hash, and message (some fields may be hidden on narrow terminals)
- Commit detail panel with full message, changed file stats (+/-), and GPG signature status
- File diff view with syntax highlighting, word-level change emphasis, and hunk-level stage/unstage/discard
- Files pane: stage/unstage (file, folder, or all), gitignore, archive to `.archive/`, trash untracked files, undo, folder grouping, fuzzy filter, copy path, per-file history
- Merge-conflict handling: accept ours/theirs, continue/abort a merge, rebase, cherry-pick, or revert
- Git operations: checkout, create/rename/delete branch, merge, rebase, cherry-pick, revert, reset (soft/mixed/hard), tag add/delete/push, stash (apply/pop/drop, staged/all/all+untracked push, branch-from-stash)
- Fetch/pull/push with multi-remote support, upstream tracking, and one-key publish
- Open-PR badges: commits whose branch has an open GitHub PR show a `#N` badge; `o` opens it in the browser (requires the `gh` CLI)
- Real branch filtering — hiding a branch removes its exclusive commits from the graph, not just its label
- Compare any two commits
- Branch search with fuzzy dropdown UI; commit filter by message/author/hash

## Requirements

- Run inside a Git repository (auto-discovery from current directory)
- A terminal with Unicode line drawing support and color
- `git` command in PATH — required for fetch/pull/push, hunk staging, stash, and most other mutating operations
- Rust toolchain (for building from source)

## Installation

### From crates.io

```bash
cargo install keifu
```

### With mise

```bash
mise use -g github:trasta298/keifu@latest
```

### With Homebrew

```bash
brew install trasta298/tap/keifu
```

### From source

```bash
git clone https://github.com/trasta298/keifu && cd keifu && cargo install --path .
```

## Usage

Run inside a Git repository:

```bash
keifu
```

## Configuration

See [docs/configuration.md](docs/configuration.md) for configuration options.

## Keybindings

Panels: **Graph** → **Files** → **Commit Detail**, cycled with `←`/`→` or `Tab`/`Shift+Tab`. The status bar always shows keys valid in the current context; `?` opens the full in-app help popup.

### Navigation (all panels)

| Key | Action |
| --- | --- |
| `↑` / `↓` | Move up/down |
| `←` / `→` / `Tab` / `Shift+Tab` | Switch panels |
| `Ctrl+d` / `PageDown` | Page down |
| `Ctrl+u` / `PageUp` | Page up |
| `g` / `Home` | Go to top |
| `G` / `End` | Go to bottom |
| `@` | Jump to HEAD |
| `Esc` | Back to graph / stop editing / quit (from the graph panel) |

### Graph panel

| Key | Action |
| --- | --- |
| `Enter` | Open the commit actions menu (see below) |
| `Space` | Open file diff for the selected commit |
| `]` / `[` | Jump to next/previous commit with a branch label |
| `b` | Create branch at selected commit |
| `d` | Delete branch (local or remote, behind confirm) |
| `f` | Fetch (resolves the remote from upstream; prompts if ambiguous) |
| `p` | Pull (fetch + integrate; honors `pull.rebase`) |
| `Shift+P` | Push current branch (publishes with `-u` if it has no upstream) |
| `Shift+B` | Branch filter — choose which branches' commits are shown |
| `Ctrl+f` | Filter commits by message/author/hash |
| `m` | Mark a commit, then mark a second to compare them (`Esc` clears) |
| `o` | Open the selected commit's PR in the browser (needs `gh`; badge shown on commits with an open PR) |
| `Shift+M` | Toggle which metadata columns (author/hash/date) show on commit rows (persists across restarts) |

The **commit actions menu** (`Enter`, fuzzy-filterable by typing) offers, depending on context: checkout, create/rename/delete branch, merge into current, rebase current onto this, cherry-pick, revert, reset (soft/mixed/hard), add/delete/push tag, push, pull, prune remote-tracking refs, copy hash/message, mark/compare, and — on the uncommitted or a stash node — stash apply/pop/drop and branch-from-stash.

### Files panel

| Key | Action |
| --- | --- |
| `s` | Stage/unstage selected file (or folder, in folder mode) |
| `Shift+S` / `Shift+U` | Stage all / unstage all |
| `i` | Add to `.gitignore` |
| `v` | Archive to `.archive/` |
| `r` | Restore file (discard changes) |
| `Delete` | Trash untracked file (recycle bin) |
| `Ctrl+z` | Undo last file operation |
| `f` | Toggle folder grouping |
| `Ctrl+f` | Filter files |
| `Space` | Open with default app |
| `y` | Copy file's repo-relative path |
| `Enter` | Open file diff |
| `h` | File history (commits touching this file) |
| `o` / `t` | Accept ours / theirs (on a conflicted file) |
| `c` / `Shift+A` | Continue / abort the in-progress merge/rebase/cherry-pick/revert |

### File diff viewer

| Key | Action |
| --- | --- |
| `j` / `k` / `↑` / `↓` | Scroll up/down |
| `h` / `l` / `←` / `→` | Scroll left/right |
| `Ctrl+d` / `Ctrl+u` | Half-page down/up |
| `Ctrl+f` / `Ctrl+b` | Full page down/up |
| `g` / `G` | Go to top/bottom |
| `0` | Scroll to line start |
| `]` / `[` | Jump to next/previous hunk |
| `n` / `N` | Jump to next/previous file |
| `s` / `u` / `x` | Stage / unstage / discard hunk under cursor (uncommitted changes only) |
| `Esc` / `q` | Back to file select / close |

### Commit panel

| Key | Action |
| --- | --- |
| `Enter` | Start editing commit message, then commit (or save amend) |
| `Ctrl+Enter` | Amend last commit |
| `Ctrl+s` | Stash (staged / all / all + untracked, with optional message) |
| `Esc` | Stop editing |

### Search

| Key | Action |
| --- | --- |
| `/` | Search branches (incremental fuzzy search) |
| `↑` / `Ctrl+k` | Select previous result |
| `↓` / `Ctrl+j` | Select next result |
| `Enter` | Jump to selected branch |
| `Esc` / `Backspace` on empty | Cancel search |

### Other

| Key | Action |
| --- | --- |
| `R` | Refresh repository data |
| `F5` | Full update — fetch all remotes, refetch open PRs, and refresh |
| `?` | Toggle help |
| `Ctrl+Q` | Quit from anywhere |

## Notes and limitations

- The TUI loads up to 500 commits, walked from the currently visible branch tips (HEAD is always included). Hiding branches in the branch filter shrinks this set rather than just hiding labels.
- Merge commits are diffed against the first parent; the initial commit is diffed against an empty tree.
- Changed files are capped at 50. Binary files are shown without line stats.
- If there are staged, unstaged, or untracked changes, an "uncommitted changes" row appears at the top.
- When multiple branches point to the same commit, the label is collapsed to a single name with a `+N` suffix (e.g., `main +2`).
- Checking out `origin/xxx` creates or updates a local branch. Upstream is set only when creating a new branch. If the local branch exists but points to a different commit, it is force-updated to match the remote.
- Remote branches can be deleted directly (`git push <remote> --delete`), behind a confirmation.
- Fetch/pull/push resolve the remote from the branch's upstream, prompting only when several remotes exist and none can be inferred.
- Hunk-level staging works on uncommitted changes only; a full 3-way merge editor is not implemented — conflicts are resolved via accept-ours/accept-theirs or your own editor.

## License

MIT
