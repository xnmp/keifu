//! Keybindings

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::action::Action;
use crate::app::{AppMode, FocusedPanel};

/// Map a raw mouse event to an `Action`, passing coordinates through. All
/// hit-testing (which panel / row / chip / menu the point lands on) happens in
/// the App handler, not here.
pub fn map_mouse_to_action(mouse: MouseEvent) -> Option<Action> {
    let (col, row) = (mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => Some(Action::MouseClick { col, row }),
        MouseEventKind::Down(MouseButton::Right) => Some(Action::MouseRightClick { col, row }),
        MouseEventKind::ScrollDown => Some(Action::MouseScroll {
            col,
            row,
            down: true,
        }),
        MouseEventKind::ScrollUp => Some(Action::MouseScroll {
            col,
            row,
            down: false,
        }),
        MouseEventKind::Drag(MouseButton::Left) => Some(Action::MouseDrag { col, row }),
        MouseEventKind::Up(MouseButton::Left) => Some(Action::MouseUp { col, row }),
        _ => None,
    }
}

pub fn map_key_to_action(
    key: KeyEvent,
    mode: &AppMode,
    focused_panel: FocusedPanel,
    editing_commit: bool,
    files_filter_active: bool,
    commit_filter_active: bool,
) -> Option<Action> {
    // With the keyboard-enhancement flags active (see `tui::init`) the terminal
    // also delivers Release and Repeat key events, not just Press. Legacy Windows
    // consoles emit Release too. Drop Release outright — otherwise every binding
    // would fire a second time when the key is let go. Repeat is deliberately
    // treated as Press so a held navigation key keeps scrolling.
    if key.kind == KeyEventKind::Release {
        return None;
    }

    // Ctrl+Q always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
        return Some(Action::ForceQuit);
    }

    // F12 toggles debug key display
    if key.code == KeyCode::F(12) {
        return Some(Action::ToggleDebugKeys);
    }

    // Alt+/ toggles layout
    if key.modifiers.contains(KeyModifiers::ALT) && key.code == KeyCode::Char('/') {
        return Some(Action::ToggleLayout);
    }

    match mode {
        AppMode::Normal => map_normal_mode(key, focused_panel, editing_commit, files_filter_active, commit_filter_active),
        AppMode::Help => map_help_mode(key),
        AppMode::Input { action, .. } => {
            if *action == crate::app::InputAction::Search {
                map_search_mode(key)
            } else {
                map_input_mode(key)
            }
        }
        AppMode::Confirm { .. } => map_confirm_mode(key),
        AppMode::Error { .. } => map_error_mode(key),
        AppMode::CommitMenu { .. } => map_commit_menu_mode(key),
        AppMode::MetadataMenu { .. } => map_metadata_menu_mode(key),
        AppMode::Settings { .. } => map_settings_menu_mode(key),
        AppMode::PullDivergence { .. } => map_pull_divergence_mode(key),
        AppMode::CiChecks => map_ci_checks_mode(key),
        AppMode::PrThread => map_pr_thread_mode(key),
        AppMode::PrCompose { .. } => map_pr_compose_mode(key),
        // The small pickers share the pull-divergence menu keymap.
        AppMode::PrMergePicker { .. } | AppMode::PrReviewPicker { .. } => {
            map_pull_divergence_mode(key)
        }
        AppMode::IssueList => map_issue_list_mode(key),
        AppMode::IssueDetail => map_issue_detail_mode(key),
        // The issue-compose editor shares the PR-compose keymap (Ctrl+S submit,
        // Esc cancel, Enter newline, everything else the editor).
        AppMode::IssueCompose { .. } => map_pr_compose_mode(key),
        AppMode::IssueLabelPicker { .. } => map_issue_label_picker_mode(key),
        AppMode::IssueLabelFilter { .. } => map_issue_label_filter_mode(key),
        AppMode::BranchPicker { .. }
        | AppMode::BranchDeletePicker { .. }
        | AppMode::TagPicker { .. }
        | AppMode::RemotePicker { .. } => map_picker_mode(key),
        AppMode::BranchFilter { .. } => map_branch_filter_mode(key),
        AppMode::FileDiff { .. } => map_file_diff_mode(key),
        AppMode::FileHistory { .. } => map_picker_mode(key),
        AppMode::CommandPalette { .. } => map_command_palette_mode(key),
    }
}

/// Command palette: type to filter, ↑↓ to navigate, Enter to run, Esc to close.
/// Reuses the shared single-line text-editing shortcuts for the query.
fn map_command_palette_mode(key: KeyEvent) -> Option<Action> {
    if let Some(action) = map_text_editing_shortcut(key) {
        return Some(action);
    }
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) => Some(Action::MoveUp),
        (KeyModifiers::NONE, KeyCode::Down) => Some(Action::MoveDown),
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::MenuSelect),
        (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::Cancel),
        (KeyModifiers::NONE, KeyCode::Backspace) => Some(Action::InputBackspace),
        (KeyModifiers::NONE, KeyCode::Char(c)) => Some(Action::InputChar(c)),
        (KeyModifiers::SHIFT, KeyCode::Char(c)) => Some(Action::InputChar(c)),
        _ => None,
    }
}

