use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use git2::Oid;
use keifu::app::{
    AppMode, CommitMenuItem, ComposePurpose, ConfirmAction, FocusedPanel, InputAction,
    IssueComposePurpose, IssueCreateTarget, RemoteOp, TagAction,
};
use keifu::diff_cache::DiffTarget;
use keifu::git::{FileChangeKind, FileDiffContent};
use keifu::keybindings::{map_key_to_action, map_key_to_action_with_keymap};
use keifu::keymap::{binding_registry, ResolvedKeymap};

struct RoutingContext {
    name: &'static str,
    mode: AppMode,
    panel: FocusedPanel,
    editing_commit: bool,
    files_filter_active: bool,
    commit_filter_active: bool,
}

impl RoutingContext {
    fn mode(name: &'static str, mode: AppMode) -> Self {
        Self {
            name,
            mode,
            panel: FocusedPanel::Graph,
            editing_commit: false,
            files_filter_active: false,
            commit_filter_active: false,
        }
    }

    fn normal(name: &'static str, panel: FocusedPanel) -> Self {
        Self {
            name,
            mode: AppMode::Normal,
            panel,
            editing_commit: false,
            files_filter_active: false,
            commit_filter_active: false,
        }
    }
}

fn routing_contexts() -> Vec<RoutingContext> {
    let mut contexts = vec![
        RoutingContext::normal("normal graph", FocusedPanel::Graph),
        RoutingContext::normal("normal files", FocusedPanel::Files),
        RoutingContext::normal("normal commit detail", FocusedPanel::CommitDetail),
        RoutingContext::mode("help", AppMode::Help),
        RoutingContext::mode(
            "search input",
            AppMode::Input {
                title: String::new(),
                input: String::new(),
                action: InputAction::Search,
            },
        ),
        RoutingContext::mode(
            "generic input",
            AppMode::Input {
                title: String::new(),
                input: String::new(),
                action: InputAction::CreateBranch,
            },
        ),
        RoutingContext::mode(
            "confirm",
            AppMode::Confirm {
                message: String::new(),
                action: ConfirmAction::Push,
            },
        ),
        RoutingContext::mode(
            "commit menu",
            AppMode::CommitMenu {
                items: Vec::<CommitMenuItem>::new(),
                selected: 0,
                filter: String::new(),
            },
        ),
        RoutingContext::mode("rebase plan", AppMode::RebasePlan { cursor: 0 }),
        RoutingContext::mode("metadata menu", AppMode::MetadataMenu { selected: 0 }),
        RoutingContext::mode(
            "settings",
            AppMode::Settings {
                selected: 0,
                editing: None,
                query: String::new(),
            },
        ),
        RoutingContext::mode("pull divergence", AppMode::PullDivergence { selected: 0 }),
        RoutingContext::mode("CI checks", AppMode::CiChecks),
        RoutingContext::mode("PR thread", AppMode::PrThread),
        RoutingContext::mode(
            "PR compose",
            AppMode::PrCompose {
                purpose: ComposePurpose::CreatePr,
            },
        ),
        RoutingContext::mode(
            "PR merge picker",
            AppMode::PrMergePicker {
                number: 1,
                selected: 0,
            },
        ),
        RoutingContext::mode(
            "PR review picker",
            AppMode::PrReviewPicker {
                number: 1,
                selected: 0,
            },
        ),
        RoutingContext::mode("issue list", AppMode::IssueList),
        RoutingContext::mode("issue detail", AppMode::IssueDetail),
        RoutingContext::mode(
            "issue compose",
            AppMode::IssueCompose {
                purpose: IssueComposePurpose::NewIssue {
                    target: IssueCreateTarget::CurrentRepository,
                },
            },
        ),
        RoutingContext::mode(
            "issue label picker",
            AppMode::IssueLabelPicker {
                number: 1,
                selected: 0,
            },
        ),
        RoutingContext::mode(
            "issue label filter",
            AppMode::IssueLabelFilter { selected: 0 },
        ),
        RoutingContext::mode(
            "branch filter",
            AppMode::BranchFilter {
                filter: String::new(),
                selected: 0,
                all_branches: vec![],
            },
        ),
        RoutingContext::mode(
            "generic picker",
            AppMode::BranchPicker {
                branches: vec![],
                selected: 0,
            },
        ),
        RoutingContext::mode(
            "branch delete picker",
            AppMode::BranchDeletePicker {
                branches: vec![],
                selected: 0,
            },
        ),
        RoutingContext::mode(
            "tag picker",
            AppMode::TagPicker {
                tags: vec![],
                selected: 0,
                action: TagAction::Delete,
            },
        ),
        RoutingContext::mode(
            "remote picker",
            AppMode::RemotePicker {
                remotes: vec![],
                selected: 0,
                op: RemoteOp::Fetch,
            },
        ),
        RoutingContext::mode(
            "file diff",
            AppMode::FileDiff {
                diff_target: DiffTarget::Uncommitted,
                file_index: 0,
                file_list: vec![],
                content: FileDiffContent {
                    path: PathBuf::new(),
                    kind: FileChangeKind::Modified,
                    is_binary: false,
                    hunks: vec![],
                    total_additions: 0,
                    total_deletions: 0,
                },
                rendered_lines: vec![],
                hunk_positions: vec![],
                scroll_offset: 0,
                horizontal_offset: 0,
                max_line_width: 0,
                total_lines: 0,
            },
        ),
        RoutingContext::mode(
            "file history",
            AppMode::FileHistory {
                path: PathBuf::new(),
                entries: vec![keifu::app::FileHistoryEntry {
                    oid: Oid::zero(),
                    short_id: String::new(),
                    date: String::new(),
                    subject: String::new(),
                }],
                selected: 0,
            },
        ),
        RoutingContext::mode(
            "command palette",
            AppMode::CommandPalette {
                query: String::new(),
                selected: 0,
            },
        ),
    ];

    contexts.push(RoutingContext {
        name: "commit editor",
        mode: AppMode::Normal,
        panel: FocusedPanel::CommitDetail,
        editing_commit: true,
        files_filter_active: false,
        commit_filter_active: false,
    });
    contexts.push(RoutingContext {
        name: "files filter",
        mode: AppMode::Normal,
        panel: FocusedPanel::Files,
        editing_commit: false,
        files_filter_active: true,
        commit_filter_active: false,
    });
    contexts.push(RoutingContext {
        name: "commit filter",
        mode: AppMode::Normal,
        panel: FocusedPanel::Graph,
        editing_commit: false,
        files_filter_active: false,
        commit_filter_active: true,
    });
    contexts
}

