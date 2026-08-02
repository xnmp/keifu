//! Startup-resolved, user-configurable keyboard shortcuts.
//!
//! The registry is deliberately pure: routing, help, and the command palette
//! all ask the same object for effective bindings, while `App` remains the
//! owner of runtime state and action handling.

use std::{collections::HashMap, fmt, str::FromStr};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::{
    action::Action,
    app::{AppMode, FocusedPanel},
};

type Scopes = u128;
const GLOBAL: Scopes = 1 << 0;
const GRAPH: Scopes = 1 << 1;
const FILES: Scopes = 1 << 2;
const DETAIL: Scopes = 1 << 3;
const EDITOR: Scopes = 1 << 4;
const HELP: Scopes = 1 << 5;
const INPUT: Scopes = 1 << 6;
const SEARCH: Scopes = 1 << 7;
const CONFIRM: Scopes = 1 << 8;
const MENU: Scopes = 1 << 9;
const REBASE_PLAN: Scopes = 1 << 10;
const SETTINGS: Scopes = 1 << 11;
const PR_THREAD: Scopes = 1 << 12;
const ISSUE_LIST: Scopes = 1 << 13;
const ISSUE_DETAIL: Scopes = 1 << 14;
const ISSUE_LABELS: Scopes = 1 << 15;
const COMPOSE: Scopes = 1 << 16;
const CI_CHECKS: Scopes = 1 << 17;
const BRANCH_FILTER: Scopes = 1 << 18;
const FILE_DIFF: Scopes = 1 << 19;
const PALETTE: Scopes = 1 << 20;
const NORMAL: Scopes = GRAPH | FILES | DETAIL;
const ALL: Scopes = (1 << 21) - 1;

/// A normalized, single-key terminal shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyBinding {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyBinding {
    pub fn matches(self, event: KeyEvent) -> bool {
        if event.kind == KeyEventKind::Release {
            return false;
        }
        let event_modifiers =
            event.modifiers & (KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT);
        if self.modifiers != event_modifiers {
            return false;
        }
        match (self.code, event.code) {
            (KeyCode::Char(expected), KeyCode::Char(actual)) => {
                expected.eq_ignore_ascii_case(&actual)
            }
            (expected, actual) => expected == actual,
        }
    }
}

impl FromStr for KeyBinding {
    type Err = String;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let source = source.trim();
        if source.is_empty() {
            return Err("binding is empty".into());
        }
        if source.chars().any(char::is_whitespace) {
            return Err("multi-key sequences are not supported".into());
        }
        let parts: Vec<&str> = source.split('+').collect();
        if parts.iter().any(|part| part.is_empty()) {
            return Err("binding contains an empty modifier or key".into());
        }
        let mut modifiers = KeyModifiers::NONE;
        let mut base = None;
        for part in parts {
            let lower = part.to_ascii_lowercase();
            let modifier = match lower.as_str() {
                "ctrl" | "control" => Some(KeyModifiers::CONTROL),
                "alt" => Some(KeyModifiers::ALT),
                "shift" => Some(KeyModifiers::SHIFT),
                _ => None,
            };
            if let Some(modifier) = modifier {
                if base.is_some() {
                    return Err(format!("modifier '{part}' appears after the key"));
                }
                if modifiers.contains(modifier) {
                    return Err(format!("modifier '{part}' is repeated"));
                }
                modifiers.insert(modifier);
            } else if base.is_some() {
                return Err("a binding must contain exactly one key".into());
            } else {
                base = Some(parse_key(part)?);
            }
        }
        let mut code = base.ok_or_else(|| "binding has modifiers but no key".to_string())?;
        if let KeyCode::Char(c) = code {
            if modifiers.contains(KeyModifiers::SHIFT) && c.is_ascii_alphabetic() {
                code = KeyCode::Char(c.to_ascii_uppercase());
            }
        }
        Ok(Self { code, modifiers })
    }
}

fn parse_key(source: &str) -> Result<KeyCode, String> {
    let lower = source.to_ascii_lowercase();
    let code = match lower.as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "space" => KeyCode::Char(' '),
        _ if lower.starts_with('f') && lower.len() > 1 => {
            let number = lower[1..]
                .parse::<u8>()
                .map_err(|_| format!("unknown key '{source}'"))?;
            if !(1..=24).contains(&number) {
                return Err("function keys must be between F1 and F24".into());
            }
            KeyCode::F(number)
        }
        _ if source.chars().count() == 1 => KeyCode::Char(source.chars().next().unwrap()),
        _ => return Err(format!("unknown key '{source}'")),
    };
    Ok(code)
}