fn map_normal_mode(
    key: KeyEvent,
    panel: FocusedPanel,
    editing_commit: bool,
    files_filter_active: bool,
    commit_filter_active: bool,
) -> Option<Action> {
    // F5 triggers a full update (fetch all remotes + PR refetch + refresh) from
    // anywhere in Normal mode, including while editing a commit message.
    if key.code == KeyCode::F(5) {
        return Some(Action::FullUpdate);
    }

    // If editing commit message, route to editor keybindings
    if editing_commit && panel == FocusedPanel::CommitDetail {
        return map_editor_mode(key);
    }

    // Command palette: Ctrl+P (or ':' for vim muscle memory) from any panel,
    // unless a text filter is currently capturing input.
    if !files_filter_active && !commit_filter_active {
        let ctrl_p = key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('p');
        let colon = !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
            && key.code == KeyCode::Char(':');
        if ctrl_p || colon {
            return Some(Action::OpenCommandPalette);
        }

        // Settings menu: Ctrl+, from any panel (VSCode-style).
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(',') {
            return Some(Action::OpenSettings);
        }

        // Shift+I opens the GitHub issue list from any panel. 'I' is unbound in
        // every Normal-mode panel scope (lowercase 'i' is Files-only gitignore).
        if key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::Char('I') {
            return Some(Action::OpenIssueList);
        }
    }

    // Panel navigation with left/right arrows and Tab (from any panel)
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Left) | (KeyModifiers::SHIFT, KeyCode::BackTab) => {
            return Some(Action::PanelLeft)
        }
        (KeyModifiers::NONE, KeyCode::Right) | (KeyModifiers::NONE, KeyCode::Tab) => {
            return Some(Action::PanelRight)
        }
        _ => {}
    }

    match panel {
        FocusedPanel::Graph => {
            if commit_filter_active {
                map_commit_filter_mode(key)
            } else {
                map_graph_mode(key)
            }
        }
        FocusedPanel::Files => {
            if files_filter_active {
                map_files_filter_mode(key)
            } else {
                map_files_mode(key)
            }
        }
        FocusedPanel::CommitDetail => map_commit_detail_mode(key),
    }
}

fn map_graph_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        // Movement (arrow keys only, j/k removed per requirements)
        (KeyModifiers::NONE, KeyCode::Down) => Some(Action::MoveDown),
        (KeyModifiers::NONE, KeyCode::Up) => Some(Action::MoveUp),

        // Follow the selected commit's lane to the adjacent commit on the
        // SAME graph line (skipping interleaved commits from other
        // branches). Matches any modifier combo that includes CONTROL since
        // some terminals also report SHIFT on Ctrl+Arrow.
        (m, KeyCode::Up) if m.contains(KeyModifiers::CONTROL) => Some(Action::SameLaneUp),
        (m, KeyCode::Down) if m.contains(KeyModifiers::CONTROL) => Some(Action::SameLaneDown),

        // Page scroll
        (KeyModifiers::CONTROL, KeyCode::Char('d')) | (KeyModifiers::NONE, KeyCode::PageDown) => {
            Some(Action::PageDown)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
            Some(Action::PageUp)
        }

        // Top/bottom. Ctrl+Home matches Home: some terminals/muscle memory send
        // the chord for "jump to very top".
        (KeyModifiers::NONE, KeyCode::Char('g'))
        | (KeyModifiers::NONE, KeyCode::Home)
        | (KeyModifiers::CONTROL, KeyCode::Home) => Some(Action::GoToTop),
        (KeyModifiers::SHIFT, KeyCode::Char('G')) | (KeyModifiers::NONE, KeyCode::End) => {
            Some(Action::GoToBottom)
        }

        // Settings menu fallback. Ctrl+, (handled in map_normal_mode) is the
        // VSCode-style binding, but the legacy terminal protocol cannot encode
        // Ctrl+punctuation, so plain ',' — free in this scope — keeps the menu
        // reachable on terminals without keyboard enhancement.
        (KeyModifiers::NONE, KeyCode::Char(',')) => Some(Action::OpenSettings),

        // Jump to HEAD
        (_, KeyCode::Char('@')) => Some(Action::JumpToHead),

        // Branch jump
        (_, KeyCode::Char(']')) => Some(Action::NextBranch),
        (_, KeyCode::Char('[')) => Some(Action::PrevBranch),

        // Enter opens commit menu (or goes to files for uncommitted)
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::OpenCommitMenu),

        // Mark / compare two commits
        (KeyModifiers::NONE, KeyCode::Char('m')) => Some(Action::MarkForCompare),

        // Jump to the fork point (merge base with main, or HEAD). '^' — the
        // caret points "up" to where the lines meet; lowercase 'b' is taken by
        // Create branch. Matches any modifier since '^' arrives shifted.
        (_, KeyCode::Char('^')) => Some(Action::JumpToMergeBase),

        // Undo the last reversible graph operation. Ctrl+Z is free in graph
        // scope (the files pane has its own separate file-op undo).
        (KeyModifiers::CONTROL, KeyCode::Char('z')) => Some(Action::UndoLastOp),

        // Open the selected commit's PR in the browser
        (KeyModifiers::NONE, KeyCode::Char('o')) => Some(Action::OpenPr),

        // Open the CI checks detail popup ('c' is free in the graph scope; the
        // conflict-resolution 'c' is Files-only)
        (KeyModifiers::NONE, KeyCode::Char('c')) => Some(Action::OpenCiChecks),

        // Open the PR conversation thread ('v' = view; the archive 'v' is
        // Files-only)
        (KeyModifiers::NONE, KeyCode::Char('v')) => Some(Action::OpenPrThread),

        // Toggle which metadata columns show on commit rows ('m' is taken by
        // mark/compare, so Shift+M — mnemonic "Metadata")
        (KeyModifiers::SHIFT, KeyCode::Char('M')) => Some(Action::OpenMetadataMenu),

        // Toggle branch tracing (highlight the selected commit's lineage). 't'
        // is graph-only; the conflict-resolution 't' is Files-only.
        (KeyModifiers::NONE, KeyCode::Char('t')) => Some(Action::ToggleTrace),

        // Shrink / widen the graph column width cap (one lane = 2 cells). Match
        // any modifier since '<'/'>' arrive shifted on most layouts.
        (_, KeyCode::Char('<')) => Some(Action::ShrinkGraphWidth),
        (_, KeyCode::Char('>')) => Some(Action::WidenGraphWidth),

        // Quick actions
        (KeyModifiers::NONE, KeyCode::Char('b')) => Some(Action::CreateBranch),
        (KeyModifiers::NONE, KeyCode::Char('d')) => Some(Action::DeleteBranch),
        (KeyModifiers::NONE, KeyCode::Char('f')) => Some(Action::Fetch),
        // Pull / push pairing: lowercase pull, Shift+P push.
        (KeyModifiers::NONE, KeyCode::Char('p')) => Some(Action::Pull),
        (KeyModifiers::SHIFT, KeyCode::Char('P')) => Some(Action::Push),

        // Space opens file diff for quick access
        (KeyModifiers::NONE, KeyCode::Char(' ')) => Some(Action::OpenFileDiff),

        // Commit filter
        (KeyModifiers::CONTROL, KeyCode::Char('f')) => Some(Action::StartCommitFilter),

        // Branch filter
        (KeyModifiers::SHIFT, KeyCode::Char('B')) => Some(Action::OpenBranchFilter),

        // Show/hide remote-only branches. Upstream uses 'o', but that's taken
        // here by Open PR, so Shift+O — it keeps the "O" mnemonic and pairs with
        // Shift+B (the per-branch filter), the other branch-visibility control.
        (KeyModifiers::SHIFT, KeyCode::Char('O')) => Some(Action::ToggleRemoteBranches),

        // Show/hide branches already merged into the trunk (merge or squash).
        // Shift+H — mnemonic "Hide merged" — joins the branch-visibility cluster
        // with Shift+B (filter) and Shift+O (remotes). Merged branches are dimmed
        // by default; this toggle removes them from the graph entirely.
        (KeyModifiers::SHIFT, KeyCode::Char('H')) => Some(Action::ToggleMergedBranches),

        // UI
        (_, KeyCode::Char('/')) => Some(Action::Search),
        (KeyModifiers::SHIFT, KeyCode::Char('R')) => Some(Action::Refresh),
        (_, KeyCode::Char('?')) => Some(Action::ToggleHelp),
        (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::Quit),

        _ => None,
    }
}

