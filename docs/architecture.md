# Architecture Decisions

## Panel Focus System (2026-03-25)

**Decision:** `FocusedPanel` is a field on `App`, not a new `AppMode` variant.

**Why:** The `FileDiff` mode and the various popups work as overlays on top of the graph/files panes. If we made panel focus a mode too, we'd need to handle mode transitions between every overlay and each panel focus state — an explosion of combinations. Keeping focus as a field (`FocusedPanel::{Graph, Files, CommitDetail}`) means modes and panel focus are orthogonal.

**How it works:** The keybinding router checks both `app.mode` and `app.focused_panel` to determine which key handler to use. In `AppMode::Normal`, keys are routed to `handle_graph_action`, `handle_files_action`, or `handle_commit_detail_action` based on focused panel. The files pane is the `Files` focused panel, not a mode. Other modes (Help, Input, Confirm, CommitMenu, FileDiff, and the menu/popup modes) handle their own keys regardless of focused panel.

## Text Editor Persistence (2026-03-25)

**Decision:** `TextEditor` lives on `App.commit_editor`, not inside an `AppMode` variant.

**Why:** The commit message must survive focus changes. If the user types a message, switches to the files panel to stage files, then switches back, the message should still be there. Storing it in a mode variant would lose it on mode transitions.

**Caveat:** `editing_commit_message: bool` is a separate flag that controls whether key events route to the editor. This flag is cleared when focus leaves the commit panel or when Esc is pressed.

## Quick Diff Cache (2026-03-25)

**Decision:** Two-tier diff caching: `quick_diff_cache` (synchronous, file names only) and `diff_cache` (async, with line stats).

**Why:** The async diff computation has a 120ms debounce + computation time. During this window, showing "Loading..." is a poor UX when we can show file names instantly. The quick diff uses `diff.deltas()` without computing patches, which is nearly instant.

**How it works:**
1. When `sync_selected_diff_target` detects a target change, it synchronously computes the quick file list
2. The UI calls `cached_diff_or_quick()` which returns the full diff if available, otherwise the quick cache
3. When line stats are still loading, file entries show "..." instead of +X/-Y

## Clipboard via Shell Commands (2026-03-25)

**Decision:** Use shell commands (`xclip`, `xsel`, `wl-copy`, `pbcopy`) for clipboard instead of a Rust crate.

**Why:** The `arboard` and `cli-clipboard` crates both pull in `openssl-sys` which fails to build with vendored OpenSSL on some systems (assembler errors with newer toolchains). Shell commands work universally and add zero dependencies.

