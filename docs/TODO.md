# Feature Work

Items from the feature plan started on 2026-03-25, organized by area.
All items implemented in the `feat/panel-system-and-features` branch.

---

## Graph Pane

### [DONE] 2026-07-18 Pixel-rendered graph lines
Continuous VSCode-style graph lines via terminal image protocols (Kitty/iTerm2),
replacing gappy box-drawing glyphs. New `ui/graph_pixels.rs` (pure, deterministic
rasterizer + `RowSpec` cache + `PixelGraphState`), overlaid in `ui/mod.rs` on a
blanked graph column so text layout is unchanged and selection highlight shows
through transparent images. Auto-detected at startup; `ui.graph_renderer` config
(`auto`/`unicode`/`pixel`) with Unicode fallback when no protocol is available.
See `docs/architecture.md`.

### [DONE] Commit Options Menu
Enter on a commit opens a full options menu with: checkout, create branch, merge into current branch (if at branch head), cherry-pick, rebase, reset (soft/mixed/hard), add tag, revert, copy commit hash to clipboard, push (if at branch head).

### [DONE] Branch Select/Deselect with Filter
Shift+B in graph pane opens branch filter popup. Space toggles branches, `a` selects all, `n` deselects all, typing filters by name. Hidden branches excluded from graph on refresh.

**Update:** Now performs *real* commit filtering — `get_commits()` accepts the visible branch set and only walks from those tips (HEAD is always pushed too), so hiding a branch removes its exclusive commits from the graph, not just the labels. Commits reachable from a visible branch stay. Hiding every branch still shows HEAD's history.

### [DONE] 2026-07-19 Show/Hide Remote-Only Branches
`Shift+O` (graph pane) toggles visibility of every remote-only branch at once — a remote ref with no matching local branch (matched by upstream config, short name, or shared tip). Hidden remotes drop their labels *and* their exclusive commits, reusing the existing `visible_branches` revwalk path, and compose with the per-branch filter (`hidden_branches`): a branch is visible iff not individually hidden AND not excluded by the remote toggle. Pure classifier `git::branch::remote_only_branch_names()` (unit-tested); state persisted in `UiState.hide_remote_branches` (`state.toml`) and honored on startup; surfaced as a "remotes hidden" status-bar chip and a help-popup entry. Upstream (trasta298) binds this to `o`, but that's Open-PR in the fork, so `Shift+O` — keeps the mnemonic and pairs with `Shift+B`.

### [DONE] 2026-07-19 Filter Branches by Author
The branch-filter picker (`Shift+B`) now attributes an author to each branch and
lets you filter/bulk-hide by author. Author = the author of the *oldest commit
unique to that branch* (reachable from its tip but no other branch tip, found via
`git2` revwalk: push tip, hide all other tips); falls back to the tip commit's
author when the branch has no unique commits (shared/merged tip). Pure, unit-
tested domain fn `git::branch::branch_authors(repo, &branches)`. Computed lazily
when the picker opens and cached on `App`, keyed by a `(name, tip OID)` snapshot
so it only recomputes when tips change — never per keystroke or per refresh. The
picker shows the author (muted, right-aligned); a filter starting with `@` matches
the author (case-insensitive), plain queries still match names. `Ctrl+A`/`Ctrl+O`
(show-all / hide-all) are scoped to the currently filtered subset, so `@alice` +
`Ctrl+O` hides all of alice's branches at once (identical to before when no filter
is active).

### [DONE] Tags Rendered as Graph Refs
Lightweight and annotated tags (peeled to their target commit) are loaded via `repository.get_tags()`, threaded through `build_graph` onto `GraphNode.tag_names`, and rendered next to branch labels as `<tag>` in a distinct tag color (`theme.tag_label`).

---

## Files Pane

### [DONE] Stage/Unstage with `s` Key
When the uncommitted files node is selected and user is in FileSelect mode, pressing `s` stages/unstages the selected file. Files are divided by staged/unstaged sections.

### [DONE] Hunk-Level Stage/Unstage/Discard + Stage-All/Unstage-All (2026-07-13)
In the FileDiff viewer on an uncommitted file: `s` stages, `u` unstages, and `x` discards (via Confirm) the hunk under the cursor. Patches are synthesised from the combined `git diff HEAD` view (`git/patch.rs`) and applied with `git apply --cached` / `--cached -R` / `-R`. Guards: binary files and committed diffs are disabled with a message; untracked files fall back to whole-file `git add`. In the files pane, `S` stages all (`git add -A`) and `U` unstages all (`git reset`). See `docs/architecture.md` → "Hunk-Level Staging Model".

### [DONE] Instant File Display
Files and their M/A/D status show instantly via a synchronous quick scan. Line numbers (+X/-Y) show "..." while the full diff loads asynchronously.

### [DONE] Selectable Folder Headers
In folder mode, folder headers are now selectable. Pressing `s` on a folder header stages/unstages all files in that folder. Pressing `i` gitignores the folder, `v` archives it. Selection index now maps to display items (headers + files) instead of just files.

### [DONE] Folder View with `f` Key
Pressing `f` in the files panel arranges files by folder hierarchy with directory headers. Panel title shows "[folders]" when enabled.

**Note:** Staging a folder header (to stage all files in folder) is not yet implemented — only individual file staging works currently.

### [DONE] Undo with Ctrl+Z
Pressing Ctrl+Z in the files pane undoes the last s/i/v operation. Single-slot undo (last operation only). Undo stage reverses the stage/unstage, undo gitignore removes the pattern from `.gitignore`, undo archive moves the file back from `.archive/`.

### [DONE] Archive File with `v` Key
When the uncommitted files panel is selected, pressing `a` moves the selected file to `.archive/` at the repo root, preserving directory structure. In folder mode, moves the containing folder instead.

### [DONE] Add to .gitignore with `i` Key
When the uncommitted files panel is selected, pressing `i` adds the selected file to `.gitignore`. In folder mode, adds the containing folder instead. Shows a status message confirming the action, and skips duplicates.

### [DONE] Fuzzy Filter Typing in Files Panel
Typing in the files panel filters the file list by character matching. Filter shown in panel title. Backspace removes characters, Esc clears filter.