fn map_files_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        // Movement
        (KeyModifiers::NONE, KeyCode::Down) => Some(Action::MoveDown),
        (KeyModifiers::NONE, KeyCode::Up) => Some(Action::MoveUp),
        (KeyModifiers::CONTROL, KeyCode::Char('d')) | (KeyModifiers::NONE, KeyCode::PageDown) => {
            Some(Action::PageDown)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
            Some(Action::PageUp)
        }
        (KeyModifiers::NONE, KeyCode::Home) => Some(Action::GoToTop),
        (KeyModifiers::NONE, KeyCode::End) => Some(Action::GoToBottom),

        // Stage/unstage
        (KeyModifiers::NONE, KeyCode::Char('s')) | (KeyModifiers::NONE, KeyCode::Char('a')) => {
            Some(Action::ToggleStage)
        }

        // Stage-all / unstage-all
        (KeyModifiers::SHIFT, KeyCode::Char('S')) => Some(Action::StageAll),
        (KeyModifiers::SHIFT, KeyCode::Char('U')) => Some(Action::UnstageAll),

        // Add to .gitignore
        (KeyModifiers::NONE, KeyCode::Char('i')) => Some(Action::AddToGitignore),

        // Archive file
        (KeyModifiers::NONE, KeyCode::Char('v')) => Some(Action::ArchiveFile),

        // Move file to recycle bin
        (KeyModifiers::NONE, KeyCode::Delete) => Some(Action::TrashFile),

        // Undo last file operation
        (KeyModifiers::CONTROL, KeyCode::Char('z')) => Some(Action::UndoLastFileOp),

        // Folder view toggle
        (KeyModifiers::NONE, KeyCode::Char('f')) => Some(Action::ToggleFolderView),

        // Restore all changes (discard)
        (KeyModifiers::NONE, KeyCode::Char('r')) => Some(Action::RestoreFile),

        // Jump to next / previous conflicted file (Merge Changes section). Free
        // in this scope and mirrors the graph's ]/[ (next/prev branch) and the
        // diff viewer's ]/[ (next/prev hunk).
        (_, KeyCode::Char(']')) => Some(Action::NextConflict),
        (_, KeyCode::Char('[')) => Some(Action::PrevConflict),

        // Merge-conflict resolution (active only when an operation is in progress)
        (KeyModifiers::NONE, KeyCode::Char('o')) => Some(Action::AcceptOurs),
        (KeyModifiers::NONE, KeyCode::Char('t')) => Some(Action::AcceptTheirs),
        (KeyModifiers::NONE, KeyCode::Char('c')) => Some(Action::ContinueOperation),
        (KeyModifiers::SHIFT, KeyCode::Char('A')) => Some(Action::AbortOperation),

        // Open file with default app
        (KeyModifiers::NONE, KeyCode::Char(' ')) => Some(Action::OpenWithDefault),

        // Copy the selected file's repo-relative path
        (KeyModifiers::NONE, KeyCode::Char('y')) => Some(Action::CopyPath),

        // Settings menu fallback (see map_graph_mode): plain ',' is free in this
        // scope and reaches the menu where Ctrl+, cannot be encoded.
        (KeyModifiers::NONE, KeyCode::Char(',')) => Some(Action::OpenSettings),

        // Enter file diff for viewing
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::OpenFileDiff),

        // Per-file commit history
        (KeyModifiers::NONE, KeyCode::Char('h')) => Some(Action::FileHistory),

        // Start filter mode
        (KeyModifiers::CONTROL, KeyCode::Char('f')) => Some(Action::StartFilesFilter),

        // Esc returns to graph
        (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::FocusGraph),

        // Help
        (_, KeyCode::Char('?')) => Some(Action::ToggleHelp),

        _ => None,
    }
}

/// Key bindings when commit filter is active (typing goes to filter)
fn map_commit_filter_mode(key: KeyEvent) -> Option<Action> {
    if let Some(action) = map_text_editing_shortcut(key) {
        return Some(action);
    }
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::Confirm),
        (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::Cancel),
        (KeyModifiers::NONE, KeyCode::Up) => Some(Action::MoveUp),
        (KeyModifiers::NONE, KeyCode::Down) => Some(Action::MoveDown),
        (KeyModifiers::NONE, KeyCode::Backspace) => Some(Action::CommitFilterBackspace),
        (KeyModifiers::NONE, KeyCode::Char(c)) => Some(Action::CommitFilterChar(c)),
        (KeyModifiers::SHIFT, KeyCode::Char(c)) => Some(Action::CommitFilterChar(c)),
        _ => None,
    }
}