impl fmt::Display for KeyBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            write!(formatter, "Ctrl+")?;
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            write!(formatter, "Alt+")?;
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            write!(formatter, "Shift+")?;
        }
        write!(formatter, "{}", key_name(self.code))
    }
}

fn key_name(code: KeyCode) -> String {
    match code {
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => "BackTab".into(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::Delete => "Delete".into(),
        KeyCode::Insert => "Insert".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PageUp".into(),
        KeyCode::PageDown => "PageDown".into(),
        KeyCode::F(number) => format!("F{number}"),
        KeyCode::Char(' ') => "Space".into(),
        KeyCode::Char(c) if c.is_ascii_alphabetic() => c.to_ascii_uppercase().to_string(),
        KeyCode::Char(c) => c.to_string(),
        other => format!("{other:?}"),
    }
}

/// One non-fatal problem found while resolving `[keymap]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeymapWarning {
    pub entry: String,
    pub reason: String,
}

/// One public command exposed to configuration and shortcut-aware UI.
#[derive(Debug, Clone)]
pub struct BindingDescriptor {
    pub id: &'static str,
    pub action: Action,
    scopes: Scopes,
    defaults: &'static [&'static str],
}

impl BindingDescriptor {
    fn parsed_defaults(&self) -> Vec<KeyBinding> {
        self.defaults
            .iter()
            .map(|binding| binding.parse().expect("registry binding is valid"))
            .collect()
    }
}

#[derive(Debug, Clone)]
struct Override {
    id: &'static str,
    action: Action,
    scopes: Scopes,
    bindings: Vec<KeyBinding>,
}

/// Startup-resolved keyboard configuration.
#[derive(Debug, Clone, Default)]
pub struct ResolvedKeymap {
    overrides: Vec<Override>,
    warnings: Vec<KeymapWarning>,
}

impl ResolvedKeymap {
    pub fn from_table(table: &toml::Table) -> Self {
        let mut resolved = Self::default();
        for (configured_id, value) in table {
            let canonical = alias(configured_id).unwrap_or(configured_id);
            let Some(descriptor) = descriptor(canonical) else {
                resolved.warnings.push(KeymapWarning {
                    entry: configured_id.clone(),
                    reason: "unknown action identifier".into(),
                });
                continue;
            };
            let Some(values) = value.as_array() else {
                resolved.warnings.push(KeymapWarning {
                    entry: configured_id.clone(),
                    reason: "expected a list of shortcut strings".into(),
                });
                continue;
            };
            let mut bindings = Vec::new();
            let mut invalid = None;
            for value in values {
                let Some(source) = value.as_str() else {
                    invalid = Some("every shortcut must be a string".to_string());
                    break;
                };
                match source.parse::<KeyBinding>() {
                    Ok(binding) if !bindings.contains(&binding) => bindings.push(binding),
                    Ok(_) => {}
                    Err(reason) => {
                        invalid = Some(format!("'{source}' is invalid: {reason}"));
                        break;
                    }
                }
            }
            if let Some(reason) = invalid {
                resolved.warnings.push(KeymapWarning {
                    entry: configured_id.clone(),
                    reason,
                });
                continue;
            }

            for binding in &bindings {
                for earlier in &resolved.overrides {
                    if earlier.scopes & descriptor.scopes != 0 && earlier.bindings.contains(binding)
                    {
                        resolved.warnings.push(KeymapWarning {
                            entry: configured_id.clone(),
                            reason: format!(
                                "conflict: '{}' and '{}' both use {}; '{}' wins",
                                earlier.id, descriptor.id, binding, descriptor.id
                            ),
                        });
                    }
                }
                for other in binding_registry() {
                    if other.id != descriptor.id
                        && other.scopes & descriptor.scopes != 0
                        && !table.contains_key(other.id)
                        && other.parsed_defaults().contains(binding)
                    {
                        resolved.warnings.push(KeymapWarning {
                            entry: configured_id.clone(),
                            reason: format!(
                                "conflict: '{}' default and '{}' both use {}; '{}' wins",
                                other.id, descriptor.id, binding, descriptor.id
                            ),
                        });
                    }
                }
            }
            resolved.overrides.push(Override {
                id: descriptor.id,
                action: descriptor.action.clone(),
                scopes: descriptor.scopes,
                bindings,
            });
        }
        resolved
    }

