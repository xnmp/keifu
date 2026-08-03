# CLAUDE.md

keifu (系譜) is a Rust TUI for git graph visualization — a VSCode-like Git Graph + Source Control experience in the terminal. Ratatui + Crossterm for rendering, git2 (libgit2) for git operations. `cargo run -- /path/to/repo` runs against a specific repo (defaults to cwd).

## Workflow

- Track work in GitHub Issues (`gh issue create`). `docs/TODO.md` is a historical log — do not add new entries.
- Never commit directly to `chong-dev`. Branch → PR (`gh pr create --base chong-dev`, body `Closes #N`) → squash merge. Keep `main` fast-forwarded from `chong-dev`.
- `cargo test` and `cargo clippy` must pass before a PR.
- The cross-platform CI test matrix has a 15-minute job timeout so an intermittent
  runner or test hang settles as a failure instead of pinning a PR indefinitely;
  `tests/ci_timeout_test.rs` guards that workflow contract.
- Changes with visible TUI behavior get verified in the real app (debug server: `--debug-listen`, see `docs/debugging.md`), not only through unit tests.
- Caps Lock warnings must use `KeyEvent.state` from Crossterm keyboard enhancement, not character casing; unsupported terminals leave that state empty and intentionally produce no warning.
- All-key keyboard enhancement must also request alternate keys; normalize its alternate and base-plus-Shift ASCII forms, including chorded Ctrl/Alt keys, at the keybinding boundary so shifted bindings and editor text preserve the user’s layout-correct character.
- Architectural decisions and gotchas go in `docs/architecture.md` when they land.

## Architecture

Event loop in `main.rs`: render → poll input → `keybindings.rs` maps keys to `Action` variants (routes on `AppMode` × `FocusedPanel`, no business logic) → `app/` handles the action (all state lives on `App`) → `ui/` widgets render statelessly from `&App`. `git/` wraps git2; `git/graph.rs` builds the visual layout. Full map and history: `docs/architecture.md`.

## Load-bearing decisions

Constraints whose violation has caused real bugs — the code shows *what*, this records *why*:

- **Panel focus is orthogonal to mode.** `FocusedPanel` is a field on `App`, not a mode variant; modes overlay it. Same reason `TextEditor` lives on `App.commit_editor`: commit messages must survive focus changes.
- **Errors are red toasts, never blocking.** There is no error mode; no error may swallow input. Toasts are for one-shot outcomes; the status bar is reserved for sticky state (network progress, conflict guidance, latched periodic errors — reported once per failure episode, not per poll).
- **Interactive-rebase intent outlives overlay modes.** The editable `RebasePlan` lives on `App`, not inside `AppMode::RebasePlan`, so opening reword input and confirmation cannot discard it. CLI interactive rebases continue/abort through `git rebase`; ordinary libgit2-started rebases must keep using `open_rebase()` because the two on-disk layouts are not interchangeable.
- **Interactive-rebase durable state has two ownership phases.** A plan records the exact branch ref and HEAD it displayed; confirmation reopens Git state and revalidates that snapshot, the clean worktree, and the absence of another operation while holding `.git/keifu-interactive-rebase.lock`, before writing any shared files. Before Git creates `.git/rebase-merge/interactive`, that lock prevents another app from deleting state during hooks; afterward the Git marker is authoritative and a second start must refuse before writing shared files. Continue/Abort reacquire the lock before Git and retain it through undo-seed consumption and cleanup, including the instant after Git removes its marker. Reword uses todo `exec`, so the initial Git command must enable failed-exec rescheduling or Continue can silently skip an authored message after a hook failure. The todo/message directory carries the pre-HEAD/count/base seed across pause/relaunch; failed starts, completion, external abort, startup, and refresh reconcile only unlocked orphans.
- **Two-tier diff caching.** Quick cache (sync, names only) shows instantly; full cache loads async with debounce. Uncommitted diff-load errors latch per episode — do not expect every failed poll to surface a fresh message. After file ops, caches are invalidated (stale data stays visible) and the quick diff recomputes synchronously — this is deliberate anti-flash design, not staleness to fix.
- **Merged-lane dimming yields to the selected trace.** A selected merged branch must keep its complete path, including its merge arc into the trunk, visible in both Unicode and pixel graph renderers; unrelated merged work remains dimmed.
- **Settings go through the pure registry** in `src/settings.rs` (no `App` dependency). State-only settings persist via `UiState` to `state.toml`; config-file settings rewrite only touched keys through `toml_edit` so user comments survive.
- **Panel-title visibility is one shared UI-state preference with three render owners.** The graph, files, and commit-detail widgets each keep their border and content when titles are off; graph filter text is also a title and must remain hidden.
- **The unicode and pixel dim/render paths are deliberately parallel implementations** (see comments in `ui/graph_view/`). Do not unify them.
- **Pixel graph cell geometry is mutable at runtime.** On terminal resize, derive it from `crossterm::terminal::window_size()` and rebuild the `ratatui_image::Picker` as well as clearing pixel protocol caches: the picker embeds its own font metrics, so changing only `PixelGraphState.font_size` leaves fixed-size image protocols stale.
- **Squash-merge connector identity is its endpoint pair, not a branch name.** Local and remote refs can alias the same tip and target; resolve them through `SquashMergeLine::from_branch_targets` so the graph draws one connector.
- **Clipboard uses shell tools with an OSC 52 fallback** — no clipboard crate (avoids openssl-sys build breakage).
- **The full-screen issue list owns mouse hit-testing.** Its content rect must be recorded as the active popup so list-row clicks use the widget's window offset rather than assuming row zero. Issue Detail has no clickable rows.
- **The command palette derives contextual commit operations from the Enter-menu availability builder.** Keep `available_commit_menu_items` as the shared source so unavailable actions never appear as palette dead ends and both routes preserve the same confirmation/prompt flow. Contextual rows capture and revalidate their commit/stash/branch target; branch/tag prompts retain the validated commit OID through final confirmation, and reset retains it through its submenu.
- **Browser-based PR eligibility uses GitHub repository metadata.** Fetch the repository URL and `defaultBranchRef` asynchronously with the PR data; do not infer the PR base from a local `main`/`master` name or reuse `BranchInfo.ahead` (which is only relative to that branch's upstream). An empty open-PR map is not authoritative until its first successful fetch. The existing in-app Create PR flow remains a separate action.
- **Keyboard shortcuts resolve through the pure keymap registry.** Public action IDs, aliases, context overlap, effective labels, and startup warnings live in `src/keymap.rs`; routing, Help, and the palette consume that one result. Help rows attach stable action IDs directly—never infer an action from translated or editable description prose. Preserve TOML source order for last-wins conflicts, and keep payload-carrying text/editor actions internal.

Fix root causes, not symptoms. Avoid band-aids like stopPropagation, setTimeout, or flags to mask bugs.