/// Key bindings when files filter is active (typing goes to filter)
fn map_files_filter_mode(key: KeyEvent) -> Option<Action> {
    if let Some(action) = map_text_editing_shortcut(key) {
        return Some(action);
    }
    match (key.modifiers, key.code) {
        // Enter confirms filter (keep filter text, exit filter mode)
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::Confirm),
        // Esc cancels filter (clear filter text, exit filter mode)
        (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::Cancel),
        // Backspace
        (KeyModifiers::NONE, KeyCode::Backspace) => Some(Action::FilesFilterBackspace),
        // Characters go to filter
        (KeyModifiers::NONE, KeyCode::Char(c)) => Some(Action::FilesFilterChar(c)),
        (KeyModifiers::SHIFT, KeyCode::Char(c)) => Some(Action::FilesFilterChar(c)),
        _ => None,
    }
}

fn map_commit_detail_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        // Scroll
        (KeyModifiers::NONE, KeyCode::Down) => Some(Action::MoveDown),
        (KeyModifiers::NONE, KeyCode::Up) => Some(Action::MoveUp),
        (KeyModifiers::CONTROL, KeyCode::Char('d')) | (KeyModifiers::NONE, KeyCode::PageDown) => {
            Some(Action::PageDown)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
            Some(Action::PageUp)
        }
        (KeyModifiers::NONE, KeyCode::Home) => Some(Action::GoToTop),

        // Enter starts editing (for uncommitted changes)
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::StartEditing),

        // Ctrl+Enter: amend --no-edit (on uncommitted node with staged files)
        (m, KeyCode::Enter) if m.contains(KeyModifiers::CONTROL) => Some(Action::AmendCommit),

        // Ctrl+S: stash staged changes with commit message
        (m, KeyCode::Char('s')) if m.contains(KeyModifiers::CONTROL) => Some(Action::StashStaged),

        // Esc returns to graph
        (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::FocusGraph),

        // Help
        (_, KeyCode::Char('?')) => Some(Action::ToggleHelp),

        // Any printable character starts editing
        (KeyModifiers::NONE, KeyCode::Char(c)) | (KeyModifiers::SHIFT, KeyCode::Char(c)) => {
            Some(Action::EditorChar(c))
        }

        _ => None,
    }
}

fn map_editor_mode(key: KeyEvent) -> Option<Action> {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match (key.modifiers, key.code) {
        // Enter commits
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::CommitChanges),

        // Ctrl+Enter amends last commit
        (m, KeyCode::Enter) if m.contains(KeyModifiers::CONTROL) => {
            Some(Action::AmendCommit)
        }

        // Ctrl+S: stash staged changes with commit message
        (m, KeyCode::Char('s')) if m.contains(KeyModifiers::CONTROL) => {
            Some(Action::StashStaged)
        }

        // Esc exits edit mode
        (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::StopEditing),

        // Shift+Enter / Alt+Enter inserts newline
        (m, KeyCode::Enter)
            if m.contains(KeyModifiers::SHIFT) || m.contains(KeyModifiers::ALT) =>
        {
            Some(Action::EditorNewline)
        }

        // Alt+Backspace / Ctrl+Backspace / Ctrl+H: delete word backward
        (m, KeyCode::Backspace)
            if m.contains(KeyModifiers::ALT) || m.contains(KeyModifiers::CONTROL) =>
        {
            Some(Action::EditorBackspaceWord)
        }
        (m, KeyCode::Char('h')) if m.contains(KeyModifiers::CONTROL) => {
            Some(Action::EditorBackspaceWord)
        }

        // Alt+Delete / Ctrl+Delete / Alt+d: delete word forward
        (m, KeyCode::Delete)
            if m.contains(KeyModifiers::ALT) || m.contains(KeyModifiers::CONTROL) =>
        {
            Some(Action::EditorDeleteWord)
        }

        // Ctrl+U: kill line (delete to beginning of line)
        (m, KeyCode::Char('u')) if m.contains(KeyModifiers::CONTROL) => {
            Some(Action::EditorKillLine)
        }
        (m, KeyCode::Char('d')) if m.contains(KeyModifiers::ALT) => {
            Some(Action::EditorDeleteWord)
        }

        // Alt+Left / Ctrl+Left / Alt+b: word left
        (m, KeyCode::Left) if m.contains(KeyModifiers::ALT) || m.contains(KeyModifiers::CONTROL) => {
            Some(Action::EditorWordLeft(shift))
        }
        (m, KeyCode::Char('b')) if m.contains(KeyModifiers::ALT) && !shift => {
            Some(Action::EditorWordLeft(false))
        }

        // Alt+Right / Ctrl+Right / Alt+f: word right
        (m, KeyCode::Right) if m.contains(KeyModifiers::ALT) || m.contains(KeyModifiers::CONTROL) => {
            Some(Action::EditorWordRight(shift))
        }
        (m, KeyCode::Char('f')) if m.contains(KeyModifiers::ALT) && !shift => {
            Some(Action::EditorWordRight(false))
        }

        // Ctrl+Home / Alt+Home: text start
        (m, KeyCode::Home) if m.contains(KeyModifiers::CONTROL) || m.contains(KeyModifiers::ALT) => {
            Some(Action::EditorTextStart(shift))
        }
        // Ctrl+End / Alt+End: text end
        (m, KeyCode::End) if m.contains(KeyModifiers::CONTROL) || m.contains(KeyModifiers::ALT) => {
            Some(Action::EditorTextEnd(shift))
        }

        // Basic cursor movement
        (_, KeyCode::Left) if !alt && !ctrl => Some(Action::EditorLeft(shift)),
        (_, KeyCode::Right) if !alt && !ctrl => Some(Action::EditorRight(shift)),
        (_, KeyCode::Up) if !alt && !ctrl => Some(Action::EditorUp(shift)),
        (_, KeyCode::Down) if !alt && !ctrl => Some(Action::EditorDown(shift)),
        (_, KeyCode::Home) if !alt && !ctrl => Some(Action::EditorHome(shift)),
        (_, KeyCode::End) if !alt && !ctrl => Some(Action::EditorEnd(shift)),

        // Backspace/Delete
        (KeyModifiers::NONE, KeyCode::Backspace) => Some(Action::EditorBackspace),
        (KeyModifiers::NONE, KeyCode::Delete) => Some(Action::EditorDelete),

        // Character input (no ctrl, no alt)
        (m, KeyCode::Char(c))
            if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
        {
            Some(Action::EditorChar(c))
        }

        _ => None,
    }
}

