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

## Merge-Conflict Awareness (2026-07-13)

**Decision:** A conflict is a first-class *outcome*, not an error. `merge_branch` / `rebase_branch` / `cherry_pick` / `revert_commit` return `OpOutcome::{Completed, Conflicts{count}}` and deliberately leave the repo mid-operation (conflicted index + MERGE_HEAD / REBASE_HEAD / CHERRY_PICK_HEAD / REVERT_HEAD). Callers (`app/confirm_actions.rs`) route conflicts to a guided "resolve then Continue / Abort" flow via `App::handle_op_outcome`, not the raw error popup.

**In-progress state** comes from `GitRepository::operation_state()` (`OperationState`, mapped from `git2::RepositoryState`) and `conflicted_count()` (`Status::CONFLICTED`), both refreshed in `refresh()`. `get_working_tree_status` must include `CONFLICTED` — otherwise a merge whose only change is the conflicted file leaves the uncommitted node (and its files) invisible.

**Conflicted files** carry `StageStatus::Conflicted`. An unmerged path surfaces in *both* the HEAD→index and index→workdir diffs, so both `from_working_tree` and `quick_file_list_for_working_tree` drop it from the staged side and keep one entry on the unstaged side; the files pane groups those into a "Merge Changes" section rendered first (marker `!`).

**Gotcha — rebase abort/continue must use libgit2, not the CLI.** `rebase_branch` starts the rebase via `repo.rebase()`, which writes a `.git/rebase-merge` layout *without* a `git-rebase-todo`. `git rebase --continue/--abort` then fails with "could not open '.git/rebase-merge/git-rebase-todo'". So `abort_operation`/`continue_operation` special-case `Rebase` to `Rebase::abort()` / `open_rebase()+commit()+finish()`. Merge/cherry-pick/revert use `git <op> --abort|--continue` (libgit2's merge writes a CLI-compatible MERGE_HEAD; cherry-pick/revert are CLI-driven throughout). Continue runs with `GIT_EDITOR=true` so it never blocks the TUI.

**Keys (files pane):** `o` accept ours, `t` accept theirs, `c` continue, `A` abort (behind the Confirm dialog).
