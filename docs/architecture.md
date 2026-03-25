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
