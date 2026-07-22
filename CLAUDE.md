# CLAUDE.md

keifu (系譜) is a Rust TUI for git graph visualization — a VSCode-like Git Graph + Source Control experience in the terminal. Ratatui + Crossterm for rendering, git2 (libgit2) for git operations. `cargo run -- /path/to/repo` runs against a specific repo (defaults to cwd).

## Workflow

- Track work in GitHub Issues (`gh issue create`). `docs/TODO.md` is a historical log — do not add new entries.
- Never commit directly to `chong-dev`. Branch → PR (`gh pr create --base chong-dev`, body `Closes #N`) → squash merge. Keep `main` fast-forwarded from `chong-dev`.
- `cargo test` and `cargo clippy` must pass before a PR.
- Changes with visible TUI behavior get verified in the real app (debug server: `--debug-listen`, see `docs/debugging.md`), not only through unit tests.
- Architectural decisions and gotchas go in `docs/architecture.md` when they land.

## Architecture

Event loop in `main.rs`: render → poll input → `keybindings.rs` maps keys to `Action` variants (routes on `AppMode` × `FocusedPanel`, no business logic) → `app/` handles the action (all state lives on `App`) → `ui/` widgets render statelessly from `&App`. `git/` wraps git2; `git/graph.rs` builds the visual layout. Full map and history: `docs/architecture.md`.

## Load-bearing decisions

Constraints whose violation has caused real bugs — the code shows *what*, this records *why*:

- **Panel focus is orthogonal to mode.** `FocusedPanel` is a field on `App`, not a mode variant; modes overlay it. Same reason `TextEditor` lives on `App.commit_editor`: commit messages must survive focus changes.
- **Errors are red toasts, never blocking.** There is no error mode; no error may swallow input. Toasts are for one-shot outcomes; the status bar is reserved for sticky state (network progress, conflict guidance, latched periodic errors — reported once per failure episode, not per poll).
- **Two-tier diff caching.** Quick cache (sync, names only) shows instantly; full cache loads async with debounce. Uncommitted diff-load errors latch per episode — do not expect every failed poll to surface a fresh message. After file ops, caches are invalidated (stale data stays visible) and the quick diff recomputes synchronously — this is deliberate anti-flash design, not staleness to fix.
- **Settings go through the pure registry** in `src/settings.rs` (no `App` dependency). State-only settings persist via `UiState` to `state.toml`; config-file settings rewrite only touched keys through `toml_edit` so user comments survive.
- **The unicode and pixel dim/render paths are deliberately parallel implementations** (see comments in `ui/graph_view/`). Do not unify them.
- **Clipboard uses shell tools with an OSC 52 fallback** — no clipboard crate (avoids openssl-sys build breakage).

Fix root causes, not symptoms. Avoid band-aids like stopPropagation, setTimeout, or flags to mask bugs.