    pub fn warnings(&self) -> &[KeymapWarning] {
        &self.warnings
    }

    pub fn display_bindings(&self, id: &str) -> String {
        let canonical = alias(id).unwrap_or(id);
        let bindings = self
            .overrides
            .iter()
            .rev()
            .find(|entry| entry.id == canonical)
            .map(|entry| entry.bindings.clone())
            .or_else(|| descriptor(canonical).map(BindingDescriptor::parsed_defaults))
            .unwrap_or_default();
        if bindings.is_empty() {
            "Unassigned".into()
        } else {
            bindings
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" / ")
        }
    }

    pub fn is_overridden(&self, id: &str) -> bool {
        let canonical = alias(id).unwrap_or(id);
        self.overrides.iter().any(|entry| entry.id == canonical)
    }

    pub fn action_for_key(
        &self,
        key: KeyEvent,
        mode: &AppMode,
        panel: FocusedPanel,
        editing_commit: bool,
        files_filter_active: bool,
        commit_filter_active: bool,
    ) -> Option<Action> {
        let active = active_scopes(
            mode,
            panel,
            editing_commit,
            files_filter_active,
            commit_filter_active,
        );
        self.overrides
            .iter()
            .rev()
            .find(|entry| {
                entry.scopes & active != 0 && entry.bindings.iter().any(|b| b.matches(key))
            })
            .map(|entry| entry.action.clone())
    }

    pub fn replaces_action(&self, action: &Action) -> bool {
        action_id(action).is_some_and(|id| self.overrides.iter().any(|entry| entry.id == id))
    }
}

fn active_scopes(
    mode: &AppMode,
    panel: FocusedPanel,
    editing_commit: bool,
    files_filter_active: bool,
    commit_filter_active: bool,
) -> Scopes {
    let local = match mode {
        AppMode::Normal if editing_commit && panel == FocusedPanel::CommitDetail => EDITOR,
        AppMode::Normal if files_filter_active && panel == FocusedPanel::Files => INPUT,
        AppMode::Normal if commit_filter_active && panel == FocusedPanel::Graph => INPUT,
        AppMode::Normal => match panel {
            FocusedPanel::Graph => GRAPH,
            FocusedPanel::Files => FILES,
            FocusedPanel::CommitDetail => DETAIL,
        },
        AppMode::Help => HELP,
        AppMode::Input { action, .. } if *action == crate::app::InputAction::Search => SEARCH,
        AppMode::Input { .. } => INPUT,
        AppMode::Confirm { .. } => CONFIRM,
        AppMode::RebasePlan { .. } => REBASE_PLAN,
        AppMode::Settings { .. } => SETTINGS,
        AppMode::PrThread => PR_THREAD,
        AppMode::PrCompose { .. } | AppMode::IssueCompose { .. } => COMPOSE,
        AppMode::IssueList => ISSUE_LIST,
        AppMode::IssueDetail => ISSUE_DETAIL,
        AppMode::IssueLabelPicker { .. } | AppMode::IssueLabelFilter { .. } => ISSUE_LABELS,
        AppMode::CiChecks => CI_CHECKS,
        AppMode::BranchFilter { .. } => BRANCH_FILTER,
        AppMode::FileDiff { .. } => FILE_DIFF,
        AppMode::CommandPalette { .. } => PALETTE,
        _ => MENU,
    };
    GLOBAL | local
}

fn alias(id: &str) -> Option<&'static str> {
    match id {
        // Compatibility name used by early development builds.
        "command-palette" | "open-palette" => Some("open-command-palette"),
        _ => None,
    }
}

fn descriptor(id: &str) -> Option<&'static BindingDescriptor> {
    binding_registry().iter().find(|entry| entry.id == id)
}

fn action_id(action: &Action) -> Option<&'static str> {
    binding_registry()
        .iter()
        .find(|entry| entry.action == *action)
        .map(|entry| entry.id)
}

pub fn public_action_id(action: &Action) -> Option<&'static str> {
    action_id(action)
}