### [DONE] Fix Laggy/Stale Files Pane When Navigating Commits
Navigating the graph showed the previous commit's file list until the full diff loaded (~150-250ms). Three fixes: background polls (including quick-diff sync) now run after input processing so the new selection's quick diff is computed before the next frame; `update_diff_cache()` reports a needed render when the diff target changes; `cached_diff_or_quick()` no longer falls back to a quick diff computed for a different target.

### [DONE] Fix Selection Jump and Flash After File Operations
After s/i/v in folder view, selection no longer jumps to top. Flash during panel refresh eliminated by keeping stale quick-diff visible and recomputing synchronously before redraw. Cursor advances to next file instead of resetting.

### [DONE] Staged/Unstaged Headers in Folder Mode
When in folder mode with uncommitted changes selected, files are now divided by staged/unstaged sections with folder grouping within each section.

### [DONE] Gitignore Cache Fix
Calling `repo.clear_ignore_rules()` before status queries ensures .gitignore edits take effect immediately without restarting keifu.

### [DONE] Delete Key to Recycle Bin
Pressing Delete in the files pane shows a confirmation modal, then moves the file to the system recycle bin via the `trash` crate. Works with folder headers to trash all files in a folder.

---

## Commit Pane

### [DONE] Full Text Editor with Micro-like Keybindings
Typable commit message when uncommitted node is selected. Alt+Enter commits. Full micro-like editing: word navigation (Alt+arrows, word boundaries = spaces only), shift+arrows for selection, Home/End, Ctrl+Home/End for text start/end, up/down for line navigation at same column.

### [DONE] Enter to Start Editing, Esc to Stop
Must hit Enter in the commit detail panel to start editing the commit message. Esc stops editing and left/right returns to panel navigation.

### [DONE] Message Retained When Panel Loses Focus
The commit message persists when the panel loses focus.

---

## Panel System

### [DONE] Panel Navigation
- Left/right arrows switch between panels (Graph -> Files -> CommitDetail, wrapping)
- Esc from files or commit detail panel returns focus to graph
- Enter from graph on uncommitted node goes to files panel
- Green border highlight on focused panel
- Ctrl+Q quits from anywhere

---

## Hotkeys

### [DONE] Remove j/k from Graph Panel
j/k removed from graph panel movement (arrow keys only). j/k retained in FileSelect, FileDiff, and commit menu modes.

### [DONE] Updated Status Bar and Help
- Status bar shows panel-specific key hints
- Help popup updated with all new keybindings organized by context
- Ctrl+Q documented as quit-from-anywhere

---

## GitHub Integration

### [DONE] 2026-07-19 GitHub issue viewing/management (issue #37)
Issues from the TUI, mirroring the PR feature's architecture 1:1: `Shift+I`
opens the list popup (open/closed/all filter via Tab, `r` refresh, `n` new,
`o` browser), Enter opens detail with the comment thread; from detail `c`
comments, `x` close/reopen (confirmed), `l` label checkbox picker, `a`
assignee input (comma-separated logins, set-diffed against current). Backend
is the `gh` CLI via `crate::gh::run`: `src/issue.rs` (models + parsing +
on-demand `IssueFetch` — list, per-number detail cache, one-shot label list),
`src/issue_action.rs` (pure tested `build_args`, bodies via `--body-file`,
`IssueActionRunner`). View state lives in `Option<IssueListView/DetailView>`
on `App` with Loading/Ready/Error rendered inline (soft-fail, never
`AppMode::Error`). Compose (new issue: first line = title; comment: body)
uses `App.issue_editor`; `Ctrl+E` in any compose (issue + PR) pops out to
`$VISUAL`/`$EDITOR` via `src/external_edit.rs` — App only records intent,
main.rs owns terminal suspend/resume, debug/headless path never suspends.

---

## Remotes & Push

### [DONE] 2026-07-19 Remote choice on push
Push (`P`) now opens the remote picker whenever the repo has 2+ remotes — even
when HEAD already has an upstream — with the selection defaulting to the upstream
remote (falls back to the first remote). Single-remote and zero-remote behavior
is unchanged. Choosing remote R pushes HEAD to R: the configured upstream → a
plain `git push`; any other remote → `git push R HEAD` (no `-u`, upstream
tracking untouched); a branch with no upstream still publishes (`push -u`). New
`PushSpec::ToRemote` + `push_head_to_remote()`; `remote_ops::run_push_to_remote`
picks the mode; pure, unit-tested `remote_picker_default()` for the default
selection. Fetch/pull/prune pickers unchanged (they only surface when there's no
upstream to disambiguate, so their default is naturally the first remote).

### [DONE] 2026-07-19 In-TUI credential prompt with paste (issue #33)
An HTTPS auth failure on a push/fetch/pull no longer dead-ends in the error
popup: keifu prompts for a username (prefilled from the URL's `user@` or a
previous entry) then a **masked** password/token, caches them per host for the
session, and retries the same op automatically. Credentials reach the child git
via a `GIT_ASKPASS` shim (`src/git/askpass.rs`, mode 0700 in the temp dir, echoes
`KEIFU_ASKPASS_USER`/`_PASS`) — never in argv, the URL, or on disk; the shim is
credential-free so it persists harmlessly. `GIT_TERMINAL_PROMPT=0` stays set.
Cached creds are attached transparently to later ops on the same host (asked once
per session); a retry that still fails auth drops the stale creds and re-prompts
(prefilled), capped so it can't loop. Detection is a pure, unit-tested predicate
`is_https_auth_failure()` (HTTPS `could not read Username` / `Authentication
failed` only — SSH `publickey` failures stay plain errors) plus `extract_auth_url`
for host/user. SSH keys are untouched. Real bracketed paste
(`EnableBracketedPaste` in `tui.rs`, `Event::Paste` forwarded through
`event.rs`): `Action::InputPaste` appends a sanitized single-line chunk to any
input (control chars incl. newlines/tabs stripped), and paste routes into the
commit/PR/issue `TextEditor` too (newlines kept). Session cache + retry state on
`App` (`credentials`, `in_flight_op`, `pending_auth`); orchestration in
`src/app/credentials.rs`. Verified live: multi-remote picker default + non-
upstream push, the username→masked-password flow, masked paste (no plaintext
leak), and re-prompt-on-retry.

---

## Maintenance Sweeps