**Update (2026-07-19):** Added an OSC 52 fallback (`tui::copy_to_clipboard_osc52`) for when no shell clipboard tool is found — it emits `\x1b]52;c;<base64>\x07` directly to stdout, which works headless/over SSH with no external binary. It's a fallback rather than the primary path because not every terminal emulator supports OSC 52 (unlike xclip/wl-copy, which either work or clearly don't). The base64 encoder is a small hand-rolled function (no new dependency — `base64` only appears in `Cargo.lock` transitively). Payloads are capped at 100,000 base64 chars (matching common terminal limits, e.g. xterm's default) and truncated on a 3-byte boundary if oversized; callers surface `" (via OSC 52[, truncated])"` in the existing status-line message when the fallback fires.

## StageStatus Tracking (2026-03-25)

**Decision:** `FileDiffInfo.stage_status` is set during `from_working_tree()` BEFORE the merge scan, and separate `staged_files`/`unstaged_files` vectors are stored alongside the merged `files` list.

**Why:** The merge scan combines files that appear in both staged and unstaged diffs (e.g., partially staged files). Keeping pre-merge copies preserves the staged/unstaged distinction for the UI. The merged `files` list is still used for total counts and the flat file view for committed changes.

## Deferred Filesystem Watcher (2026-07-13)

**Decision:** `FsWatcher` is constructed on a background thread (`FsWatcher::spawn`) and installed into `App.watcher` by `poll_fs_watcher` once ready, instead of synchronously in `App::new`.

**Why:** Registering a recursive inotify watch walks every directory in the working tree (including `node_modules`, `target`, `.git/objects`). Profiling showed this was 91–94% of pre-first-frame time — 142ms on a small repo, ~500ms on a 135k-file repo. The watcher only drives auto-refresh; nothing about the first frame needs it. Events during the sub-second construction window are covered by the auto-refresh timer.

**Also:** the OSC-11 terminal background-color query (blocking, typically 5–15ms, worst case 100ms) runs on a parallel thread during `App::new` and is joined before `tui::init`, so it can't race the TUI's raw-mode handling.

**Future:** the watch could be scoped to `.git/{refs,HEAD,...}` + non-ignored directories, cutting inotify watch counts 10–100× (relevant near `fs.inotify.max_user_watches`).

## Diff Viewer File List = Display Order (2026-07-13)

**Decision:** The `FileDiff` viewer's `file_list` snapshot is built from the files pane's display items (`display_file_list()`), not from the deduplicated `diff.files`.

**Why:** `file_index` is computed in display space (a partially-staged file appears once in the staged section and once in the unstaged section). Mixing display-space indices with the shorter deduplicated list caused an out-of-bounds panic in PrevFile navigation. One index space everywhere: display order.

## UI Render Pass May Write Layout State Back to App (2026-07-13)

**Documented exception:** `ui/*` is otherwise stateless over `&App`, but `draw()` writes render-time layout facts back to `App` (`sync_file_list_cache`, `diff_viewport_*`, commit-detail scroll clamps) because terminal size is only known at render time. This is intentional; new widgets should not add other kinds of mutation.

## Merge-Conflict Awareness (2026-07-13)

**Decision:** A conflict is a first-class *outcome*, not an error. `merge_branch` / `rebase_branch` / `cherry_pick` / `revert_commit` return `OpOutcome::{Completed, Conflicts{count}}` and deliberately leave the repo mid-operation (conflicted index + MERGE_HEAD / REBASE_HEAD / CHERRY_PICK_HEAD / REVERT_HEAD). Callers (`app/confirm_actions.rs`) route conflicts to a guided "resolve then Continue / Abort" flow via `App::handle_op_outcome`, not the raw error popup.

**In-progress state** comes from `GitRepository::operation_state()` (`OperationState`, mapped from `git2::RepositoryState`) and `conflicted_count()` (`Status::CONFLICTED`), both refreshed in `refresh()`. `get_working_tree_status` must include `CONFLICTED` — otherwise a merge whose only change is the conflicted file leaves the uncommitted node (and its files) invisible.

**Conflicted files** carry `StageStatus::Conflicted`. An unmerged path surfaces in *both* the HEAD→index and index→workdir diffs, so both `from_working_tree` and `quick_file_list_for_working_tree` drop it from the staged side and keep one entry on the unstaged side; the files pane groups those into a "Merge Changes" section rendered first (marker `!`).

**Gotcha — rebase abort/continue must use libgit2, not the CLI.** `rebase_branch` starts the rebase via `repo.rebase()`, which writes a `.git/rebase-merge` layout *without* a `git-rebase-todo`. `git rebase --continue/--abort` then fails with "could not open '.git/rebase-merge/git-rebase-todo'". So `abort_operation`/`continue_operation` special-case `Rebase` to `Rebase::abort()` / `open_rebase()+commit()+finish()`. Merge/cherry-pick/revert use `git <op> --abort|--continue` (libgit2's merge writes a CLI-compatible MERGE_HEAD; cherry-pick/revert are CLI-driven throughout). Continue runs with `GIT_EDITOR=true` so it never blocks the TUI.

**Keys (files pane):** `o` accept ours, `t` accept theirs, `c` continue, `A` abort (behind the Confirm dialog).

## Hunk-Level Staging Model (2026-07-13)

**Decision:** The FileDiff viewer for uncommitted changes shows the *combined*
`git diff HEAD` diff (`diff_tree_to_workdir_with_index`). Hunk operations
synthesise a minimal single-hunk unified diff for the hunk under the cursor and
shell out to `git apply`: stage → `--cached`, unstage → `--cached -R`, discard
→ `-R` (working tree, routed through Confirm). Direction is chosen by the key
(`s`/`u`/`x`), not inferred from the hunk, because the combined view cannot tell
a staged hunk from an unstaged one.

**Why patch synthesis, not libgit2 apply:** libgit2 has no hunk-scoped index
apply. Patches are built by `git/patch.rs::extract_hunk_from_working_tree` from
libgit2's *raw* line bytes (not the display `DiffLineContent`, whose content is
trimmed and would lose CRLF), then rendered by the pure `render_hunk_patch`. A
line whose raw content lacks a trailing `\n` yields the `\ No newline at end of
file` marker; CRLF endings pass through verbatim.

**Correctness boundary:** `git apply` validates the patch against the target
(index or worktree) and fails loudly rather than corrupting state. In the common
case (a file with only unstaged changes, so index == HEAD) every direction
applies cleanly. Untracked files have no index/HEAD entry, so stage-hunk falls
back to a whole-file `git add` (the combined diff is a single all-additions hunk
== the whole file) and unstage/discard-hunk defer to the files pane.

## Pixel-Rendered Graph (2026-07-18)

**Decision:** Draw graph lines as transparent RGBA images via a terminal
graphics protocol, overlaid on top of a blanked graph column, instead of
box-drawing glyphs. Off by default-to-Unicode unless a protocol is detected.

**Why:** Box-drawing glyphs leave visible gaps between rows (a `│` never quite
touches the cell above/below, `●` is barely wider than the line), so the graph
reads as dashed rather than continuous. Rasterizing each row lets lines touch
cell edges exactly and dots be visibly wider than lines, matching VSCode Git
Graph.

**How it works:**
1. `PixelGraphState::new()` (`ui/graph_pixels.rs`) calls
   `ratatui_image::Picker::from_query_stdio()` once at startup — after raw mode
   is enabled but before the event loop polls, so crossterm's reader doesn't eat
   the terminal's query reply. It returns `None` (→ Unicode fallback) unless a
   *transparency-preserving* protocol is detected: only **Kitty** and **iTerm2**
   are whitelisted. Halfblocks isn't graphics, and Sixel's encoder drops the
   alpha channel (`to_rgb8`), which would paint black boxes over the selection
   highlight. `config.ui.graph_renderer` gates this: `Unicode` skips detection
   entirely; `Auto`/`Pixel` attempt it.
2. Each visible row is described by a `RowSpec` — a fully-resolved, hashable list
   of `PixelCell`s (shape + concrete RGB, resolved from the theme; commit dots
   carry `connect_up`/`connect_down` bits computed from whether the adjacent
   *visible* rows' cells touch the shared edge). `build_row_spec` takes the
   previous/next visible nodes (adjacency follows `visible_commit_indices` in
   filtered mode, not raw node order).
3. `rasterize_row` is a pure, deterministic function drawing lines (distance-based
   anti-aliasing), quadratic-bezier arcs for branch/merge corners, and commit
   dots (filled disc / ring for HEAD / hollow for uncommitted) onto a transparent
   canvas at exactly `n_cells * cell_w × cell_h` pixels.
   Each spec's cells are truncated to the width the overlay will draw, because
   the iTerm2/Sixel fixed protocols render *nothing* when the protocol is wider
   than its render area (only Kitty crops) — the cached protocol's cell-width
   must equal the overlay rect.
4. Protocols are cached in a `HashMap<RowSpec, Protocol>`; on overflow past 1024
   entries the cache is pruned down to the current frame's spec set (not cleared
   wholesale), so the hot set survives. Protocol-creation failures are counted;
   after 3 in a row the state is poisoned and the app permanently falls back to
   Unicode instead of re-encoding every frame.
5. In `ui/mod.rs::draw`, a pre-pass (needs `&mut app`) builds the row specs —
   cached on `App` keyed by `(graph_generation, commit_filter, width)` and
   rebuilt only when one changes — then `sync_frame` transmits/evicts protocols
   before the graph widget's immutable borrow. In pixel mode `render_graph_line`
   emits blank spaces of the exact same width as the glyphs (HEAD star included)
   so text layout is unchanged, then `overlay_pixel_graph` renders each row's
   `Image` at the shared `GRAPH_LEADING_COLUMNS` x-offset using the scroll offset
   the list widget wrote back into `graph_list_state`. Both the space-emitter and
   the overlay share `visible_nodes()` for row ordering and the same offset
   constant, so they can't drift. Images are transparent so the list's selection
   highlight shows through.

**Color mapping:** theme lane colors may be named ANSI variants, not RGB.
`color_to_rgb` maps named/Indexed colors to real RGB (standard xterm values,
xterm-256 palette formula) so rasterization has concrete colors.

**S-curve lane transitions (2026-07-19, reworked 2026-07-20).** Lane-to-lane
shifts render as VSCode-style cubic Bézier S-curves rather than a straight
horizontal run + a tight rounded corner. `draw_cells` (`src/ui/graph_pixels.rs`)
runs in two phases:
1. `transition_curves` (~line 908) — a pure, row-local scan of `spec.cells` —
   reconstructs each maximal horizontal-family run into cubics via
   `cubic_between` (~line 863). A run's endpoints are its corners/Tees plus any
   flanking commit dot, classified as *hubs* (row mid-height: a flanking dot,
   or a `TeeRight`/`TeeLeft` that stays on the trunk with no flanking dot — a
   fork connector's trunk, whose up-arms fan from it) or *spokes* (a row edge:
   `Merge*`/`TeeUp` turning up to the top edge, `Branch*` and `TeeDown` turning
   down to the bottom edge, and a `Tee{Right,Left}` that *is* flanked by a dot —
   that Tee is the commit's parent-connector into a still-descending trunk, so
   it sweeps down to the bottom edge like a `Branch*` rather than staying flat). One
   cubic is drawn from the run's primary hub to each spoke (a branch/merge is
   the 2-endpoint case → one curve; a fork connector `├─┴─╯` fans several);
   with no hub the spokes are chained pairwise; a lone hub/spoke with nothing
   to connect to bridges straight to the run's far edge. Each spoke curve
   carries the spoke's own flat color/dim for its entire sweep, so a
   multi-color fork connector colors each arm correctly.

   `cubic_between(a, b)` (~line 863) places **both** control points at the
   endpoints' shared y-midpoint — `(a.x, ymid)` and `(b.x, ymid)` — rather than
   at a hand-tuned hub-tilt/fan offset. That makes every curve leave and enter
   *every* endpoint travelling vertically, so its tangent matches the straight
   vertical lane pipe immediately above/below the transition: the join is
   C1-continuous with no kink or flat spot. Curves sharing a hub separate by
   *direction* (up-arms vs. down-arms) rather than by a tilt hack, and when
   both endpoints share a y (a flat dot↔dot stub run) the two control points
   collapse onto the endpoint line and the cubic degenerates to a straight
   horizontal segment. This replaced an earlier "Tangent hub/spoke" model
   (horizontal-tangent hubs eased by `CURVE_EASE`, with `HUB_TILT` and
   `FAN_EXTRA` hand-tuning how much a hub handle leaned toward its spokes to
   keep co-hub curves from coinciding) — the vertical-tangent construction
   gets the same visual separation for free from the endpoint geometry, with
   no tuning constants. The old algorithm survives only as a verbatim,
   `#[cfg(test)]`-gated copy (`legacy_cubic`/`LEGACY_CURVE_EASE`/
   `LEGACY_HUB_TILT`/`LEGACY_FAN_EXTRA`, near the bottom of the `tests` module)
   that `rasterize_row_with` can drive for the PNG before/after comparison
   harness (`raster_debug`); it is not reachable from production code.