fn map_picker_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) => Some(Action::MoveUp),
        (KeyModifiers::NONE, KeyCode::Down) => Some(Action::MoveDown),
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::MenuSelect),
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
            Some(Action::Cancel)
        }
        _ => None,
    }
}

fn map_metadata_menu_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
            Some(Action::MoveUp)
        }
        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
            Some(Action::MoveDown)
        }
        (KeyModifiers::NONE, KeyCode::Char(' ')) | (KeyModifiers::NONE, KeyCode::Enter) => {
            Some(Action::MenuSelect)
        }
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
            Some(Action::Cancel)
        }
        _ => None,
    }
}

/// Settings menu: arrows move, Space/Enter toggles or cycles, typing
/// fuzzy-filters the list by label (mirrors the command palette), Backspace
/// edits the filter (or an in-progress numeric buffer), Esc unwinds one layer
/// at a time (cancel edit → clear filter → close).
///
/// 'j'/'k' are intentionally NOT nav aliases here (unlike other list modes)
/// since they're ordinary filterable letters once typing is live — only the
/// arrow keys move the selection. Space stays a toggle, not filter text
/// (matching `BranchFilter`, which reserves Space for its own toggle too).
fn map_settings_menu_mode(key: KeyEvent) -> Option<Action> {
    if let Some(action) = map_text_editing_shortcut(key) {
        return Some(action);
    }
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) => Some(Action::MoveUp),
        (KeyModifiers::NONE, KeyCode::Down) => Some(Action::MoveDown),
        (KeyModifiers::NONE, KeyCode::Char(' ')) | (KeyModifiers::NONE, KeyCode::Enter) => {
            Some(Action::MenuSelect)
        }
        (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::Cancel),
        (KeyModifiers::NONE, KeyCode::Backspace) => Some(Action::InputBackspace),
        (KeyModifiers::NONE, KeyCode::Char(c)) => Some(Action::InputChar(c)),
        (KeyModifiers::SHIFT, KeyCode::Char(c)) => Some(Action::InputChar(c)),
        _ => None,
    }
}

fn map_pr_thread_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
            Some(Action::MoveUp)
        }
        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
            Some(Action::MoveDown)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
            Some(Action::PageUp)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('d')) | (KeyModifiers::NONE, KeyCode::PageDown) => {
            Some(Action::PageDown)
        }
        (KeyModifiers::NONE, KeyCode::Char('g')) | (KeyModifiers::NONE, KeyCode::Home) => {
            Some(Action::GoToTop)
        }
        (KeyModifiers::SHIFT, KeyCode::Char('G')) | (KeyModifiers::NONE, KeyCode::End) => {
            Some(Action::GoToBottom)
        }
        (KeyModifiers::NONE, KeyCode::Char('o')) => Some(Action::OpenPr),
        (KeyModifiers::NONE, KeyCode::Char('r')) => Some(Action::OpenReviewPicker),
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
            Some(Action::Cancel)
        }
        _ => None,
    }
}

/// Issue list: j/k move, Enter opens the detail, Tab/f cycle the filter, r
/// refresh, n new issue, o open in browser, Esc/q close.
fn map_issue_list_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
            Some(Action::MoveUp)
        }
        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
            Some(Action::MoveDown)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
            Some(Action::PageUp)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('d')) | (KeyModifiers::NONE, KeyCode::PageDown) => {
            Some(Action::PageDown)
        }
        (KeyModifiers::NONE, KeyCode::Char('g')) | (KeyModifiers::NONE, KeyCode::Home) => {
            Some(Action::GoToTop)
        }
        (KeyModifiers::SHIFT, KeyCode::Char('G')) | (KeyModifiers::NONE, KeyCode::End) => {
            Some(Action::GoToBottom)
        }
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::OpenIssueDetail),
        (KeyModifiers::NONE, KeyCode::Tab) | (KeyModifiers::NONE, KeyCode::Char('f')) => {
            Some(Action::CycleIssueFilter)
        }
        (KeyModifiers::NONE, KeyCode::Char('r')) => Some(Action::RefreshIssues),
        (KeyModifiers::NONE, KeyCode::Char('n')) => Some(Action::NewIssue),
        (KeyModifiers::NONE, KeyCode::Char('t')) => Some(Action::OpenIssueLabelFilter),
        (KeyModifiers::NONE, KeyCode::Char('u')) => Some(Action::ToggleUnblockedOnly),
        (KeyModifiers::NONE, KeyCode::Char('l')) => Some(Action::EditIssueLabels),
        (KeyModifiers::NONE, KeyCode::Char('o')) => Some(Action::OpenIssueInBrowser),
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
            Some(Action::Cancel)
        }
        _ => None,
    }
}

/// Issue label-filter picker: j/k move, Space toggles, Ctrl+A all, Ctrl+O none,
/// Enter applies, Esc cancels. Mirrors the branch-filter checkbox idiom.
fn map_issue_label_filter_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
            Some(Action::MoveUp)
        }
        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
            Some(Action::MoveDown)
        }
        (KeyModifiers::NONE, KeyCode::Char(' ')) => Some(Action::ToggleIssueLabel),
        (KeyModifiers::CONTROL, KeyCode::Char('a')) => Some(Action::SelectAll),
        (KeyModifiers::CONTROL, KeyCode::Char('o')) => Some(Action::SelectNone),
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::MenuSelect),
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
            Some(Action::Cancel)
        }
        _ => None,
    }
}