### [DONE] 2026-07-13 comprehensive perf/bug/architecture sweep
Startup: FsWatcher (recursive inotify registration, 91-94% of pre-first-frame
time) now builds on a background thread; terminal-bg OSC query overlaps repo
loading. App::new dropped 167ms -> 11ms (keifu repo), 656ms -> 48ms (135k-file
repo), dev profile. Render path: files-pane/commit-detail no longer rebuilt
multiple times per frame; per-row Local::now() and Vec<char> allocations
removed; build_graph O(N^2) scans replaced with map lookups. Bugs fixed (with
regression tests): PrevFile OOB panic with partially-staged files, empty-pane
refresh_after_file_op panic, orphan detached HEAD missing from graph,
NetworkManager stuck on dead worker. Deps: openssl-sys removed entirely
(git2 default-features off — network transports unused), thiserror removed.
app.rs split into src/app/ modules.

**Future startup work (not currently worth it):** get_commits + status scan +
build_graph are the remaining ~48ms on huge repos; could render first frame
before status scan, or cap initial walk at ~100 commits and extend lazily.
**Other deferred items:** notify 6->8 bump (breaking API); scope the fs watch
to .git + non-ignored dirs (cuts inotify watch count 10-100x, matters near
fs.inotify.max_user_watches); rename ui::files_pane::FilesPaneState to avoid
collision with crate::files_pane_state::FilesPaneState; UiState::save silently
ignores IO errors.

### [DONE] 2026-07-13 test suite hardening (audit + implementation)
347 -> 439 tests. Plugged zero-coverage destructive ops (rebase, all stash
ops, remote checkout, restore-untracked/trash), confirm->operation dispatch
(merge/rebase/cherry-pick/revert/reset x3/stash-drop), undo direction logic,
commit-menu construction, graph edge cases (octopus, stash nodes, uncommitted
lane collision, orphan roots), config parsing, unicode editor edges. Fixed 6
vacuous/tautological tests, removed 1 duplicate, rewrote 2 transport-coupled
diff-cache tests to observable contracts. Conflict-stranding behavior of
merge/rebase/cherry-pick/revert pinned with "documents current behavior"
tests (baseline for the merge-conflict feature work, see vscode-parity.md).
Shared tests/common harness; removed 1.1s sleep (suite: 1.2s -> 0.2s).

### [DONE] 2026-07-13 parity-gap implementation sweep
Closed the top gaps from `docs/vscode-parity.md` across 6 feature branches,
merged into `parity-gaps`: real branch filtering (hidden branches drop their
exclusive commits, not just labels) + tags rendered as graph refs (43f4f8d);
merge-conflict awareness with accept-ours/theirs and abort/continue
(05c2242); hunk-level stage/unstage/discard plus stage-all/unstage-all
(138ae73); branch rename, tag delete/push, stash-all and stash-branch, copy
file path (9a9cc1c); pull, multi-remote resolution, upstream tracking and
one-key publish (2e14a4c); compare-two-commits, per-file history, and commit
signature status (3d707f1). Test suite: 439 -> 530 tests, clippy clean.
Followed by a docs-coherence pass: audited `help_popup.rs` against
`keybindings.rs` for every new action (fixed a stale Tab/`]`/`[` mislabel
inherited from before the panel system, a "q quits" claim that was never
true, a misleading in-progress-operation hint shown in status_bar.rs outside
the files panel where the keys actually work, and added missing entries for
folder-toggle and commit-filter); refreshed README.md/README_JA.md and
vscode-parity.md to match current behavior.

### [DONE] 2026-07-19 OSC 52 clipboard fallback
`copy_to_clipboard` still tries xclip/xsel/wl-copy/pbcopy first; if none is
found, falls back to `tui::copy_to_clipboard_osc52`, which writes
`\x1b]52;c;<base64>\x07` straight to stdout (works headless/over SSH, no
external binary). Base64 is a small hand-rolled encoder, not a new
dependency. Payload capped at 100,000 base64 chars (typical terminal limit),
truncated on a 3-byte boundary if oversized. Status-line messages append
"(via OSC 52[, truncated])" when the fallback fires, unchanged otherwise.
See `docs/architecture.md` "Clipboard via Shell Commands" for details.

### [DONE] 2026-07-19 Diff viewer soft line-wrap toggle
`Ctrl+Alt+W` in the full-screen file-diff viewer (`AppMode::FileDiff`) toggles
soft word-wrapping of long lines (default off: horizontal truncation/scroll,
unchanged). Wrapping breaks at whitespace where possible and hard-breaks tokens
longer than the pane width. The gutter (line numbers + change prefix) renders
only on the first row of a wrapped line; continuation rows pad the gutter width
and keep each span's syntax/diff-background style. Scrolling, the scrollbar, and
hunk navigation/staging all operate on wrapped-row coordinates (hunk-header
positions are re-mapped into wrapped space, so hunk ops keep working while
wrapped). State persists in `UiState.diff_word_wrap`. The pure wrapping math
(`wrap_offsets`, `source_row_starts`, `layout_diff_rows`, `DiffRow::wrap`) lives
in `ui/file_diff_view.rs` with unit tests (word-boundary + unbreakable-token
cases). Source rows are held on `App.diff_source` beside the mode (like
`diff_viewport_*`) to avoid bloating the AppMode enum. Debug harness gained a
`<c-a-w>` (Ctrl+Alt) key token for headless verification.

### [DONE] 2026-07-19 Merge-conflict UX batch (issue #36)
Three tracks: (1) stash pop/apply through `run_git_allow_conflict` → typed
`OpOutcome::Conflicts` guided flow (stash kept on conflict, no continue step,
op_state stays Clean; guidance points at the stash-menu Drop, not the merge
Continue/Abort keys); (2) app-level guardrails while an operation is in
progress / conflicts outstanding — checkout, merge/rebase/cherry-pick/revert
initiation, pull, and stash pop/apply are intercepted with a guided message
(pure predicates `op_guard_message`/`commit_guard_message` in
`app/conflict_actions.rs`); commit is blocked only while unmerged paths remain,
so resolving re-enables it; (3) conflict navigation — `]`/`[` in the files pane
jump between conflicted files (wrap-around), literal conflict-marker lines are
highlighted in diffs (`src/conflict.rs`). In-viewer conflict-block jumping was
deliberately dropped: libgit2 emits no hunks for unmerged paths, so marker
content never reaches the diff viewer during a live conflict (a 3-way/conflict
view remains the XL follow-up in docs/vscode-parity.md).

