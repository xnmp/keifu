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

Track work in GitHub Issues — `gh issue list` / `gh issue create`. (`docs/TODO.md` is a historical log; do not add new entries to it.)

When implementing features or fixes:
1. Create (or reference) a GitHub issue
2. Create a new git branch — never commit directly to `chong-dev`
3. Write unit tests where appropriate
4. Implement the feature
5. Ensure `cargo test` and `cargo clippy` pass
6. Push and open a PR against `chong-dev` (`gh pr create`, body `Closes #N`); land it with a squash merge
7. Document architectural decisions or gotchas in `docs/`

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
- **Two-tier diff caching.** Quick cache (synchronous, file names from `diff.deltas()`) shows instantly. Full cache (async, line stats) loads with 120ms debounce. `cached_diff_or_quick()` returns the best available. Gotcha: uncommitted diff-load errors latch (`uncommitted_diff_error_reported`) and won't re-report until an episode boundary (success, or `clear_uncommitted()`) — don't expect every failed poll to surface a fresh message.
- **`refresh_after_file_op()`** — After file operations (stage/gitignore/archive/trash), uses `invalidate_uncommitted_diff_cache()` (keeps stale data visible) + immediate quick-diff recomputation to avoid UI flash.
- **TextEditor lives on `App.commit_editor`**, not in a mode variant, so commit messages survive panel focus changes.
- **Clipboard uses shell commands** (`xclip`/`xsel`/`wl-copy`/`pbcopy`) to avoid openssl-sys build issues, falling back to an OSC 52 escape sequence (`tui::copy_to_clipboard_osc52`) when no shell tool is found.
- **git2 ignore cache** — `repo.clear_ignore_rules()` is called before status queries so `.gitignore` edits take effect without restarting.
- **Settings persistence via a pure registry.** `src/settings.rs` has no `App` dependency (descriptors + get/set accessors); `App::settings_model()`/`apply_settings_model()` project to/from `App` state. State-only settings save through `UiState::save()` to `state.toml`; config-file settings save through `toml_edit`, rewriting only touched keys so comments and unknown keys survive.
- **Toasts for one-shot events, status bar for persistent state.** `ToastQueue` (`src/toast.rs`) handles transient outcomes (Info/Success/Error, TTL-based, capped at 3 visible). The status bar stays reserved for sticky state: in-flight network progress, conflict guidance, and latched periodic errors — a background check that fails repeatedly reports once per failure episode (set on first failure, cleared on success), not on every poll.

### AppMode variants

`Normal` | `Help` | `Input` (create branch, tag, search) | `Confirm` (destructive ops) | `CommitMenu` | `BranchFilter` | `FileDiff` (full diff viewer with syntax highlighting)

There is no error mode: one-shot errors are red toasts (12s TTL, Esc dismisses; #116) — no error may block input.

### File operations on uncommitted changes

`s` stage/unstage | `r` restore (discard changes) | `i` gitignore | `v` archive to `.archive/` | `Delete` trash untracked (recycle bin) | `Ctrl+Z` undo last op | `Space` open file | `f` toggle folder grouping | `Ctrl+F` filter

Fix root causes, not symptoms. Avoid band-aids like stopPropagation, setTimeout, or flags to mask bugs.

