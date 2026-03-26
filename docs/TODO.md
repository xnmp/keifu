# Feature Work

Items from the feature plan started on 2026-03-25, organized by area.
All items implemented in the `feat/panel-system-and-features` branch.

---

## Graph Pane

### [DONE] Commit Options Menu
Enter on a commit opens a full options menu with: checkout, create branch, merge into current branch (if at branch head), cherry-pick, rebase, reset (soft/mixed/hard), add tag, revert, copy commit hash to clipboard, push (if at branch head).

### [DONE] Branch Select/Deselect with Filter
Shift+B in graph pane opens branch filter popup. Space toggles branches, `a` selects all, `n` deselects all, typing filters by name. Hidden branches excluded from graph on refresh.

**Note:** Currently hides branch labels only. Full commit filtering (hiding commits not reachable from any visible branch) would require changing `get_commits()` to accept branch tips — left as future enhancement.

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