### [DONE] 2026-07-19 Popup chrome polish
All popups route through `Theme::popup_block` (rounded borders matching the
panes, bold border-colored title, one column of horizontal padding); the help
sheet is data-driven (`HelpEntry`) with a computed fixed key column (fixes the
"Tab / S-Tab" collision); Commit Detail gets the same one-space inset plus
muted field labels; empty states show muted placeholders ("no changes",
"empty commit", "no matching branches" — the branch-filter popup also no
longer collapses to zero body rows when nothing matches).

### [DONE] 2026-07-19 Issues enhancement batch (full-screen + filtering)
Reworked the GitHub Issues feature (issue #37 follow-up). (1) Full-screen views:
`AppMode::IssueList`/`IssueDetail` now render full-screen (content + 1-row status
bar, early return in `ui::draw` via `draw_issue_screen`, mirroring `FileDiff`)
instead of an 80% popup; compose / label-picker / label-filter draw the relevant
issue view as a backdrop with a centered overlay on top (backdrop = detail when a
detail popup is live, else the list). The old `ISSUE_POPUP_PCT` popup arms and the
detail scroll pre-pass were removed; scroll clamping happens in the new
full-screen path. (2) Filtering via a pure, unit-tested predicate
(`issue::visible_issues` / `issue_matches`) shared by the widget and the
selection/navigation handlers — `selected` now indexes the *visible* rows and is
re-clamped whenever a filter shrinks the set. Status filter (open/closed/all)
stays on `f`/Tab; `t` opens a label-filter checkbox picker
(`AppMode::IssueLabelFilter`, Space toggle, ^a/^o all/none, Enter apply); `u`
toggles unblocked-only. (3) Blocking data is sourced from GitHub's native issue
dependencies via `gh api graphql` (`blockedBy` field, resolved through the repo's
`nameWithOwner`), *combined* with body-parsed references (`blocked by #N`,
`depends on #N`, `- [ ] #N`) to other open issues — both live in pure functions
(`parse_blocked`, `blockers_in_body`, `compute_blocked_set`) with unit tests
(self-refs, closed blockers, cross-repo refs ignored, malformed bodies). The
fetch runs in the existing background style (`IssueFetch::start_blocked` /
`poll_blocked`), degrading to "all unblocked" if unavailable. (4) `l` in the list
opens the label picker for the selected issue (was detail-only), returning to
whichever view it was opened from. (5) Aesthetics: list header (repo + active
filters + shown/total), aligned rows (colored state glyph ● open / ✓ closed
purple, right-aligned number, ⛔ blocked marker, truncated title, colored label
chips, relative updated time, muted author, theme selection highlight); detail
gets colored label chips (shared `hex_to_color`), muted field captions, comments
with an author + relative-time header and indented body; "no issues match
filters" empty state. Added a `Theme::issue_closed` (purple) color and a
relative-time helper (`issue::relative_time`). Status-bar hints and the
data-driven Help sheet updated with the new keys.

### [DONE] 2026-07-19 Folder view shows basenames
In the files pane's folder-grouping view (`f`), file rows under a folder header
now display just the basename (`main.rs` under `src/`) instead of repeating the
full repo-relative path. Display-only: `FileDiffInfo.path` is untouched (staging
and diff lookups still key on the full path). A pure helper
`is_under_folder_header` (`files_pane_state.rs`, unit-tested: nested files,
root files, grouping off, `SectionHeader` reset between staged/unstaged
sections) drives the choice in `ui/files_pane.rs`.

### [DONE] 2026-07-19 Star gap under commit filter
The "sometimes a small gap beneath the star" report: with a Ctrl+F commit
filter active that excluded HEAD's message, HEAD's row was hidden while the
always-visible uncommitted row kept its connector (wired against the unfiltered
node list at build time), leaving a dangling stub under the top marker.
`node_passes_commit_filter` now always keeps the HEAD row whenever an
uncommitted node exists. The pure-geometry hypothesis was disproved and
codified as a raster regression test (star row tiles seamlessly across 10+
font metrics).

### [DONE] 2026-07-19 S-curve pixel graph edges
Lane transitions in the pixel graph are now full VSCode-style cubic S-curves
instead of straight horizontal runs with a small rounded corner. A pure
row-local scan (`transition_curves`) reconstructs each horizontal run into
hub→spoke cubics (dots/Tee arms = horizontal tangent; Merge/Branch/TeeUp
risers = vertical tangent at row edges); verticals and dots draw on top so
HorizontalPipe crossings and trace dim/bright layering are unchanged. Curve
endpoints stay at exact lane centers on row edges (unit-tested) so rows tile
seamlessly. Unicode mode untouched. See docs/architecture.md.

### [DONE] 2026-07-19 Traced fork fan no longer floods sibling arms
User report: tracing a commit on one branch painted that branch's color over
sibling arms' lead-ins in shared fork fans (bright layer composited over dim).
Untraced rendering was verified correct (75+ fuzzed topologies). Fix:
`draw_curve_shaded` in `ui/graph_pixels.rs` shades curve sub-segments by the
underlying cell column — but only inside the trunk corridor near mid-height
(|y-cy| <= stroke width) where arms genuinely overlap; the elevated part of a
sweep keeps its own arm color. Regression tests cover both regimes (dim sibling
keeps its color at the trunk; elevated arm keeps its own color over a sibling's
column, traced and untraced). raster_debug gained FOLD/NODIM/underlay options
for pixel-mode repros.

### [DONE] 2026-07-19 Trace bleed: cross-layer curve overlap (real root cause)
The traced-branch color appearing to "lead into" other branches survived two
prior fixes because the colliding curves were never in the same reconstruction
run: the bright traced arm lives in the row's own `cells` while the dim
sibling's lead-in lives in the folded `underlay` — two `transition_curves`
passes over the same columns, both dead-flat at mid-height near the shared dot,
so bright-over-dim compositing erased the dim stroke. Fix per user direction:
per-column corridor shading removed (flat per-arm colors restored); instead
every hub→spoke cubic's hub handle leans toward its spoke (`HUB_TILT` +
`FAN_EXTRA` for same-run fans), so curves sharing a dot diverge immediately,
whether same-run or split across cells/underlay. Endpoints unchanged (tiling +
protocol cache unaffected). raster_debug now folds `cell_oids` and dims the
underlay app-faithfully. Regression tests: cross-layer bury case, fan bury
case, hub-handle geometry.