2. The straight verticals (lane pipes, `HorizontalPipe`'s crossed pipe, `Tee`
   trunks, commit connectors) and the dots/stars draw *on top* of the curves, so
   a crossed pipe stays visible over the sweep and dims independently.

Curve endpoints are exact lane centers on row edges (spokes) or dot centers
(hubs), so rows still tile seamlessly (no new gap class) and each curve meets
the dot it belongs to. Because `transition_curves` is pure over the cells,
drawing stays a deterministic function of the `RowSpec`, so the protocol cache
stays correct. Unicode mode is unchanged (box glyphs can't curve).

**Squash link joins the uncommitted band via `TeeDown` (2026-07-22, #115).**
When HEAD sits on a squash-merge target with uncommitted changes, two layout
overlays land on the same row: the uncommitted node's horizontal landing band
and the squash link — and the band sits geometrically *between* the link's
endpoints, so no clean column exists. The overlay writers can only cross
`Pipe` cells (`HorizontalPipe`); anything else used to force the link onto a
fresh far lane (the #110 phantom-detour shape) while the crossing run was
silently severed by the band, leaving orphan curve fragments in both
renderers. `draw_squash_link` now *joins* the band instead: a
`CellType::TeeDown(band_color, stem_color)` junction (┬) on the tip's own lane
lets the band carry the link into the target dot while the stem drops straight
into the tip dot. Junction preconditions (`junction_ok`): the blocking cell is
an overlay `Horizontal` with no trace edges, and every cell between it and the
target dot is horizontal-family (a continuous corridor). The pixel renderer
treats `TeeDown` as a down-spoke (its stem is the SECONDARY stroke, like a
`Tee*` arm), so `transition_curves` draws one grey S from the target dot into
the tip dot. The mirror orientation (band on the *lower* endpoint row) has no
junction cell and keeps the old fallback.

**Uncommitted connector survives the commit filter (2026-07-19).** The synthetic
uncommitted-changes node always passes the commit filter, and its connector is
wired to HEAD at graph-build time. If HEAD's own message missed the filter it
was hidden, orphaning the connector into a dangling grey stub beneath the top
marker. `node_passes_commit_filter` now also keeps the HEAD row whenever an
uncommitted node exists (`has_uncommitted_node`, O(1) — the uncommitted node is
always at index 0), so the connector always terminates at the star.

## Toasts vs. Status Bar (2026-07-20)

**Decision:** One-shot notifications ("staged 3 files", "push failed") go
through a toast queue; the status bar is reserved for *sticky* state that
describes what's currently true, not what just happened.

**Toast queue** (`src/toast.rs`) is a pure state machine — `ToastQueue::push`/
`evict(now)` take time as a parameter, so expiry is unit-testable without a
clock. `ToastKind::{Info, Success, Error}` drive color and TTL: Info/Success
live 4s, Error lingers 12s. `push` evicts expired toasts, then caps the queue
at `MAX_VISIBLE = 3` by dropping the oldest. ~95 former one-shot
`set_message()` call sites were converted to toasts (2026-07-20 sweep).
Errors are toast-only (#116): `App::show_error` pushes an Error toast — the
old input-swallowing `AppMode::Error` modal is gone, so no failure can lock
the UI. Esc dismisses lingering error toasts first (`dismiss_errors`), then
falls through to its usual quit/cancel meaning; info/success toasts never
intercept Esc.

**Status bar** stays reserved for state a user should be able to glance at any
time: sticky network progress (`set_progress_message()` marks a message
sticky; it persists for the whole in-flight op and is explicitly cleared on
completion — a plain, non-sticky message instead self-expires after 5s and is
never resurrected by later background activity, fixing an earlier "stale
message re-flashes when a silent auto-fetch runs" bug), merge-conflict
guidance, latched periodic errors, and the chips described below.

**Episode latching.** A background poll that fails on every tick (e.g. the
working tree is mid-churn) must not spam a fresh error every tick — but a
*new* failure episode should still report once. Four `bool` latch flags
implement this pattern uniformly: set on the first failure since the last
success, left alone on subsequent failures, cleared on success (which re-arms
reporting for the next episode):
- `DiffCache::uncommitted_diff_error_reported` (`src/diff_cache.rs`) — set on
  an uncommitted-diff load failure, cleared on success or on
  `clear_uncommitted()`.
- `App.refresh_latches.wt_status` (the `RefreshLatches` bag in
  `src/app/mod.rs`, set/cleared in `src/app/refresh.rs`) — working-tree-status
  failures during periodic refresh.
- `App.refresh_latches.auto_refresh` — auto-refresh timer failures
  (`src/app/network_ops.rs`).
- `App.refresh_latches.watch_refresh` — filesystem-watcher-driven refresh
  failures (`src/app/network_ops.rs`). A fourth latch,
  `App.refresh_latches.auto_fetch`, follows the same shape for background
  auto-fetch failures.

All four follow `if !latched { latched = true; report(error) }` on failure and
`latched = false` on success — the shape to copy for any new periodic
background check. Two more of the same shape were added in #65 for the
background gh polls: `App.refresh_latches.pr_fetch` (open-PR poll) and
`App.refresh_latches.merged_fetch` (merged-PR poll), both set/cleared in
`src/app/network_ops.rs`. On failure the *last-good* data is kept (the PR map /
merged-branch set is not wiped), so a transient gh error can't blank the badges.

## Settings Registry (2026-07-20)

**Decision:** `Ctrl+,` settings menu is backed by a descriptor registry
(`src/settings.rs::descriptors()`) that is the single source of truth for what
settings exist and how they behave. There is no separate `SettingsModel`
snapshot and no `settings_model()`/`apply_settings_model()` projection — each
descriptor carries fn-pointer accessors that read and write the live value
**directly on `App`**, and a persistence lens, so the menu and the store share
one definition.

**Why:** One entry per setting means no hand-written projection to drift out of
sync. The pure value operations (`cycle_value`, `clamp_int`, `format_value`,
`filter_descriptors`) take no `App`, so the menu logic (cycling, clamping,
display, fuzzy filtering) stays unit-testable without a TUI.

**How it works:**
- `SettingDescriptor` — one menu row: a `label`, a `SettingGroup`
  (Graph/Files/Refresh/Interface, in menu order), a `SettingKind` (`Bool` /
  `Enum{options}` / `Int{min,max,step,zero_label}`), an optional `note` (e.g.
  "restart"), a `SettingStore` (persistence destination), and `get`/`set`
  fn-pointers onto `App`. Descriptors are built by the `state_bool!` /
  `config_bool!` / `config_int!` macros so the common shapes are declared in a
  single line.
- **Persistence lives in the descriptor.** `SettingStore::State { read, write }`
  carries the lens between the `SettingValue` and its `UiState` field, so
  state-persisted settings save to `state.toml` via `App::save_ui_state`
  (a loop over the descriptors, `UiState::save()` underneath). `SettingStore::Config`
  settings write `app.config.*` through the `set` accessor and persist via
  `Config::save()` (`src/config.rs`) using **`toml_edit`**, which rewrites only
  the touched keys in the existing document (`doc["refresh"]["auto_refresh"] =
  value(...)`) rather than re-serializing — so comments and keys the menu
  doesn't know about survive a save. `App::settings_snapshot` is the read-side
  loop.
- **Clamping / sentinels are in the kind, not a projection.** `clamp_int`
  bounds an `Int` to its `min`/`max` (e.g. graph split ratio 20-80); a
  `zero_label` (e.g. "uncapped" for the graph width cap) shows a friendly token
  in place of `0`, encoded once via the `cap_to_value`/`value_to_cap` helpers.
- `graph_renderer` is **restart-only**: its descriptor carries a `note:
  Some("restart")` shown in the menu, because `PixelGraphState` (terminal
  graphics-protocol detection) is constructed once in `main.rs` before the
  event loop starts; changing the config value takes effect only on the next
  launch.

## Merged-Branch Classification (2026-07-20)

**Decision:** Whether a branch is merged is a pure domain computation
(`src/git/merged.rs`), fed by async GitHub polling infrastructure
(`src/merged_branch_fetch.rs`) that never blocks a frame.

**Domain logic** (`src/git/merged.rs`):
- `is_ancestor_merged()` — the cheap case: `graph_descendant_of(base_tip,
  branch_tip)` catches merge commits and fast-forwards.
- `is_squash_merged()` — a bounded scan (`SQUASH_SCAN_LIMIT = 400` commits)
  from the branch's fork point (`merge_base(branch_tip, base_tip)`) forward:
  compute the branch's cumulative patch-id across fork→tip
  (`combined_patch_id`), then walk `base_tip` backward hiding the fork point,
  comparing each single-parent base commit's own patch-id
  (`tree_diff_patch_id`) against it. A match means the branch's changes landed
  on the base as a squash commit. Patch-ids are content-addressed
  (`diff.patchid()`), so this survives rebase/re-authoring, not just identical
  commits. The diffs are generated with **zero context lines** (issue #97): a
  patch-id normally folds surrounding context into its hash, so a trunk commit
  editing lines *near* the branch's change (within the default 3-line window)
  shifts the context and breaks the match even though the squash adds exactly
  the same lines. Keying only on the changed lines makes the id survive an
  advancing base while still requiring the whole cumulative change set to be
  identical (it narrows, not widens, what the hash covers).
- `base_branch()` — preference cascade: local `main` → local `master` →
  `origin/main` → `origin/master` → checked-out HEAD.
- `branch_content_in_base()` — a GitHub-signal cross-check: given `gh`'s
  merged-PR list says a branch merged, this runs a **three-way merge**
  (`merge_trees`) of the branch into the base with the fork point as the
  common ancestor, and reports contained only when the merged tree equals the
  base tree (and doesn't conflict). Anchoring to the fork ancestor is what lets
  it survive an advancing base — a trunk edit to a file the branch also carries
  an older copy of is attributed to the base side, not counted as branch work
  (issue #97). Any change the base lacks leaves the merged tree ≠ base (or
  conflicts), so the GitHub signal alone can't be trusted (guards against
  branch-name reuse, and against a conflict-resolved landing being over-claimed
  locally).

**Transitive closure** (2026-07-22): classification iterates to a fixed point
(`classify_merged_branches_with_targets`) — after each pass, the tips of
newly-classified branches join the tested tip set and the still-unclassified
branches are re-tested, until a pass classifies nothing new. This catches
work that reached the trunk only through a chain (a stacked PR squashed into
its parent branch, a sub-branch folded into a feature before the feature was
squashed): no direct trunk signal exists for those, since a squash shares no
commits. The trunk first-parent "behind" guard stays trunk-only on purpose —
an old pointer into a *merged* branch's line is a dead pointer into an
abandoned line and classifies via ancestry, unlike a pointer into a live
trunk line. (Caveat: if a later commit on the merged branch superseded the
pointed-at tree, that intermediate tree never literally landed on trunk —
the pointer still classifies. Accepted: such pointers are stale leftovers,
and the classification self-heals the moment the branch gets a new commit.)
`--explain-merged`
reports transitively-classified branches in a dedicated section naming the
merged branch they landed through.

**Selection exemption** (2026-07-22): the merged-lane *dim* never applies to
the commit under the cursor — `resolve_row_model` drops the merged-lane mute
on the selected row (chips included), and both stroke renderers exempt any
edge touching the selected commit's oid (`edge_touches_merged`'s `exempt`
parameter, threaded as `RowRenderCtx::merged_exempt` / the pixel dim core's
`merged_exempt`). Rationale: the widget-level `selection_style` only
subtracts the DIM bit, so without this the selected row kept its muted
foreground and dimmed strokes — inspecting a merged commit read greyed-out.

**Async infrastructure** (`src/merged_branch_fetch.rs`):
- The merged-PR-branch poll (`gh pr list --state merged`, 10s timeout, 300s
  interval) is an `IntervalFetch<HashSet<String>>` (see *Async Worker Shapes*
  below), never on the render thread.
- `MergedClassifier` reruns the pure classifier in a background worker, gated
  by an input signature (`ClassifyInput::signature()`, an order-independent
  XOR-hash over base name/tip, every branch's `(name, tip, is_remote,
  is_head)`, and the gh-merged name set) — `maybe_start()` is a no-op when the
  signature hasn't changed, so a worker only spawns when something that could
  change the classification actually changed.

**Shift+H** (`Action::ToggleMergedBranches`, mnemonic "Hide merged" — joins
`Shift+B` filter and `Shift+O` remotes-hidden) flips `App.merged.hide`
and refreshes. `visible_branches` composes three independent filters: not
individually hidden AND not remote-only-hidden AND not (merged AND
hide-merged-branches-on) — merged branches are removed from the graph
entirely when hidden, not just their labels.

## Lane-0 HEAD Invariant (2026-07-20)

**Decision:** `build_graph` (`src/git/graph.rs`) structurally reserves lane 0
for HEAD's first-parent line, when HEAD is known.

**Why:** So the checked-out branch's line is always the leftmost, stable lane
— matching VSCode Git Graph and giving the user a fixed visual anchor instead
of "whichever tip was walked first happens to land on the left."

**How it works:** `head_first_parent_line()` walks down from HEAD along first
parents and up through descendants whose first parent continues the line,
producing the set of commit OIDs that make up "HEAD's line." If non-empty,
`build_graph` seeds lane 0 as an empty reserved slot before assigning any
commit a lane; `eligible_empty_lane()` then refuses lane 0 to any commit not
in that set, so only HEAD's line can ever land there. `head_commit_oid` (not
the branch-derived HEAD) drives this, so a **detached HEAD** is anchored the
same way. When HEAD is unknown or not yet loaded, the reservation is skipped
entirely and the legacy layout applies: whichever tip is processed first wins
lane 0.

**Lane 0 owns the reserved blue:** when the reservation is active,
`color_assigner.reserve_color(MAIN_BRANCH_COLOR)` takes `MAIN_BRANCH_COLOR`
(LightBlue) out of the general assignment pool, and the first commit that
actually lands on lane 0 claims it via `assign_main_color`, latched by a
`main_color_assigned` flag so no later commit can reclaim it. In the no-HEAD
fallback, the same color still goes to whichever commit first lands on lane 0.

## PrContext (2026-07-20)

**Decision:** Open-PR data is indexed once per frame into `PrContext`
(`src/pr.rs`), keyed by head commit OID, rather than looked up per-commit
against the raw PR list.

**How it works:** `PrContext::by_head_oid: HashMap<Oid, &PrInfo>` maps each
open PR's head commit to its info; `pr_for_head_commit()` looks up by OID, so
a badge renders on exactly the PR's head row — other commits on the same
branch (further back in history) never pick one up. `ReviewState::{None,
Approved, ChangesRequested}` maps from GitHub's `reviewDecision` field
(`ReviewState::from_decision`) to the glyph shown next to the badge.
**PR-merge-commit detection** (`is_pr_merge()`) checks, in order: (1) the
commit's second parent OID is a key in the same `head_oids` index — an open
PR whose head just got merged; (2) a message-format fallback,
`message_is_github_merge()`, matching GitHub's `"Merge pull request #<N>
from …"` merge-commit message (distinguishing it from a local `"Merge
branch …"`) — covers PRs that are already closed by the time of render, so
have no open-PR index entry.

## Graph View Widget Modules

**Decision:** The graph-row widget is the `src/ui/graph_view/` package (was one
`graph_view.rs`), split by responsibility with typed seams between the passes so
each is independently testable and the render tail carries no ad-hoc tuples:

- `mod.rs` — widget entry / glue (`render_graph_line`, list-item assembly).
- `metrics.rs` — text metrics: VS16-aware display width, width-bounded
  truncation, compact relative-date formatting.
- `geometry.rs` — graph-column geometry: `GRAPH_LEADING_COLUMNS`, avatar
  reservations, effective width / cap arithmetic, the per-row truncation budget
  shared by the unicode and pixel renderers.
- `rows.rs` — row folding + viewport windowing (`RenderRow`,
  `visible_row_window`).
- `chips.rs` — branch-chip construction. `optimize_branch_display` yields typed
  `BranchChip`s, each carrying the real click-target ref from construction (so
  the hit-test isn't re-derived at render time).
- `badges.rs` — pure PR/merged badge decisions; `pr_for_row` returns a typed
  `PrBadge` the render tail draws directly.
- `pixel_dim.rs` — pixel base-spec build (`build_pixel_base_specs`) + per-frame
  dim windowing (`dim_pixel_specs_window`, `apply_trace_dim`).
- `row.rs` — the message tail as a pure decision pass (`resolve_row_model` →
  `RowModel` of styling/label/message *decisions*, no widths or spans) followed
  by a layout pass (`layout_row`) that turns the model into the final `Line` +
  `ChipHit`s.

## Windowed Rendering (2026-07-20)

**Decision:** Both the text (Unicode) graph and the pixel-graph path avoid
rebuilding every row on every keypress; only the visible window is rebuilt,
and pixel caching goes further by never invalidating on trace state.

**Text layer:** `visible_row_window()` (`src/ui/graph_view/rows.rs`) computes which
row indices actually need styled `ListItem`s built — the viewport plus an
8-row margin, always including the selected row. Everything outside the
window gets a cheap blank placeholder so the list's length (and therefore
scrollbar/selection math) is unaffected. This dropped per-keypress item
building from ~15.3ms to ~4ms at 5.6k nodes.

**Pixel layer:** `PixelGraphState::protocols: HashMap<RowSpec, Protocol>`
caches the rendered image per row spec. `RowSpec` no longer includes trace
selection in its key — trace dim is instead applied per-frame, after cloning
a row's cached UNDIMMED base spec, only to rows inside the visible window
(`apply_trace_dim`, called from the windowed dim pass; out-of-window rows get
an empty placeholder spec and skip dimming entirely). This turned a full
`O(n)` RowSpec rebuild (~15ms at 5.2k nodes, since tracing used to be baked
into the cache key) into an `O(window)` dim pass (~1.24ms).

## Status-Bar Chips (2026-07-20)

**Decision:** A cluster of small pure `&App`-state renders in
`src/ui/status_bar.rs` surface persistent, glanceable state as compact chips
rather than transient messages:
- **remotes hidden** — `app.hide_remote_branches`.
- **merged hidden** — mirrors `app.merged.hide` (see Merged-Branch
  Classification above).
- **compare pending/range** — `app.compare_marked` (one commit picked, still
  choosing the second) vs. `app.compare_range` (both picked).
- **watch off** — `app.watcher_disconnected` (the filesystem watcher died and
  auto-refresh fell back to timer-only).
- **op + conflict count** — `app.op_state.is_in_progress()` and/or
  `app.conflict_count > 0`.

Each chip is a direct, stateless function of an `App` field — no separate chip
state to keep in sync.

## Repo-Handle Reopen (2026-07-20, gated 2026-07-20 #69)

**Decision:** `GitRepository::reopen()` (`src/git/repository.rs`) re-opens the
underlying `git2::Repository` during `reload_refs` (`src/app/refresh.rs`),
best-effort — a reopen failure doesn't abort the refresh.

**Why:** A long-lived `git2::Repository` handle caches refs and config
internally. External processes — another terminal running `git push`/`git
fetch`, an upstream-tracking change, a manual `.git/config` edit — mutate
`.git/refs`/`.git/config` on disk, but the handle won't see those changes
until it's reopened. This is the refs/config analogue of the existing `git2`
ignore-cache pattern (`repo.clear_ignore_rules()` before status queries, see
above): flush a libgit2-internal cache immediately before the queries that
depend on it, so external changes are observed without restarting keifu.

**Gated (`maybe_reopen_repo`, #69):** the reopen no longer runs on every
refresh. It fires only when there's a reason refs could have changed on disk:
a `force` refresh, `App.repo_dirty` (set by `poll_fs_watcher` when the
fs-watcher reports a `.git` ref/HEAD change — `PollResult::Refresh {
git_changed }`, classified in `watcher.rs::touches_git_refs`), or no active
watcher (`self.watcher.is_none()`, the startup/disconnected fallback with no
push-based signal). So a working-tree-only watcher tick or a quiet
auto-refresh timer skips the reopen. On a successful reopen `repo_dirty` and
the `reopen` error latch clear; on failure the old handle is kept, the failure
is reported once per episode via `RefreshLatches::reopen` (same latch shape as
`wt_status`), and `repo_dirty` is left set so the next refresh retries instead
of silently sitting on a stale handle.

## Refresh Phases (2026-07-20, #69)

**Decision:** `refresh_inner` (`src/app/refresh.rs`) is split into four
sequential phases, each with a documented contract (inputs / what it writes /
what it must not touch):

1. `reload_refs(force)` — pull fresh git state off disk into `self` (working
   tree status + latch, op/conflict state, branches, remotes; gated repo
   reopen; kick the merged classifier + recompute base-update merges). Touches
   no graph/selection/cache state.
2. `rebuild_graph()` — recompute `visible_branches` from the already-loaded
   refs + the three visibility filters (incl. hide-merged), revwalk, build the
   layout, bump `graph_generation`, refresh HEAD facts + branch positions.
   Self-contained: re-fetches cheap ref-derived data (stashes/tags/HEAD) itself
   so it needs no `reload_refs`.
3. `restore_selection(snapshot)` — re-point the cursor onto the equivalent row
   (uncommitted node → same branch → same commit OID → nearest valid row),
   using a `SelectionSnapshot` captured before the rebuild.
4. `invalidate_caches(force, …)` — reconcile the diff cache (force clears all;
   auto-refresh keeps it when the same content stays selected), clear stale
   search state, clamp selection, recompute filtered commits.

`rebuild_and_restore` bundles phases 2–4.

**Why / cheaper classifier delivery:** merged-classification delivery
(`update_merged_classification`) previously called a full `refresh(false)` just
to re-apply the merged filter — repaying the reopen + `git status` + branch
enumeration even though only `self.merged.branches` changed. It now calls
`rebuild_and_restore(false)` (phases 2–4), skipping `reload_refs`. The revwalk
+ `build_graph` still run (they must, to apply the filter), but the expensive
disk reloads don't.

## Async Worker Shapes (2026-07-20, #65)

**Decision:** Background workers that shell out to `gh` share two axes —
*trigger* (interval vs. one-shot vs. signature-gated) and *transport* (spawn a
thread, send a `Result` down an `mpsc` channel, poll it) — routed through the
single `gh::run` helper (`src/gh.rs`) for the actual subprocess + timeout.

**Taxonomy:**
- **Interval fetchers** — `src/interval_fetch.rs::IntervalFetch<T>`. A generic
  fetcher parameterized by a poll interval + a producer `Fn(&str) -> Result<T,
  String>`. The two open/merged-PR polls (`pr::open_pr_fetch` →
  `IntervalFetch<HashMap<String, PrInfo>>`, `merged_branch_fetch::
  merged_branch_fetch` → `IntervalFetch<HashSet<String>>`) were byte-identical
  hand-rolled spawn+deadline loops before #65; they now differ only in their
  producer closure. The producer is injectable, so interval gating / delivery /
  error surfacing are unit-tested with no real `gh`.
- **One-shot runners** — `CheckFetch`, `IssueFetch`, `PrThreadFetch`,
  `PrActionRunner`. Fetched-on-demand (popup opens / action fires), each with
  bespoke per-key caching + in-flight tracking (`pending_detail`, per-run log
  cache, per-PR thread cache). They already call `gh::run` directly and already
  surface `Result`; they are *not* forced under a shared abstraction because the
  caching/keying differs per runner and a generic wrapper would need a callback
  for each poll anyway — no net simplification.
- **Signature-gated worker** — `MergedClassifier`. Not an `IntervalFetch`: its
  trigger is an input-signature change (not a clock) and its producer is a
  *local* git classification (not a `gh` call), so there is nothing to route
  through `gh::run` and no interval to honor. Left as its own shape.
- **Deliberately distinct** — `AvatarFetch`, `DiffCache`. Untouched.

**Error convention:** `IntervalFetch::poll` returns `Option<Result<T, String>>`.
A gh-missing / no-remote / timeout failure is surfaced (not mapped to an empty
value) so the caller latches + reports it once per episode (see *Episode
latching*) and keeps the last-good data instead of blanking the badges.
