//! Command palette: open, build candidates, rank, and execute selections.

use super::*;
use crate::palette::{
    command_registry, rank, Candidate, ContextualPaletteSnapshot, PaletteAction,
    PaletteCommitTarget, PaletteContext, PaletteKind, PaletteResults, PALETTE_CAP,
};

impl App {
    /// Open the fuzzy command palette with an empty query.
    pub(crate) fn open_command_palette(&mut self) {
        self.command_palette_snapshot = Some(ContextualPaletteSnapshot {
            target: self.palette_commit_target(),
            items: self.available_commit_menu_items(),
        });
        self.mode = AppMode::CommandPalette {
            query: String::new(),
            selected: 0,
        };
    }

    fn palette_commit_target(&self) -> Option<PaletteCommitTarget> {
        self.selected_commit_node().map(|node| PaletteCommitTarget {
            oid: node.commit.as_ref().map(|commit| commit.oid),
            is_stash: node.is_stash,
            selected_branch: self.selected_branch_name().map(str::to_owned),
        })
    }

    /// The eligibility context that gates which commands the registry offers.
    fn palette_context(&self) -> PaletteContext {
        let has_selected_commit = self
            .selected_commit_node()
            .is_some_and(|node| node.commit.is_some() && !node.is_stash && !node.is_uncommitted);
        PaletteContext {
            has_selected_commit,
            // PR actions are commit-contextual too; a stash's synthetic
            // commit payload must not make ordinary registry actions appear.
            can_create_pr: has_selected_commit && self.can_offer_create_pr(),
            selected_has_open_pr: has_selected_commit && self.selected_commit_has_open_pr(),
            can_load_more: !self.all_commits_loaded,
            can_undo: !self.undo_ledger.is_empty(),
        }
    }

    /// Build every palette row: eligible commands, then all branches, then all
    /// commits. Ranking (in [`crate::palette::rank`]) orders them.
    fn palette_candidates(&self) -> Vec<Candidate> {
        let mut out = Vec::new();

        // Commands — registry order is the within-kind tiebreak.
        let ctx = self.palette_context();
        for (i, e) in command_registry().into_iter().enumerate() {
            if !(e.eligible)(&ctx) {
                continue;
            }
            out.push(Candidate {
                kind: PaletteKind::Command,
                label: e.label.to_string(),
                hint: crate::keymap::public_action_id(&e.action)
                    .and_then(|id| {
                        let display = self.keymap.display_bindings(id);
                        (self.keymap.is_overridden(id) || display != "Unassigned")
                            .then_some(display)
                    })
                    .or_else(|| e.hint.map(str::to_string)),
                match_text: e.label.to_string(),
                action: PaletteAction::Dispatch(e.action),
                order: i,
            });
        }

        // Enter-menu operations are context-sensitive: the same builder that
        // supplies the menu determines which palette rows exist. This keeps
        // unavailable operations out of the palette rather than offering a
        // command that would immediately fail or do nothing.
        let registry_len = out.len();
        let (items, target) = match self.command_palette_snapshot.as_ref() {
            Some(snapshot) => (snapshot.items.clone(), snapshot.target.clone()),
            None => (
                self.available_commit_menu_items(),
                self.palette_commit_target(),
            ),
        };
        for (i, item) in items.into_iter().enumerate() {
            let Some(target) = target.clone() else {
                continue;
            };
            let label = item.label().to_string();
            let contextual_action = PaletteAction::CommitMenuItem { item, target };
            if let Some(candidate) = out.iter_mut().find(|candidate| candidate.label == label) {
                // Contextual rows supersede same-labelled registry shortcuts.
                // In particular, Create branch here and Pull must retain the
                // palette-open target fingerprint rather than dispatching a
                // current-selection action directly.
                candidate.action = contextual_action;
                continue;
            }
            out.push(Candidate {
                kind: PaletteKind::Command,
                label: label.clone(),
                hint: Some("Enter".to_string()),
                match_text: label,
                action: contextual_action,
                order: registry_len + i,
            });
        }

        // Branches — "Checkout <name>", remote branches marked with the cloud
        // glyph (same convention as the graph chips). Match on the bare name.
        for b in &self.branches {
            let display = if b.is_remote {
                format!("{} {}", crate::ui::graph_view::REMOTE_ONLY_ICON, b.name)
            } else {
                b.name.clone()
            };
            out.push(Candidate {
                kind: PaletteKind::Branch,
                label: format!("Checkout {display}"),
                hint: None,
                match_text: b.name.clone(),
                action: PaletteAction::Checkout {
                    name: b.name.clone(),
                    is_remote: b.is_remote,
                },
                order: 0, // ties broken alphabetically by label
            });
        }

        // Commits — subject + short hash, tiebreak by row index (recency).
        for (idx, node) in self.graph_layout.nodes.iter().enumerate() {
            let Some(commit) = &node.commit else {
                continue;
            };
            let subject = commit.message.lines().next().unwrap_or("").to_string();
            out.push(Candidate {
                kind: PaletteKind::Commit,
                label: subject.clone(),
                hint: Some(commit.short_id.clone()),
                match_text: format!("{} {}", subject, commit.short_id),
                action: PaletteAction::JumpToCommit(idx),
                order: idx,
            });
        }

        out
    }