### [DONE] 2026-07-19 Trace identity: co-routed edge no longer recolors sibling lead-ins
The final piece of the trace-bleed saga (user: "the color was actually correct
before, the lead-ins of other branches just need to be dimmed"). Fork
connectors co-route a farther merging lane's edge through a nearer arm's `┴`
cell (secondary oid slot) — correct data for the old straight-run renderer
where one stroke served both branches, but `apply_trace_dim` lit and recolored
a non-pipe cell if EITHER edge was traced, so a sibling's lead-in rendered
bright in the traced branch's color. Non-pipe, non-dot cells now dim/recolor
from their primary (own) edge only; HorizontalPipe keeps two-channel handling;
dots still light via either edge. Regression test modeled cell-for-cell on the
real junction. Note: the Unicode renderer still lights via either edge (a
single glyph can't split colors) — acceptable there.

### [DONE] 2026-07-20 Trunk-merge connectors no longer render dead-flat
A commit whose parent stays on the trunk (the `was_existing && !already_shown`
case → a `TeeRight`/`TeeLeft` on the trunk lane) rendered as a completely
horizontal line: `transition_curves` classified every Tee as a mid-height hub,
so the run was dot(mid)→Tee(mid) with zero vertical rise. Reported as "merges
of the trunk into other branches are completely horizontal." Fix: a Tee flanked
by a commit dot is that commit's parent-connector into a descending trunk, so
its arm now sweeps DOWN to the bottom edge (a spoke), mirroring how `Merge*`
sweeps up to a parent above; a Tee with no flanking dot stays a mid-height hub
(fork-connector trunk, up-arms still fan). Pixel mode only; endpoints still land
on lane centers (tiling + protocol cache unaffected). keifu's own history had 0
such rows so it was invisible here — reproduced against a repo with trunk-heavy
merges. Regression tests: dot↔Tee turns down; fork-connector Tee stays a hub.
Blast radius: stroke geometry in `transition_curves` only (sole caller
`draw_cells`); no cell/layout/cache/Unicode-path changes.

---

## 2026-07-20 issue sweep

~20 branches merged into `chong-dev`, closing issues #40-#64. One line per issue below; see `docs/architecture.md` for the subsystems that grew out of this batch (toasts/episode latching, settings registry, merged-branch classification, lane-0 HEAD, PrContext, windowed rendering, status-bar chips, repo-handle reopen, bezier curves).

### [DONE] #40 Restore refresh
Stale full-diff rows are pruned on restore so restored files vanish immediately instead of lingering in the cache.

### [DONE] #41 / #60 Merged-branch dim/hide, including squash
Merged branches dim by default and `Shift+H` hides them; merge detection covers squash-merges (bounded patch-id scan since fork point) in addition to ancestor merges, cross-checked against `gh`'s merged-PR list.

### [DONE] #42 / #43 PR badge on head + review-state glyphs
Open PRs get one badge on their head commit (not every commit in range), plus review-state glyphs.

### [DONE] #44 / #49 Toast sweep + episode latching
Transient notifications moved off the status bar into a toast queue; periodic background errors (e.g. refresh failures) now latch once per episode instead of re-reporting every poll.

### [DONE] #45 Lane-0 HEAD invariant
The checked-out HEAD's line always occupies lane 0.

### [DONE] #46 Remote-tracking merge/rebase
Merge/rebase of remote-tracking branches works correctly (regression coverage for a stale-ref bug).

### [DONE] #47 / #48 Fetch --prune + reopen-on-refresh
Fetch prunes stale remote-tracking refs; refresh reopens the repo handle so external pushes/fetches are observed.

### [DONE] #50 / #53 Badge order/color
Branch badge order is stable across refreshes; badge color matches its lane color.

### [DONE] #51 Windowed rendering (two phases)
Text-layer item building windowed to the viewport+margin (15.3ms -> ~4ms/keypress at 5.6k nodes); pixel specs cache an undimmed base keyed without trace state and dim only the visible protocol window (~15ms -> 1.24ms/keypress at 5.2k nodes).

### [DONE] #52 Grey PR merges
PR merge commits (second parent matches the PR's head OID, with a message-format fallback) render in grey.

### [DONE] #54 Help Shift labels
Help popup lists all Shift-modified bindings with consistent labels.

### [DONE] #55 / #59 Mute base-update merges + collapse merge messages
Merges that only bring a branch up to date with its base are muted; merge commit messages collapse to a glyph.

### [DONE] #56 Settings menu
`Ctrl+,` opens a settings menu with live apply and persisted values, backed by the new settings registry.

### [DONE] #57 Remotes-hidden cap at local tip
Hiding remote-only branches no longer leaks remote-only commits past the local tip.

### [DONE] #58 Same-lane Ctrl+Up/Down
`Ctrl+Up`/`Ctrl+Down` jump along the same graph lane.

### [DONE] #61 Right-click retarget
Right-click retargets the commit options menu to the clicked commit instead of the previous selection.

### [DONE] #62 Chips
Status-bar chips surface persistent state: compare mode pending/range, watcher disconnected.

### [DONE] #63 Bezier curves
Lane transitions render as VSCode-style vertical-tangent bezier connectors, replacing the tangent hub/spoke model.

### [DONE] #64 Keyboard enhancement + ',' fallback
Kitty keyboard protocol enabled for reachable Ctrl+, chords, with a plain ',' fallback when the protocol isn't available.

### [DONE] #75 Full-height S-curves
Lane-transition beziers span row-center to row-center (VSCode geometry): curves leave commit dots immediately with no straight stub in the adjacent row and no curvature kink at the row edge. Each row draws its clipped half of the shared cubic; RowSpec carries the neighbor rows' boundary-crossing curves.

### [DONE] #76 Scroll latency batch
Input coalescing (drain buffered nav events per frame), trace lineage/lit-edge cache keyed by (generation, selection), pixel sync window shrunk to viewport+margin while trace dim is active, .archive/ walk cached out of the draw path.

### [DONE] #77 Pixel window stale-offset cutoff
The pixel spec/protocol pass ran before the list render, so its sync window used the pre-clamp scroll offset — page jumps (and G/g even before the lean trace window) left whole bands of the graph blank at the top/bottom. The pass now runs after the list render on the final offset; a mid-frame protocol poisoning triggers an immediate redraw for the Unicode fallback.

### [DONE] #78 Async startup merged-branch classification
Merged-branch classification (ancestry + patch-id squash scans, >1s on branchy repos) ran synchronously in App::new. Now: synchronous only when hide-merged is on (async fill-in would flash hidden branches); dim-only mode starts unclassified and the background classifier — kicked at init — dims merged branches moments after the first frame.

### [DONE] #79 Traced re-encode set: measured minimal; LRU protocol cache
Measured (via new KEIFU_FORCE_PIXEL + encode-count logging): with tracing on, a selection move re-encodes only the 0-5 rows whose lit-state changed — the RowSpec-keyed protocol cache already restricts the set. Shipped the real gap found while measuring: the cache's at-cap prune nuked everything but the current frame (full re-encode when scrolling back); it now evicts the least-recently-used half.

### [DONE] #80 Perf gates
Permanent startup phase timings (startup.* ops in the exit perf summary) and a perf regression test suite: wall-clock budgets (~10-30x measured, catching algorithmic blowups) for startup and window rasterization, plus an instrumentation contract test. Deterministic counter tests (e.g. #78's no-sync-classification gate) remain the primary absolute gates.

### [DONE] #81 Squash-merge origin link line
Option: subtle grey linking line from a squash-merged branch's tip to the squash commit on the base. Depends on #82's classification data.

### [DONE] #82 Squash-merged branches not hidden
Bug: branches merged via squash aren't being hidden by hide-merged. Investigate patch-id + gh-signal classification paths.

### [DONE] #83 Remote-ahead-of-local display question
Verified: with hide-remotes OFF, a remote branch ahead of its local shows its extra commits on their own rows with a cloud chip, and is navigable (dedup is per-node, so only same-tip local/remote pairs collapse to the synced chip). With hide-remotes ON, ahead-remote commits are hidden deliberately (#57's cap-at-local-tip). No change needed.

### [DONE] #84 F5 fast-forwards non-divergent locals (option)
Option: refresh (F5) also fast-forwards local branches that are strictly behind their upstream (no divergence).

### [DONE] #85 Toasts bottom-right
Move toast stack to the bottom-right corner.

### [DONE] #86 "7-shaped" corner artifacts on fork rows
Pixel graph: little 7-shaped sections at lower corners when many lines come from the same parent (fork connectors). Round 1: length-based tessellation + boundary-crossing handle extension. Round 2 (user repro at small cell sizes): the elbow radius collapses quadratically with cell height, so dot-anchored ends of wide arms now tilt their tangent toward the far end (the dot hides the junction); pipe-joining ends stay strictly vertical for seam tiling. Cross-row spoke_on_dot flags keep both halves of each cubic identical.

### [DONE] #87 Input latency diagnostics
MacBook still laggy with tracing off; keypresses appear to queue and "catch up" on the next press. Add event→action→draw timing logs to diagnose.

### [DONE] #88 Ctrl+Enter deletes local+remote branch
Branch delete confirm: third option (Ctrl+Enter) that also deletes the remote branch when one exists.

### [DONE] #89 Optimistic remote branch deletion
Remote branch deletion updates the UI immediately and reconciles on failure, instead of blocking on the remote round-trip.

### [DONE] #86-r3 Folded-connector pipe overshoots a curve-fed join
Round 3 (user repro on keifu's own graph): a lane that terminates where a wide arm merges in shows a "/|" — the arm's tilted-looking tip plus an orphan vertical stub dangling half a cell below the join. Not a tilt regression (renders identically pre-3eecd60): the folded connector's vertical (e.g. HorizontalPipe under a dot) shadows the host cell's lane segment, but its curved_below is computed against the host row's own cells instead of the row-below view, so it never learns the bottom half is curve-fed and draws full height past the join.

### [TODO] #90 Remote-counterpart disambiguation
Follow-up from #88's adversarial review: when a branch has no upstream and exists on multiple remotes, the name-match fallback picks by list order; prefer the configured push remote and only trust an upstream whose short name matches. Low risk (target is displayed and secondary-key gated), but worth tightening.

### [DONE] #91 Hide merged branches appears to do nothing
Bug: toggling hide-merged has no visible effect. Investigate the setting → classification → graph filter data flow.

### [DONE] #92 Merge commits not muted enough
Merge commit rows should read as grey (subject/author/date), not just slightly dimmed.

### [DONE] #93 Ctrl+Up at lane top jumps to merge target
When lane navigation hits the top of a lane, Ctrl+Up should follow the merge edge into the branch the lane was merged INTO.

### [DONE] #94 Trace toggle becomes a setting
`t` trace toggle should be a settings entry; remove its indicator from the status bar.

### [DONE] #95 Push feedback via toast
Push outcome should report as a toast, not occupy the status bar.

### [DONE] #96 Pull error (needs stash) blocks UI
A pull failing on dirty local changes keeps showing on the status bar and the UI can't be accessed until dismissed. Error should be non-blocking (toast), ideally with stash guidance.

### [DONE] #97 Squash merges onto an advanced base not classified
Bug: squash-merged branches still aren't hidden by hide-merged and draw no link line when the trunk advanced since the fork. Two independent failures, both when the base moved on near the branch's edits: (1) the local patch-id squash scan folded diff *context* into the hash, so a trunk commit editing lines adjacent to the branch's change broke the match — fixed by generating the diffs with zero context lines (`tree_diff_patch_id`), keying the id only on changed lines while still requiring the whole cumulative change set to match; (2) the GitHub-signal cross-check `branch_changes_landed` used a raw base→branch tree diff and read a file the branch was merely *behind* on as novel work — replaced with `branch_content_in_base`, a three-way merge against the fork ancestor that reports contained only when merging the branch into the base is a no-op. Both are exact/sound, not fuzzy: an unlanded branch (or a conflict-resolved landing) is never classified locally. Follow-up hardening: the collision cross-check anchors at the *matched squash commit* (the landing point), not the base tip — later trunk edits to the landed lines would three-way-conflict at the tip and wrongly un-classify a genuinely squashed branch.

### [DONE] #98 PR badges before branch pill
In graph rows, PR badges now render BEFORE the branch name pill.

### [DONE] #99 PR-styled subjects for PR-landing commits (option)
Toggleable option (default ON, `pr_subjects` in `MetadataColumns`): a commit that landed a merged PR — the PR's merge commit or its squash commit — shows an icon + PR number + title instead of the raw "Merge pull request #x from y" / "title (#x)" subject. Pure parser `pr::pr_landed_subject` extracts `(number, title)` strictly (merge: number off the subject, title = first non-blank body line after it, else `None` rather than inventing one; squash: strict `<title> (#n)` suffix, exactly one space before the paren, nothing after). Rendered in `render_graph_line_tail`; `collapse_merges` wins when both would apply to the same row.

### [DONE] #100 Squash hide/link-lines still failing on real repos
User repro persisted after #97. Built a wild-repo investigation test (`tests/squash_real_world_test.rs`, `#[ignore]`, run with `--ignored`): clones casey/just, fetches the surviving `refs/pull/N/head` refs of recently merged squash PRs, and classifies them. It decisively isolated the cause: **12/12 PR heads classified when materialised as LOCAL branches, but 0/12 as REMOTE branches** (`origin/feature`). Root cause (H1): `branch_is_merged` early-returned `false` for `b.is_remote`, so after a GitHub squash-merge — where the surviving ref is typically the *remote* branch (kept on the remote, or only ever fetched) — the branch was never hidden and never got a #81 link line. Fix: classify remote branches too (removed the `is_remote` skip), keeping the trunk's own remote ref safe via the existing base-tip guard (`base_tips` always carries `origin/<base>` when distinct), and added `gh_key()` to match the GitHub merged-PR set by the remote-prefix-stripped name (`origin/feature` → `feature`). Offline fixtures added for every exposed shape: remote-only squash classification, remote trunk never classified, gh-by-stripped-name, remote name-reuse safety, and (H2) a squash still detected after the advanced base was back-merged into the PR branch ("Update branch"). Other hypotheses ruled out by the wild suite: local classification, update-merges (H2), scan caps (H3, recent PRs well within `SQUASH_SCAN_LIMIT`=400), and base selection (H4, `origin/<base>` reach from #82) all work. H5: #81 link lines sit behind `ui.squash_link_lines` (default OFF) — confirmed working end-to-end once enabled (verified via debug-tui: hide-merged removes the squash branch; enabling the option draws the grey curve from the branch tip to its squash commit) and the H1 fix populates the link target for remote branches too; the default is left OFF (a UX choice for the maintainer).

### [DONE] #101 PR subjects: drop the number
Rewritten PR subjects show icon + title only — the parsed number is sometimes an issue reference, and the number adds noise anyway.

### [TODO] #102 graph_view.rs decomposition (tracked as GH issues #75-#81)
Investigation confirmed the god-module drift: 4172 lines, a 318-line render_graph_line_tail reading all 13 RowRenderCtx fields, ~40 tests asserting by scanning rendered spans. Seven-item split plan filed as GitHub issues: metrics (#75), geometry (#76), BranchChip/chips (#77), PrBadge/badges (#78), row-folding (#79), pixel-dim (#80), and the capstone pure-RowModel seam (#81). Order: #75/#76/#79 → #77/#78 → #80 → #81.

### [DONE] #103 Trunk-aware merged classification + --explain-merged diagnostics
Found via the new `keifu --explain-merged` trace (added here) run on keifu's own repo: classification measured only against main/master (+origin mirror), so on a repo whose working trunk is a long-lived branch (chong-dev), nothing ever classified — 0 branches. The checked-out branch's tip is now an additional trunk tip ("merged = landed in the trunk or the line you're on"); guards keep main/HEAD themselves unclassifiable. keifu repo: 47 branches now classify. Also added the exact reported user shape (local survivor, remote branch deleted, squash on drifted origin/main) as a passing fixture — the remaining real-repo gap, if any, is diagnosable with --explain-merged output.

### [DONE] #104 Startup blocks on synchronous merged classification
In branchy repos with hide-merged on, startup ran the full patch-id classification synchronously (pricier post-#100/#103), freezing time-to-first-frame. Now persisted to disk: a per-repo cache (`<config>/keifu/merged_cache/<repo-hash>.json`) stores the (merged-set, squash-targets) result under a SIGNATURE — `ClassifyInput::signature()` over all (branch, tip) pairs + base name/tip + gh-merged set (reused from the async guard). Startup with hide-merged on: an exact-signature hit is applied synchronously (instant, correct); a stale/absent entry paints its last-known (or empty) result WITHOUT blocking and kicks the async classifier to reconcile (brief flash of soon-to-hide branches is the accepted tradeoff). Fresh results are written back on every async delivery (`update_merged_classification`), tagged with the signature+gh set of the input that produced them (read off the classifier, not live state, so the entry can't be poisoned). `merged.rs` stays pure — the cache lives at the app/persistence layer. Gates: deterministic tests that a cold/absent cache defers classification and a matching cache applies synchronously, a stale-cache reconcile test, and a cold-startup wall-clock budget on a 40-branch fixture (`tests/merged_cache_startup_test.rs`); the pre-existing perf suite missed this because its fixture didn't enable hide-merged.

### [DONE] #105 Branch behind its upstream misclassified as merged
A local branch strictly behind its own upstream (dev behind origin/dev) is a stale tracking ref, not landed work — must never classify as merged.

### [DONE] #106 Mute-merged toggle ignored for squash-merged branches
Squash-merged branches render greyed regardless of the mute/dim merged-branches setting.

### [DONE] #107 Remote mirror behind its local counterpart misclassified as merged
Symmetric to #105: origin/<branch> lagging a local branch with unpushed commits (e.g. origin/chong-dev behind the checked-out chong-dev) read as ancestry-"merged into the line you're on" and got dimmed. Stale-tracking guard now covers both directions.

### [DONE] #109 Working trunk's remote counterpart is a trunk tip

### [DONE] #110 Push is a branch-level action, not a commit-menu option (GH #87)
Push shouldn't appear in the commit menu (and never for remote-only branches). Make it an app-level action available when the checked-out branch is ahead of its remote.

### [DONE] #111 PR badge color reflects CI status (GH #88)
Four states: CI failed (red), CI running (orange), CI passed but merge-blocked e.g. changes requested (green-yellow), CI passed and mergeable (full green).

### [DONE] #112 Detached HEAD gets no star in the graph (GH #89)
The HEAD marker doesn't render when HEAD is detached.

### [DONE] #113 Merged-branch badges keep their color, muted (GH #90)
Lane dimming (#108) should keep colored branch chips on merged branches, just with a muted color.

### [DONE] #114 Fetch-all-remotes sometimes misses remotes (GH #91)
Observed with a remote ahead of a non-checked-out local branch. Audit remote enumeration, refspecs, and per-remote error handling.
The real-repo squash failure, reproduced live via PR #84 on keifu itself: a squash PR against the working trunk lands on origin/<head> while the local head lags until the next pull — and base_tips never tested origin/<head>, so the landed branch stayed visible (gh signal fired but containment had no tip containing the squash). The checked-out branch's upstream (or origin/<name>) tip is now a trunk tip; being a tip also protects it from classification.

### [DONE] #110 Squash link line: phantom curves / right-side detour
Bug: the #81 grey squash-link connector rendered as two curves aimed at a phantom point right of the tip — one leaving the branch tip up-and-right, one leaving the squash commit down-and-right into a void column, crossing. Root cause in `draw_squash_link` (graph.rs): the connector demanded a column empty across the *entire* span AND distinct from *both* endpoints; when the lanes between were busy at any intermediate row, the only qualifying free column was `max_lane + 1` — to the right of the tip — so the link detoured out and back. Fix: allow the connector to ride an **endpoint's own lane**, anchoring on that endpoint's commit dot (the elbow lands on the far endpoint; the near end runs straight out of the dot, exactly how a merge/fork connector routes). A candidate column now need only be free at the intermediate rows and at each endpoint row *unless* it is that endpoint's lane. The nearest-usable pick then prefers a between/on-endpoint column, keeping the link hugging lane geometry with no detour, no void, no crossing — one continuous connector. Shared injection, so Unicode and pixel renderers both fixed. Repro'd + regression-tested at cell level (single-column spine, no grey right of the tip, tip dot-anchored with the grey pipe straight above it) in graph.rs; verified live via the debug server on keifu's own three squash fixture branches.

### [DONE] #108 Dim merged branches dims the whole lane
"Dim merged branches" dims everything a merged branch contributes — its exclusive commits' rows (message + metadata) and the graph dots/lines — in both renderers, gated on the existing setting; instant toggle, composes with trace dim. Landed via PR #86. Also fixed latent pixel bug: the trace pass dimmed every cell when tracing was off.

### [DONE] #116 Errors are red toasts, never a blocking modal
User request: error messages showed in the status bar via the input-swallowing `AppMode::Error` modal — the UI was locked until dismissed. Now `App::show_error` pushes an Error toast (red, ✗): mode stays Normal, input keeps working, TTL raised 8s → 12s so the only error surface isn't missed, and Esc dismisses lingering error toasts before taking its usual quit/cancel meaning (info/success toasts never intercept Esc — they expire in 4s and swallowing Esc for them would make quit feel unreliable). `AppMode::Error` is deleted outright (enum variant, keybinding map, status-bar arms, debug-server state string); sticky state (conflict guidance, latched background-check errors, network progress) stays on the status bar as designed.

### [DONE] #115 Merge-into-feature Tee dims the live trunk segment
(Numbering note: #110–#114 were double-booked by two parallel work streams; continuing from #115.)
User repro (screenshot): where the trunk was merged INTO a feature branch that later landed (a dimmed lane), a segment of the live trunk's own line rendered grey. Root cause: the ├/┤ Tee marker on the trunk lane composes TWO strokes — the lane's straight through-line and the connector arm curve — but `build_row_cells_with_colors` overwrote the cell's edges with just the arm's, and the dim/trace passes styled the whole cell from that one edge; the arm touches the dimmed merge commit, so the trunk's through-line dimmed with it. Fix: the Tee marker keeps the replaced Pipe's own edge as the cell's secondary; `dim` now styles the trunk from the lane edge and `dim_secondary` styles the arm (mirroring `HorizontalPipe`'s two-stroke split) across merged-lane dim, trace dim, `run_style`, and the cross-row incoming-tail restyle. Pure ╰/╯ corners keep single-edge semantics (no trunk). Unicode renderer: the Tee glyph follows the trunk edge (one glyph can't split strokes). Fork-connector hub Tees have no lane edge and behave as before.

### [DONE] #112 Behind on the trunk line is not "merged"
User repro: branches on the same trunk line as the checked-out branch — tips pointing at *older commits of a line already in view* — classified as "merged" and got hidden/dimmed. They are merely behind. Root cause: #103 made HEAD's tip a trunk tip, so any ref whose tip is an ancestor of it ancestry-classifies; the #105/#107 guards only cover upstream-tracking staleness. Rule now enforced: the trunk tips' first-parent chains are collected once per classification (`trunk_first_parent_line`, bounded 2000/tip), and a tip IN that set is refused by every signal, gh included (for an on-line tip the containment cross-check is trivially true, so the PR name alone would have decided). A genuinely merged branch's tip hangs off the line — a merge commit's second parent, or a squash's unshared commits — never on it. Semantics change: FF-merged/stale-pointer branches on the trunk line no longer hide/dim (they are labels on visible commits); three fixtures that encoded the old reading were reworked to land via real merge commits. Verified live on keifu's own repo via --explain-merged.

### [DONE] #111 Dim mirrors hide by construction
User repro: dim only worked for squash-merged branches with surviving refs; merge-commit PR lanes (branch deleted on merge) stayed bright though hide removed them. The lane set walked from classified merged tips — refs that no longer exist can't seed a walk. Now it IS hide's semantics: loaded commits ∖ first-parent chains of live refs (+HEAD, +stashes) — no merged tips needed, deleted-branch side lanes dim, classified-or-not.
