//! Command palette: open, build candidates, rank, and execute selections.

use super::*;
use crate::palette::{
    command_registry, rank, Candidate, PaletteAction, PaletteContext, PaletteKind, PaletteResults,
    PALETTE_CAP,
};

impl App {
    /// Open the fuzzy command palette with an empty query.
    pub(crate) fn open_command_palette(&mut self) {
        self.mode = AppMode::CommandPalette {
            query: String::new(),
            selected: 0,
        };
    }

    /// The eligibility context that gates which commands the registry offers.
    fn palette_context(&self) -> PaletteContext {
        let has_selected_commit = self
            .selected_commit_node()
            .and_then(|n| n.commit.as_ref())
            .is_some();
        PaletteContext {
            has_selected_commit,
            can_create_pr: self.can_offer_create_pr(),
            selected_has_open_pr: self.selected_commit_has_open_pr(),
            can_load_more: !self.all_commits_loaded,
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
                hint: e.hint.map(str::to_string),
                match_text: e.label.to_string(),
                action: PaletteAction::Dispatch(e.action),
                order: i,
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
                action: PaletteAction::Checkout(b.name.clone()),
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
                self.focused_panel = FocusedPanel::Graph;
                self.handle_action(inner)?;
            }
            PaletteAction::Checkout(name) => {
                // Route through the existing checkout confirmation.
                self.mode = AppMode::Confirm {
                    message: format!("Checkout branch '{name}'?"),
                    action: ConfirmAction::Checkout(name),
                };
            }
            PaletteAction::JumpToCommit(idx) => {
                self.mode = AppMode::Normal;
                self.focused_panel = FocusedPanel::Graph;
                self.select_commit_by_full_idx(idx);
            }
        }
        Ok(())
    }
}