#[test]
fn every_legacy_action_remains_reachable_where_its_default_was_routed() {
    let replacement = KeyEvent::new(KeyCode::F(13), KeyModifiers::NONE);
    let contexts = routing_contexts();
    let mut failures = vec![];

    for descriptor in binding_registry() {
        let table = format!("{} = [\"F13\"]", descriptor.id)
            .parse::<toml::Table>()
            .unwrap();
        let keymap = ResolvedKeymap::from_table(&table);
        for context in &contexts {
            let was_routed = descriptor.default_bindings().into_iter().any(|binding| {
                let mut codes = vec![binding.code];
                if let KeyCode::Char(character) = binding.code {
                    codes.push(KeyCode::Char(character.to_ascii_lowercase()));
                    codes.push(KeyCode::Char(character.to_ascii_uppercase()));
                }
                codes.into_iter().any(|code| {
                    map_key_to_action(
                        KeyEvent::new(code, binding.modifiers),
                        &context.mode,
                        context.panel,
                        context.editing_commit,
                        context.files_filter_active,
                        context.commit_filter_active,
                    ) == Some(descriptor.action.clone())
                })
            });
            if was_routed
                && map_key_to_action_with_keymap(
                    replacement,
                    &context.mode,
                    context.panel,
                    context.editing_commit,
                    context.files_filter_active,
                    context.commit_filter_active,
                    &keymap,
                ) != Some(descriptor.action.clone())
            {
                failures.push(format!("{} in {}", descriptor.id, context.name));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "unreachable overrides:\n{}",
        failures.join("\n")
    );
}
