# Architecture Decisions

## Panel Focus System (2026-03-25)

**Decision:** `FocusedPanel` is a field on `App`, not a new `AppMode` variant.

**Why:** The existing `FileSelect` and `FileDiff` modes work as overlays on top of the graph view. If we made panel focus a mode, we'd need to handle mode transitions between `FileSelect`, `FileDiff`, and each panel focus state — an explosion of combinations. Keeping focus as a field means modes and panel focus are orthogonal.

**How it works:** The keybinding router checks both `app.mode` and `app.focused_panel` to determine which key handler to use. In `AppMode::Normal`, keys are routed to `handle_graph_action`, `handle_files_action`, or `handle_commit_detail_action` based on focused panel. Other modes (Help, Input, Confirm, CommitMenu, FileSelect, FileDiff) handle their own keys regardless of focused panel.

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