/// Issue detail: j/k scroll, c comment, x close/reopen, l labels, a assignees,
/// o browser, r refresh, Esc back to the list.
fn map_issue_detail_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
            Some(Action::MoveUp)
        }
        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
            Some(Action::MoveDown)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
            Some(Action::PageUp)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('d')) | (KeyModifiers::NONE, KeyCode::PageDown) => {
            Some(Action::PageDown)
        }
        (KeyModifiers::NONE, KeyCode::Char('g')) | (KeyModifiers::NONE, KeyCode::Home) => {
            Some(Action::GoToTop)
        }
        (KeyModifiers::SHIFT, KeyCode::Char('G')) | (KeyModifiers::NONE, KeyCode::End) => {
            Some(Action::GoToBottom)
        }
        (KeyModifiers::NONE, KeyCode::Char('c')) => Some(Action::CommentOnIssue),
        (KeyModifiers::NONE, KeyCode::Char('x')) => Some(Action::ToggleIssueState),
        (KeyModifiers::NONE, KeyCode::Char('l')) => Some(Action::EditIssueLabels),
        (KeyModifiers::NONE, KeyCode::Char('a')) => Some(Action::EditIssueAssignees),
        (KeyModifiers::NONE, KeyCode::Char('o')) => Some(Action::OpenIssueInBrowser),
        (KeyModifiers::NONE, KeyCode::Char('r')) => Some(Action::RefreshIssues),
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
            Some(Action::Cancel)
        }
        _ => None,
    }
}

/// Issue label picker: j/k move, Space toggles the label, Enter applies, Esc
/// cancels.
fn map_issue_label_picker_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
            Some(Action::MoveUp)
        }
        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
            Some(Action::MoveDown)
        }
        (KeyModifiers::NONE, KeyCode::Char(' ')) => Some(Action::ToggleIssueLabel),
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::MenuSelect),
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
            Some(Action::Cancel)
        }
        _ => None,
    }
}

/// PR-compose editor: Enter inserts a newline, Ctrl+S / Ctrl+Enter submit, Esc
/// cancels; everything else reuses the commit-message editor's key handling.
fn map_pr_compose_mode(key: KeyEvent) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => return Some(Action::Cancel),
        KeyCode::Char('s') if ctrl => return Some(Action::SubmitCompose),
        KeyCode::Char('e') if ctrl => return Some(Action::ExternalEdit),
        KeyCode::Enter if ctrl => return Some(Action::SubmitCompose),
        KeyCode::Enter if key.modifiers.is_empty() => return Some(Action::EditorNewline),
        _ => {}
    }
    map_editor_mode(key)
}

fn map_ci_checks_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
            Some(Action::MoveUp)
        }
        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
            Some(Action::MoveDown)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
            Some(Action::PageUp)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('d')) | (KeyModifiers::NONE, KeyCode::PageDown) => {
            Some(Action::PageDown)
        }
        (KeyModifiers::NONE, KeyCode::Char('g')) | (KeyModifiers::NONE, KeyCode::Home) => {
            Some(Action::GoToTop)
        }
        (KeyModifiers::SHIFT, KeyCode::Char('G')) | (KeyModifiers::NONE, KeyCode::End) => {
            Some(Action::GoToBottom)
        }
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::MenuSelect),
        (KeyModifiers::NONE, KeyCode::Char('o')) => Some(Action::OpenPr),
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
            Some(Action::Cancel)
        }
        _ => None,
    }
}

fn map_pull_divergence_mode(key: KeyEvent) -> Option<Action> {
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) | (KeyModifiers::NONE, KeyCode::Char('k')) => {
            Some(Action::MoveUp)
        }
        (KeyModifiers::NONE, KeyCode::Down) | (KeyModifiers::NONE, KeyCode::Char('j')) => {
            Some(Action::MoveDown)
        }
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::MenuSelect),
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
            Some(Action::Cancel)
        }
        _ => None,
    }
}

fn map_commit_menu_mode(key: KeyEvent) -> Option<Action> {
    if let Some(action) = map_text_editing_shortcut(key) {
        return Some(action);
    }
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) => Some(Action::MoveUp),
        (KeyModifiers::NONE, KeyCode::Down) => Some(Action::MoveDown),
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::MenuSelect),
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
            Some(Action::Cancel)
        }
        (KeyModifiers::NONE, KeyCode::Backspace) => Some(Action::InputBackspace),
        (KeyModifiers::NONE, KeyCode::Char(c)) => Some(Action::InputChar(c)),
        (KeyModifiers::SHIFT, KeyCode::Char(c)) => Some(Action::InputChar(c)),
        _ => None,
    }
}

fn map_branch_filter_mode(key: KeyEvent) -> Option<Action> {
    if let Some(action) = map_text_editing_shortcut(key) {
        return Some(action);
    }
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) => Some(Action::MoveUp),
        (KeyModifiers::NONE, KeyCode::Down) => Some(Action::MoveDown),
        (KeyModifiers::NONE, KeyCode::Char(' ')) => Some(Action::MenuSelect),
        (KeyModifiers::CONTROL, KeyCode::Char('a')) => Some(Action::SelectAll),
        (KeyModifiers::CONTROL, KeyCode::Char('o')) => Some(Action::SelectNone),
        (KeyModifiers::NONE, KeyCode::Enter) => Some(Action::Confirm),
        (KeyModifiers::NONE, KeyCode::Backspace) => Some(Action::InputBackspace),
        (KeyModifiers::NONE, KeyCode::Esc) => Some(Action::Cancel),
        (KeyModifiers::NONE, KeyCode::Char(c)) => Some(Action::InputChar(c)),
        (KeyModifiers::SHIFT, KeyCode::Char(c)) => Some(Action::InputChar(c)),
        _ => None,
    }
}