    /// Ranked, capped palette results for `query` — shared by the handler and
    /// the widget so navigation and rendering stay in sync.
    pub fn palette_results(&self, query: &str) -> PaletteResults {
        rank(query, self.palette_candidates(), PALETTE_CAP)
    }

    pub(crate) fn handle_command_palette_action(&mut self, action: Action) -> Result<()> {
        let AppMode::CommandPalette { query, selected } = &self.mode else {
            return Ok(());
        };
        let query = query.clone();
        let selected = *selected;

        match action {
            Action::MoveUp | Action::MoveDown => {
                let count = self.palette_results(&query).items.len();
                if count == 0 {
                    return Ok(());
                }
                let new = if matches!(action, Action::MoveUp) {
                    cyclic_prev(selected, count)
                } else {
                    cyclic_next(selected, count)
                };
                self.mode = AppMode::CommandPalette {
                    query,
                    selected: new,
                };
            }
            Action::InputChar(c) => {
                let mut query = query;
                query.push(c);
                self.mode = AppMode::CommandPalette { query, selected: 0 };
            }
            Action::InputBackspace => {
                let mut query = query;
                query.pop();
                self.mode = AppMode::CommandPalette { query, selected: 0 };
            }
            Action::InputBackspaceWord => {
                let mut query = query;
                crate::text_editor::pop_word(&mut query);
                self.mode = AppMode::CommandPalette { query, selected: 0 };
            }
            Action::InputClearLine => {
                self.mode = AppMode::CommandPalette {
                    query: String::new(),
                    selected: 0,
                };
            }
            Action::MenuSelect | Action::Confirm => {
                let results = self.palette_results(&query);
                if let Some(item) = results.items.get(selected) {
                    let palette_action = item.action.clone();
                    self.execute_palette_action(palette_action)?;
                }
            }
            Action::Cancel | Action::Quit => {
                self.mode = AppMode::Normal;
                self.command_palette_snapshot = None;
            }
            _ => {}
        }
        Ok(())
    }

    /// Run the chosen palette row and close the palette.
    fn execute_palette_action(&mut self, action: PaletteAction) -> Result<()> {
        match action {
            PaletteAction::Dispatch(inner) => {
                // Registry commands act on the graph/repo: close the palette,
                // focus the graph panel, then dispatch through the normal path.
                self.mode = AppMode::Normal;
                self.command_palette_snapshot = None;
                self.focused_panel = FocusedPanel::Graph;
                self.handle_action(inner)?;
            }
            PaletteAction::CommitMenuItem { item, target } => {
                // This is deliberately the Enter-menu executor, so prompts,
                // confirmations, toasts, and cancellation stay identical.
                self.command_palette_snapshot = None;
                if self.palette_commit_target().as_ref() != Some(&target)
                    || !self.available_commit_menu_items().contains(&item)
                {
                    self.mode = AppMode::Normal;
                    self.show_error(
                        "Selection or repository state changed; reopen command palette".to_string(),
                    );
                    return Ok(());
                }
                self.execute_menu_item(item)?;
            }
            PaletteAction::Checkout { name, is_remote } => {
                // Route through the existing checkout confirmation.
                self.mode = AppMode::Confirm {
                    message: format!("Checkout branch '{name}'?"),
                    action: ConfirmAction::Checkout { name, is_remote },
                };
            }
            PaletteAction::JumpToCommit(idx) => {
                self.mode = AppMode::Normal;
                self.command_palette_snapshot = None;
                self.focused_panel = FocusedPanel::Graph;
                self.select_commit_by_full_idx(idx);
            }
        }
        Ok(())
    }
}
