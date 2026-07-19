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