fn map_help_mode(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => Some(Action::ToggleHelp),
        _ => None,
    }
}

/// Common word-editing shortcuts for simple text fields (no cursor position).
/// Returns Some(action) if the key is a word-editing shortcut, None otherwise.
fn map_text_editing_shortcut(key: KeyEvent) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        KeyCode::Backspace if ctrl || alt => Some(Action::InputBackspaceWord),
        KeyCode::Char('h') if ctrl => Some(Action::InputBackspaceWord),
        KeyCode::Char('u') if ctrl => Some(Action::InputClearLine),
        _ => None,
    }
}


fn map_input_mode(key: KeyEvent) -> Option<Action> {
    if let Some(action) = map_text_editing_shortcut(key) {
        return Some(action);
    }
    match key.code {
        KeyCode::Enter => Some(Action::Confirm),
        KeyCode::Esc => Some(Action::Cancel),
        KeyCode::Backspace => Some(Action::InputBackspace),
        KeyCode::Char(c) => Some(Action::InputChar(c)),
        _ => None,
    }
}

fn map_search_mode(key: KeyEvent) -> Option<Action> {
    if let Some(action) = map_text_editing_shortcut(key) {
        return Some(action);
    }
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Up) => Some(Action::SearchSelectUp),
        (KeyModifiers::NONE, KeyCode::Down) => Some(Action::SearchSelectDown),
        (KeyModifiers::CONTROL, KeyCode::Char('k')) => Some(Action::SearchSelectUp),
        (KeyModifiers::CONTROL, KeyCode::Char('j')) => Some(Action::SearchSelectDown),
        (KeyModifiers::NONE, KeyCode::Tab) => Some(Action::SearchSelectDownQuiet),
        (KeyModifiers::SHIFT, KeyCode::BackTab) => Some(Action::SearchSelectUpQuiet),
        (_, KeyCode::Enter) => Some(Action::Confirm),
        (_, KeyCode::Esc) => Some(Action::Cancel),
        (_, KeyCode::Backspace) | (_, KeyCode::Delete) => Some(Action::InputBackspace),
        (_, KeyCode::Char(c)) => Some(Action::InputChar(c)),
        _ => None,
    }
}

fn map_confirm_mode(key: KeyEvent) -> Option<Action> {
    // Secondary confirm (delete-branch "also delete remote"): Ctrl+Enter, with a
    // plain `R` fallback because the legacy terminal protocol can't encode
    // Ctrl+Enter without the kitty keyboard enhancement (same reachability
    // concern as the Ctrl+, settings binding). The handler ignores it for any
    // confirm action that isn't a DeleteBranchWithRemote.
    if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(Action::ConfirmDeleteBranchAndRemote);
    }
    match key.code {
        KeyCode::Char('R') => Some(Action::ConfirmDeleteBranchAndRemote),
        KeyCode::Char('y') | KeyCode::Enter => Some(Action::Confirm),
        KeyCode::Char('n') | KeyCode::Esc => Some(Action::Cancel),
        _ => None,
    }
}

fn map_error_mode(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => Some(Action::Cancel),
        _ => None,
    }
}

