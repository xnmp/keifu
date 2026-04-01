# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

keifu (系譜) is a Rust TUI for git graph visualization, aiming for a VSCode-like Git Graph + Source Control experience in the terminal. Built with Ratatui + Crossterm for rendering, git2 (libgit2) for git operations.

## Build & Development Commands

```bash
cargo build              # Build
cargo check              # Fast type-check (use during development)
cargo test               # Run all tests
cargo test <test_name>   # Run a single test
cargo clippy             # Lint
cargo run                # Run against current directory's git repo
cargo run -- /path/to   # Run against a specific repo
```

## Workflow

When implementing features or fixes:
1. Create an entry in `docs/TODO.md`
2. Create a new git branch
3. Write unit tests where appropriate
4. Implement the feature
5. Ensure `cargo test` and `cargo clippy` pass
6. Merge the branch, mark the issue in `docs/TODO.md` as done
7. Document architectural decisions or gotchas in `docs/`

Use beads issue tracker — run `bd quickstart` for details.

## Architecture

### Data Flow

Event loop in `main.rs`: render frame → poll input → `map_key_to_action()` → `app.handle_action()` → update state → next frame.

### Key Modules

- **`app.rs`** — Central state machine. All application state lives on the `App` struct. Handles actions dispatched by the keybinding layer. This is the largest file and the integration point for everything.
- **`git/`** — Git abstraction layer. `repository.rs` wraps git2::Repository. `diff.rs` computes diffs (staged/unstaged/committed). `operations.rs` has all mutating git commands. `graph.rs` builds the visual graph layout from commits.
- **`ui/`** — Stateless rendering widgets. Each widget receives `&App` or relevant data and renders to a Ratatui frame. `mod.rs` has the top-level `draw()` function that composes the layout.
- **`keybindings.rs`** — Maps key events to `Action` variants. Routes based on both `AppMode` and `FocusedPanel`. This is the input layer — no business logic here.
- **`action.rs`** — Enum of all user actions. Decouples input mapping from action handling.
- **`text_editor.rs`** — Multi-line text editor with cursor, selection, word navigation. Used for commit message editing.
- **`config.rs`** — TOML config from `~/.config/keifu/config.toml`. Controls auto-refresh/fetch intervals.

### Key Design Decisions (see `docs/architecture.md` for full context)

- **Panel focus is orthogonal to mode.** `FocusedPanel` (Graph/Files/CommitDetail) is a field on `App`, not a mode variant. Modes (`Normal`, `Help`, `Input`, `Confirm`, `FileDiff`, etc.) overlay on top. The keybinding router checks both.
- **Two-tier diff caching.** Quick cache (synchronous, file names from `diff.deltas()`) shows instantly. Full cache (async, line stats) loads with 120ms debounce. `cached_diff_or_quick()` returns the best available.
- **`refresh_after_file_op()`** — After file operations (stage/gitignore/archive/trash), uses `invalidate_uncommitted_diff_cache()` (keeps stale data visible) + immediate quick-diff recomputation to avoid UI flash.
- **TextEditor lives on `App.commit_editor`**, not in a mode variant, so commit messages survive panel focus changes.
- **Clipboard uses shell commands** (`xclip`/`xsel`/`wl-copy`/`pbcopy`) to avoid openssl-sys build issues.
- **git2 ignore cache** — `repo.clear_ignore_rules()` is called before status queries so `.gitignore` edits take effect without restarting.

### AppMode variants

`Normal` | `Help` | `Input` (create branch, tag, search) | `Confirm` (destructive ops) | `Error` | `CommitMenu` | `BranchFilter` | `FileDiff` (full diff viewer with syntax highlighting)

### File operations on uncommitted changes

`s` stage/unstage | `r` restore (discard changes) | `i` gitignore | `v` archive to `.archive/` | `Delete` trash untracked (recycle bin) | `Ctrl+Z` undo last op | `Space` open file | `f` toggle folder grouping | `Ctrl+F` filter

Fix root causes, not symptoms. Avoid band-aids like stopPropagation, setTimeout, or flags to mask bugs.

