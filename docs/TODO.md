# Remaining Feature Work

Items below are from the feature plan started on 2026-03-25, organized by area.
Items marked **[DONE]** were implemented in the `feat/panel-system-and-features` branch.

---

## Graph Pane

### [DONE] Commit Options Menu
Enter on a commit opens a full options menu with: checkout, create branch, merge into current branch (if at branch head), cherry-pick, rebase, reset (soft/mixed/hard), add tag, revert, copy commit hash to clipboard, push (if at branch head).

### Branch Select/Deselect with Filter
**Status: Not started** | Priority: P2

Add ability to select/deselect branches in the graph view, together with select all and select none. Typing filters the branch list.

**Implementation plan:**
- Add `hidden_branches: HashSet<String>` to `App` state
- Add `AppMode::BranchFilter` with: filter text, selected index, full branch list
- Open via a keybinding (e.g. `B` in graph pane)
- Popup shows all branches with checkboxes (toggled with Space)
- `a` selects all, `n` deselects all, typing filters the list
- On exit, pass filtered `branches` list to `build_graph`
- For full commit filtering: use git2 revwalk from selected branch tips only
  (currently only branch label hiding is straightforward; full commit filtering
  requires changing `get_commits()` to accept a set of branch tips)

---

## Files Pane

### [DONE] Stage/Unstage with `s` Key
When the uncommitted files node is selected and user is in FileSelect mode, pressing `s` stages/unstages the selected file. Files are divided by staged/unstaged sections.

### [DONE] Instant File Display
Files and their M/A/D status show instantly via a synchronous quick scan. Line numbers (+X/-Y) show "..." while the full diff loads asynchronously.

### Folder View with `f` Key
**Status: Not started** | Priority: P2

Pressing `f` in the files panel arranges files by folder hierarchy. Staging a folder header stages all files in that folder.

**Implementation plan:**
- Add `files_group_by_folder: bool` toggle to `App`
- `f` key in files panel toggles the flag
- `files_pane_items()` groups files under folder path headers when enabled
- `FilesPaneItem::FolderHeader(String)` variant for folder nodes
- Staging on a folder header iterates its children and stages each
- Tree-style indentation for nested folders

### Fuzzy Filter Typing in Files Panel
**Status: Not started** | Priority: P2

Typing in the changed files panel uses fuzzy matching to filter the file list.

**Implementation plan:**
- Add `files_filter: String` to `App`
- When files panel is focused and a letter key is pressed, append to filter
- Backspace removes last character, Esc clears filter
- `files_pane_items()` filters results by fuzzy match against file path
- Show the filter string in the panel title (e.g. " Changed Files [filter: foo] ")
- Use the existing `fuzzy-matcher` crate for scoring

---

## Commit Pane

### [DONE] Full Text Editor with Micro-like Keybindings
Typable commit message when uncommitted node is selected. Alt+Enter commits. Full micro-like editing: word navigation (Alt+arrows, word boundaries = spaces only), shift+arrows for selection, Home/End, Ctrl+Home/End for text start/end, up/down for line navigation at same column.

### [DONE] Enter to Start Editing, Esc to Stop
Must hit Enter in the commit detail panel to start editing the commit message. Esc stops editing and left/right returns to panel navigation.

### [DONE] Message Retained When Panel Loses Focus
The commit message no longer says "focus detail pane to type" when the panel loses focus - it retains whatever the user typed.

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

## Architecture Notes

### Panel Focus System
`FocusedPanel` is a field on `App`, not a new `AppMode`. This keeps `FileSelect`/`FileDiff` modes working unchanged. The keybinding router checks both `mode` and `focused_panel`.

### Commit Editor
`TextEditor` lives on `App.commit_editor` (not in an `AppMode` variant) so the message persists when focus moves away. The `editing_commit_message: bool` flag controls whether key events are routed to the editor.

### Quick Diff Cache
`quick_diff_cache` is computed synchronously when the selected diff target changes. It contains file paths and change kinds but no line statistics. The UI falls back to this cache when the full async diff hasn't completed yet.

### StageStatus Tracking
`FileDiffInfo.stage_status: Option<StageStatus>` is set during `from_working_tree()` before the merge scan. Pre-merge copies of staged/unstaged file lists are stored in `CommitDiffInfo.staged_files` and `CommitDiffInfo.unstaged_files`.
