# Feature Work

Items from the feature plan started on 2026-03-25, organized by area.
All items implemented in the `feat/panel-system-and-features` branch.

---

## Graph Pane

### [DONE] Commit Options Menu
Enter on a commit opens a full options menu with: checkout, create branch, merge into current branch (if at branch head), cherry-pick, rebase, reset (soft/mixed/hard), add tag, revert, copy commit hash to clipboard, push (if at branch head).

### [DONE] Branch Select/Deselect with Filter
Shift+B in graph pane opens branch filter popup. Space toggles branches, `a` selects all, `n` deselects all, typing filters by name. Hidden branches excluded from graph on refresh.

**Update:** Now performs *real* commit filtering — `get_commits()` accepts the visible branch set and only walks from those tips (HEAD is always pushed too), so hiding a branch removes its exclusive commits from the graph, not just the labels. Commits reachable from a visible branch stay. Hiding every branch still shows HEAD's history.

### [DONE] Tags Rendered as Graph Refs
Lightweight and annotated tags (peeled to their target commit) are loaded via `repository.get_tags()`, threaded through `build_graph` onto `GraphNode.tag_names`, and rendered next to branch labels as `<tag>` in a distinct tag color (`theme.tag_label`).

---

## Files Pane

### [DONE] Stage/Unstage with `s` Key
When the uncommitted files node is selected and user is in FileSelect mode, pressing `s` stages/unstages the selected file. Files are divided by staged/unstaged sections.

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