macro_rules! registry {
    ($(($id:literal, $action:ident, $scopes:expr, [$($default:literal),* $(,)?])),* $(,)?) => {{
        static REGISTRY: std::sync::OnceLock<Vec<BindingDescriptor>> = std::sync::OnceLock::new();
        REGISTRY.get_or_init(|| vec![$(BindingDescriptor {
            id: $id,
            action: Action::$action,
            scopes: $scopes,
            defaults: &[$($default),*],
        }),*])
    }};
}

/// Stable public action inventory. Payload-carrying text insertion/movement
/// variants and mouse events are intentionally absent.
pub fn binding_registry() -> &'static [BindingDescriptor] {
    registry![
        ("force-quit", ForceQuit, ALL, ["Ctrl+Q"]),
        ("report-keifu-issue", ReportKeifuIssue, ALL, ["Alt+K"]),
        ("new-issue", NewIssue, ALL | ISSUE_LIST, ["Alt+I"]),
        ("toggle-debug-keys", ToggleDebugKeys, ALL, ["F12"]),
        ("toggle-layout", ToggleLayout, ALL, ["Alt+/"]),
        ("full-update", FullUpdate, NORMAL, ["F5"]),
        (
            "open-command-palette",
            OpenCommandPalette,
            NORMAL,
            ["Ctrl+P", "Ctrl+Alt+P", ":"]
        ),
        ("search", Search, GRAPH, ["Ctrl+F", "/"]),
        (
            "start-commit-filter",
            StartCommitFilter,
            GRAPH,
            ["Ctrl+Shift+F"]
        ),
        ("open-settings", OpenSettings, NORMAL, ["Ctrl+,", ","]),
        ("open-issue-list", OpenIssueList, NORMAL, ["Shift+I"]),
        ("toggle-files-pane", ToggleFilesPane, NORMAL, ["Shift+F"]),
        ("toggle-commit-pane", ToggleCommitPane, NORMAL, ["Shift+C"]),
        ("panel-left", PanelLeft, NORMAL, ["Left", "Shift+BackTab"]),
        ("panel-right", PanelRight, NORMAL, ["Right", "Tab"]),
        (
            "move-up",
            MoveUp,
            GRAPH
                | FILES
                | DETAIL
                | MENU
                | REBASE_PLAN
                | SETTINGS
                | PR_THREAD
                | ISSUE_LIST
                | ISSUE_DETAIL
                | ISSUE_LABELS
                | CI_CHECKS
                | BRANCH_FILTER
                | PALETTE,
            ["Up", "k"]
        ),
        (
            "move-down",
            MoveDown,
            GRAPH
                | FILES
                | DETAIL
                | MENU
                | REBASE_PLAN
                | SETTINGS
                | PR_THREAD
                | ISSUE_LIST
                | ISSUE_DETAIL
                | ISSUE_LABELS
                | CI_CHECKS
                | BRANCH_FILTER
                | PALETTE,
            ["Down", "j"]
        ),
        (
            "page-up",
            PageUp,
            GRAPH | FILES | DETAIL | PR_THREAD | ISSUE_LIST | ISSUE_DETAIL | CI_CHECKS | FILE_DIFF,
            ["PageUp", "Ctrl+U"]
        ),
        (
            "page-down",
            PageDown,
            GRAPH | FILES | DETAIL | PR_THREAD | ISSUE_LIST | ISSUE_DETAIL | CI_CHECKS | FILE_DIFF,
            ["PageDown", "Ctrl+D"]
        ),
        (
            "go-to-top",
            GoToTop,
            GRAPH | FILES | DETAIL | PR_THREAD | ISSUE_LIST | ISSUE_DETAIL | CI_CHECKS,
            ["Home", "g"]
        ),
        (
            "go-to-bottom",
            GoToBottom,
            GRAPH | FILES | PR_THREAD | ISSUE_LIST | ISSUE_DETAIL | CI_CHECKS,
            ["End", "Shift+G"]
        ),
        ("same-lane-up", SameLaneUp, GRAPH, ["Ctrl+Up"]),
        ("same-lane-down", SameLaneDown, GRAPH, ["Ctrl+Down"]),
        ("jump-to-head", JumpToHead, GRAPH, ["@"]),
        ("next-branch", NextBranch, GRAPH, ["]"]),
        ("previous-branch", PrevBranch, GRAPH, ["["]),
        ("open-commit-menu", OpenCommitMenu, GRAPH, ["Enter"]),
        ("mark-for-compare", MarkForCompare, GRAPH, ["m"]),
        ("jump-to-merge-base", JumpToMergeBase, GRAPH, ["^"]),
        ("undo-last-operation", UndoLastOp, GRAPH, ["Ctrl+Z"]),
        ("open-pr", OpenPr, GRAPH | PR_THREAD | CI_CHECKS, ["o"]),
        ("open-ci-checks", OpenCiChecks, GRAPH, ["c"]),
        ("open-pr-thread", OpenPrThread, GRAPH, ["v"]),
        ("open-review-picker", OpenReviewPicker, PR_THREAD, ["r"]),
        ("open-metadata-menu", OpenMetadataMenu, GRAPH, ["Shift+M"]),
        ("toggle-trace", ToggleTrace, GRAPH, ["t"]),
        ("shrink-graph-width", ShrinkGraphWidth, GRAPH, ["<"]),
        ("widen-graph-width", WidenGraphWidth, GRAPH, [">"]),
        ("create-branch", CreateBranch, GRAPH, ["b"]),
        ("delete-branch", DeleteBranch, GRAPH, ["d"]),
        ("fetch", Fetch, GRAPH, ["f"]),
        ("pull", Pull, GRAPH, ["l"]),
        ("push", Push, GRAPH, ["Shift+P"]),
        (
            "open-file-diff",
            OpenFileDiff,
            GRAPH | FILES,
            ["Space", "Enter"]
        ),
        ("open-branch-filter", OpenBranchFilter, GRAPH, ["Shift+B"]),
        (
            "toggle-remote-branches",
            ToggleRemoteBranches,
            GRAPH,
            ["Shift+O"]
        ),
        (
            "toggle-merged-branches",
            ToggleMergedBranches,
            GRAPH,
            ["Shift+H"]
        ),
        ("refresh", Refresh, GRAPH, ["Shift+R"]),
        ("toggle-help", ToggleHelp, NORMAL | HELP, ["?"]),
        ("quit", Quit, GRAPH, ["Esc"]),
        ("toggle-stage", ToggleStage, FILES, ["s"]),
        ("stage-all", StageAll, FILES, ["Shift+S"]),
        ("unstage-all", UnstageAll, FILES, ["Shift+U"]),
        ("add-to-gitignore", AddToGitignore, FILES, ["i"]),
        ("archive-file", ArchiveFile, FILES, ["v"]),
        ("trash-file", TrashFile, FILES, ["Delete"]),
        (
            "undo-last-file-operation",
            UndoLastFileOp,
            FILES,
            ["Ctrl+Z"]
        ),
        ("toggle-folder-view", ToggleFolderView, FILES, ["f"]),
        ("restore-file", RestoreFile, FILES, ["r"]),
        ("next-conflict", NextConflict, FILES, ["]"]),
        ("previous-conflict", PrevConflict, FILES, ["["]),
        ("accept-ours", AcceptOurs, FILES, ["o"]),
        ("accept-theirs", AcceptTheirs, FILES, ["t"]),
        ("continue-operation", ContinueOperation, FILES, ["c"]),
        ("abort-operation", AbortOperation, FILES, ["Shift+A"]),
        ("open-with-default", OpenWithDefault, FILES, ["Space"]),
        ("copy-path", CopyPath, FILES, ["y"]),
        ("file-history", FileHistory, FILES, ["h"]),
        ("start-files-filter", StartFilesFilter, FILES, ["/"]),
        ("focus-graph", FocusGraph, FILES | DETAIL, ["Esc"]),
        ("start-editing", StartEditing, DETAIL, ["Enter"]),
        ("amend-commit", AmendCommit, DETAIL | EDITOR, ["Ctrl+Enter"]),
        ("stash-staged", StashStaged, DETAIL | EDITOR, ["Ctrl+S"]),
        (
            "toggle-commit-detail-wrap",
            ToggleCommitDetailWrap,
            DETAIL,
            ["Ctrl+Alt+W"]
        ),
        ("commit-changes", CommitChanges, EDITOR, ["Enter"]),
        ("stop-editing", StopEditing, EDITOR, ["Esc"]),
        (
            "editor-newline",
            EditorNewline,
            EDITOR | COMPOSE,
            ["Shift+Enter"]
        ),
        (
            "editor-backspace",
            EditorBackspace,
            EDITOR | COMPOSE,
            ["Backspace"]
        ),
        ("editor-delete", EditorDelete, EDITOR | COMPOSE, ["Delete"]),
        (
            "editor-backspace-word",
            EditorBackspaceWord,
            EDITOR,
            ["Ctrl+Backspace", "Alt+Backspace"]
        ),
        (
            "editor-delete-word",
            EditorDeleteWord,
            EDITOR,
            ["Ctrl+Delete", "Alt+D"]
        ),
        ("editor-kill-line", EditorKillLine, EDITOR, ["Ctrl+K"]),
        (
            "menu-select",
            MenuSelect,
            MENU | SETTINGS | ISSUE_LABELS | CI_CHECKS | BRANCH_FILTER | PALETTE,
            ["Enter"]
        ),
        (
            "cancel",
            Cancel,
            INPUT
                | SEARCH
                | CONFIRM
                | MENU
                | SETTINGS
                | PR_THREAD
                | ISSUE_LIST
                | ISSUE_DETAIL
                | ISSUE_LABELS
                | COMPOSE
                | CI_CHECKS
                | BRANCH_FILTER
                | FILE_DIFF
                | PALETTE,
            ["Esc", "q"]
        ),
        (
            "confirm",
            Confirm,
            INPUT | SEARCH | CONFIRM | REBASE_PLAN | BRANCH_FILTER,
            ["Enter", "y"]
        ),
        (
            "confirm-delete-branch-and-remote",
            ConfirmDeleteBranchAndRemote,
            CONFIRM,
            ["Ctrl+Enter", "Shift+R"]
        ),
        (
            "input-backspace",
            InputBackspace,
            INPUT | SEARCH | MENU | SETTINGS | BRANCH_FILTER | PALETTE,
            ["Backspace"]
        ),
        (
            "input-backspace-word",
            InputBackspaceWord,
            INPUT | SEARCH | MENU | SETTINGS | BRANCH_FILTER | PALETTE,
            ["Ctrl+Backspace", "Alt+Backspace", "Ctrl+H"]
        ),
        (
            "input-clear-line",
            InputClearLine,
            INPUT | SEARCH | MENU | SETTINGS | BRANCH_FILTER | PALETTE,
            ["Ctrl+U"]
        ),
        (
            "select-all",
            SelectAll,
            ISSUE_LABELS | BRANCH_FILTER,
            ["Ctrl+A"]
        ),
        (
            "select-none",
            SelectNone,
            ISSUE_LABELS | BRANCH_FILTER,
            ["Ctrl+O"]
        ),
        (
            "rebase-move-commit-up",
            RebaseMoveCommitUp,
            REBASE_PLAN,
            ["Shift+K"]
        ),
        (
            "rebase-move-commit-down",
            RebaseMoveCommitDown,
            REBASE_PLAN,
            ["Shift+J"]
        ),
        ("rebase-pick", RebasePick, REBASE_PLAN, ["P"]),
        ("rebase-squash", RebaseSquash, REBASE_PLAN, ["s"]),
        ("rebase-fixup", RebaseFixup, REBASE_PLAN, ["f"]),
        ("rebase-reword", RebaseReword, REBASE_PLAN, ["r"]),
        ("rebase-drop", RebaseDrop, REBASE_PLAN, ["d"]),
        ("help-scroll-up", HelpScrollUp, HELP, ["Up", "k"]),
        ("help-scroll-down", HelpScrollDown, HELP, ["Down", "j"]),
        ("help-page-up", HelpPageUp, HELP, ["PageUp"]),
        ("help-page-down", HelpPageDown, HELP, ["PageDown"]),
        ("help-scroll-to-top", HelpScrollToTop, HELP, ["Home"]),
        ("help-scroll-to-bottom", HelpScrollToBottom, HELP, ["End"]),
        ("toggle-status-bar", ToggleStatusBar, HELP, ["s"]),
        ("search-select-up", SearchSelectUp, SEARCH, ["Up", "Ctrl+K"]),
        (
            "search-select-down",
            SearchSelectDown,
            SEARCH,
            ["Down", "Ctrl+J"]
        ),
        (
            "search-select-up-quiet",
            SearchSelectUpQuiet,
            SEARCH,
            ["Shift+BackTab"]
        ),
        (
            "search-select-down-quiet",
            SearchSelectDownQuiet,
            SEARCH,
            ["Tab"]
        ),
        (
            "submit-compose",
            SubmitCompose,
            COMPOSE,
            ["Ctrl+S", "Ctrl+Enter"]
        ),
        ("external-edit", ExternalEdit, COMPOSE, ["Ctrl+E"]),
        (
            "toggle-issue-clipboard-image",
            ToggleIssueClipboardImage,
            COMPOSE,
            ["Tab"]
        ),
        (
            "refresh-issues",
            RefreshIssues,
            ISSUE_LIST | ISSUE_DETAIL,
            ["r"]
        ),
        (
            "cycle-issue-filter",
            CycleIssueFilter,
            ISSUE_LIST,
            ["Tab", "f"]
        ),
        (
            "open-issue-label-filter",
            OpenIssueLabelFilter,
            ISSUE_LIST,
            ["t"]
        ),
        (
            "toggle-unblocked-only",
            ToggleUnblockedOnly,
            ISSUE_LIST,
            ["u"]
        ),
        ("open-issue-detail", OpenIssueDetail, ISSUE_LIST, ["Enter"]),
        ("edit-issue", EditIssue, ISSUE_DETAIL, ["e"]),
        ("comment-on-issue", CommentOnIssue, ISSUE_DETAIL, ["c"]),
        ("toggle-issue-state", ToggleIssueState, ISSUE_DETAIL, ["x"]),
        (
            "edit-issue-labels",
            EditIssueLabels,
            ISSUE_LIST | ISSUE_DETAIL,
            ["l"]
        ),
        (
            "edit-issue-assignees",
            EditIssueAssignees,
            ISSUE_DETAIL,
            ["a"]
        ),
        (
            "toggle-issue-label",
            ToggleIssueLabel,
            ISSUE_LABELS,
            ["Space"]
        ),
        (
            "open-issue-in-browser",
            OpenIssueInBrowser,
            ISSUE_LIST | ISSUE_DETAIL,
            ["o"]
        ),
        ("scroll-up", ScrollUp, FILE_DIFF, ["Up"]),
        ("scroll-down", ScrollDown, FILE_DIFF, ["Down"]),
        ("scroll-page-up", ScrollPageUp, FILE_DIFF, ["Ctrl+U"]),
        ("scroll-page-down", ScrollPageDown, FILE_DIFF, ["Ctrl+D"]),
        ("scroll-to-top", ScrollToTop, FILE_DIFF, ["g", "Home"]),
        (
            "scroll-to-bottom",
            ScrollToBottom,
            FILE_DIFF,
            ["Shift+G", "End"]
        ),
        ("scroll-left", ScrollLeft, FILE_DIFF, ["h", "Left"]),
        ("scroll-right", ScrollRight, FILE_DIFF, ["l", "Right"]),
        ("scroll-to-line-start", ScrollToLineStart, FILE_DIFF, ["0"]),
        ("next-hunk", NextHunk, FILE_DIFF, ["]"]),
        ("previous-hunk", PrevHunk, FILE_DIFF, ["["]),
        ("next-file", NextFile, FILE_DIFF, ["n"]),
        ("previous-file", PrevFile, FILE_DIFF, ["Shift+N"]),
        ("stage-hunk", StageHunk, FILE_DIFF, ["s"]),
        ("unstage-hunk", UnstageHunk, FILE_DIFF, ["u"]),
        ("discard-hunk", DiscardHunk, FILE_DIFF, ["x"]),
        (
            "toggle-diff-wrap",
            ToggleDiffWrap,
            FILE_DIFF,
            ["Ctrl+Alt+W"]
        ),
        ("create-pull-request", CreatePullRequest, GRAPH, []),
        ("merge-pull-request", MergePullRequest, GRAPH, []),
        ("load-more-commits", LoadMoreCommits, GRAPH, []),
        ("load-all-commits", LoadAllCommits, GRAPH, []),
    ]
}

/// Useful for documentation and compatibility tests.
pub fn aliases() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        ("command-palette", "open-command-palette"),
        ("open-palette", "open-command-palette"),
    ])
}