fn map_file_diff_mode(key: KeyEvent) -> Option<Action> {
    // Ctrl+Alt+W toggles soft line-wrapping. Matched via a guard (combined
    // modifiers) before the exact-modifier arms below.
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && key.modifiers.contains(KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char('w') | KeyCode::Char('W'))
    {
        return Some(Action::ToggleDiffWrap);
    }
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Down) => Some(Action::ScrollDown),
        (KeyModifiers::NONE, KeyCode::Up) => Some(Action::ScrollUp),
        (KeyModifiers::CONTROL, KeyCode::Char('d')) => Some(Action::ScrollPageDown),
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => Some(Action::ScrollPageUp),
        (KeyModifiers::CONTROL, KeyCode::Char('f')) | (KeyModifiers::NONE, KeyCode::PageDown) => {
            Some(Action::PageDown)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('b')) | (KeyModifiers::NONE, KeyCode::PageUp) => {
            Some(Action::PageUp)
        }
        (KeyModifiers::NONE, KeyCode::Char('g')) | (KeyModifiers::NONE, KeyCode::Home) => {
            Some(Action::ScrollToTop)
        }
        (KeyModifiers::SHIFT, KeyCode::Char('G')) | (KeyModifiers::NONE, KeyCode::End) => {
            Some(Action::ScrollToBottom)
        }
        (KeyModifiers::NONE, KeyCode::Char('h')) | (KeyModifiers::NONE, KeyCode::Left) => {
            Some(Action::ScrollLeft)
        }
        (KeyModifiers::NONE, KeyCode::Char('l')) | (KeyModifiers::NONE, KeyCode::Right) => {
            Some(Action::ScrollRight)
        }
        (KeyModifiers::NONE, KeyCode::Char('0')) => Some(Action::ScrollToLineStart),
        (_, KeyCode::Char(']')) => Some(Action::NextHunk),
        (_, KeyCode::Char('[')) => Some(Action::PrevHunk),
        (KeyModifiers::NONE, KeyCode::Char('n')) => Some(Action::NextFile),
        (KeyModifiers::SHIFT, KeyCode::Char('N')) => Some(Action::PrevFile),
        // Hunk-level staging (uncommitted diffs only; guarded in the handler)
        (KeyModifiers::NONE, KeyCode::Char('s')) => Some(Action::StageHunk),
        (KeyModifiers::NONE, KeyCode::Char('u')) => Some(Action::UnstageHunk),
        (KeyModifiers::NONE, KeyCode::Char('x')) => Some(Action::DiscardHunk),
        (KeyModifiers::NONE, KeyCode::Esc) | (KeyModifiers::NONE, KeyCode::Char('q')) => {
            Some(Action::Cancel)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_maps_ctrl_e_to_external_edit() {
        let key = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert_eq!(map_pr_compose_mode(key), Some(Action::ExternalEdit));
    }

    #[test]
    fn compose_ctrl_s_still_submits_and_plain_e_types_a_char() {
        let submit = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(map_pr_compose_mode(submit), Some(Action::SubmitCompose));
        // Plain 'e' (no ctrl) types a character, not an external-edit request.
        let plain = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE);
        assert_eq!(map_pr_compose_mode(plain), Some(Action::EditorChar('e')));
    }

    #[test]
    fn graph_ctrl_home_goes_to_top() {
        let plain = KeyEvent::new(KeyCode::Home, KeyModifiers::NONE);
        let chord = KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL);
        assert_eq!(map_normal(plain), Some(Action::GoToTop));
        assert_eq!(map_normal(chord), Some(Action::GoToTop));
    }

    /// Helper: map a key in Normal mode, Graph panel, no filters/editing.
    fn map_normal(key: KeyEvent) -> Option<Action> {
        map_key_to_action(
            key,
            &AppMode::Normal,
            FocusedPanel::Graph,
            false,
            false,
            false,
        )
    }

    #[test]
    fn release_events_produce_no_action() {
        // With keyboard enhancement on, the terminal echoes a Release for every
        // key. It must not re-fire the binding (here: 'p' → Pull in the graph).
        let press = KeyEvent::new_with_kind(
            KeyCode::Char('p'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        );
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('p'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(map_normal(press), Some(Action::Pull));
        assert_eq!(map_normal(release), None);
    }

    #[test]
    fn repeat_events_behave_as_press() {
        // A held navigation key repeats; Repeat is treated as Press so scrolling
        // continues while held.
        let repeat = KeyEvent::new_with_kind(
            KeyCode::Down,
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        );
        assert_eq!(map_normal(repeat), Some(Action::MoveDown));
    }

    #[test]
    fn plain_comma_opens_settings_in_graph_and_files() {
        // Legacy fallback for the (often unencodable) Ctrl+, binding.
        let comma = KeyEvent::new(KeyCode::Char(','), KeyModifiers::NONE);
        assert_eq!(
            map_key_to_action(
                comma,
                &AppMode::Normal,
                FocusedPanel::Graph,
                false,
                false,
                false
            ),
            Some(Action::OpenSettings)
        );
        assert_eq!(
            map_key_to_action(
                comma,
                &AppMode::Normal,
                FocusedPanel::Files,
                false,
                false,
                false
            ),
            Some(Action::OpenSettings)
        );
    }

    #[test]
    fn ctrl_comma_still_opens_settings() {
        let key = KeyEvent::new(KeyCode::Char(','), KeyModifiers::CONTROL);
        assert_eq!(map_normal(key), Some(Action::OpenSettings));
    }

    #[test]
    fn settings_menu_letters_type_into_the_filter_not_nav_shortcuts() {
        // 'j'/'k' used to be MoveDown/MoveUp aliases; once typing filters the
        // list they must behave like every other letter.
        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(map_settings_menu_mode(j), Some(Action::InputChar('j')));
        assert_eq!(map_settings_menu_mode(k), Some(Action::InputChar('k')));
        // An arbitrary letter also types.
        let d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(map_settings_menu_mode(d), Some(Action::InputChar('d')));
        // Uppercase (Shift held) types too.
        let shift_d = KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT);
        assert_eq!(map_settings_menu_mode(shift_d), Some(Action::InputChar('D')));
    }

    #[test]
    fn settings_menu_arrows_still_navigate() {
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(map_settings_menu_mode(up), Some(Action::MoveUp));
        assert_eq!(map_settings_menu_mode(down), Some(Action::MoveDown));
    }

    #[test]
    fn settings_menu_space_and_enter_still_select_not_type() {
        let space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(map_settings_menu_mode(space), Some(Action::MenuSelect));
        assert_eq!(map_settings_menu_mode(enter), Some(Action::MenuSelect));
    }

    #[test]
    fn settings_menu_digits_still_route_through_input_char() {
        // The handler (not the keybinding layer) decides whether a digit
        // starts a numeric edit or filters, based on live state.
        let five = KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE);
        assert_eq!(map_settings_menu_mode(five), Some(Action::InputChar('5')));
    }

    #[test]
    fn settings_menu_esc_backspace_map_unconditionally() {
        // The three-way Esc priority (cancel edit / clear filter / close) and
        // backspace's dual role (filter vs numeric buffer) are decided by the
        // action handler, not the keybinding layer — both always map here.
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let bs = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(map_settings_menu_mode(esc), Some(Action::Cancel));
        assert_eq!(map_settings_menu_mode(bs), Some(Action::InputBackspace));
    }

    #[test]
    fn settings_menu_ctrl_w_and_ctrl_u_edit_shortcuts_still_work() {
        let ctrl_w = KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL);
        let ctrl_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(
            map_settings_menu_mode(ctrl_w),
            Some(Action::InputBackspaceWord)
        );
        assert_eq!(map_settings_menu_mode(ctrl_u), Some(Action::InputClearLine));
    }

    #[test]
    fn confirm_mode_plain_enter_is_primary_confirm() {
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(map_confirm_mode(enter), Some(Action::Confirm));
        let y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(map_confirm_mode(y), Some(Action::Confirm));
    }

    #[test]
    fn confirm_mode_ctrl_enter_routes_to_secondary_confirm() {
        // Ctrl+Enter is the "also delete remote" secondary confirm.
        let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        assert_eq!(
            map_confirm_mode(ctrl_enter),
            Some(Action::ConfirmDeleteBranchAndRemote)
        );
    }

    #[test]
    fn confirm_mode_r_is_ctrl_enter_fallback() {
        // `R` reaches the secondary confirm on terminals that can't encode
        // Ctrl+Enter (no kitty keyboard enhancement).
        let r = KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE);
        assert_eq!(
            map_confirm_mode(r),
            Some(Action::ConfirmDeleteBranchAndRemote)
        );
    }
}
