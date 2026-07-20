//! Repository refresh and diff-cache access.

use super::*;

impl App {
    /// Load the next chunk of commits (or the whole history when `all`),
    /// rebuilding the graph through a full refresh — which re-walks with the
    /// raised limit, bumps `graph_generation`, and restores the selection by
    /// OID. Synchronous: a full 10k rebuild profiles at ~12ms (release), so the
    /// hitch is imperceptible; even a "load all" on a very large repo is a
    /// one-off cost behind an explicit confirm. Toasts the outcome.
    pub fn load_more_commits(&mut self, all: bool) {
        if self.all_commits_loaded {
            self.toast(crate::toast::ToastKind::Info, "All commits already loaded");
            return;
        }
        let before = self.commits.len();
        self.commit_load_limit = if all {
            usize::MAX
        } else {
            self.commit_load_limit.saturating_add(COMMIT_CHUNK)
        };
        if let Err(e) = self.refresh(false) {
            self.report_refresh_error(e);
            return;
        }
        let added = self.commits.len().saturating_sub(before);
        if added == 0 {
            self.toast(crate::toast::ToastKind::Info, "No more commits to load");
        } else {
            self.toast(
                crate::toast::ToastKind::Success,
                format!("Loaded {} more commits ({} total)", added, self.commits.len()),
            );
        }
    }

    /// Auto-load the next chunk when the selection or scroll offset comes within
    /// `AUTOLOAD_THRESHOLD` rows of the last loaded commit. Returns whether a
    /// load happened (so the caller redraws). No-op once all commits are loaded.
    pub fn maybe_autoload_commits(&mut self) -> bool {
        if self.all_commits_loaded {
            return false;
        }
        let rows = self.graph_layout.nodes.len();
        let selected = self.graph_nav.graph_list_state.selected().unwrap_or(0);
        let offset = self.graph_nav.graph_list_state.offset();
        let frontier = selected.max(offset);
        if rows.saturating_sub(frontier) > AUTOLOAD_THRESHOLD {
            return false;
        }
        self.load_more_commits(false);
        true
    }

    /// Refresh after a file operation (stage/unstage/gitignore/archive/trash).
    ///
    /// After the file list reshuffles, selects the next file in the same
    /// section. Falls back to previous in section, then to any file.
    pub fn refresh_after_file_op(&mut self) -> Result<()> {
        // Snapshot the current items and selection BEFORE the refresh.
        // Use files_pane_items() directly — the cache might be stale.
        let old_items = self.files_pane_items();
        let old_idx = self.files_pane.file_selected_index_in(&old_items);
        let old_section = section_of(&old_items, old_idx);

        // Next files in same section (forward until next header).
        // `old_items` may be empty (e.g. undo invoked from a node with no
        // file changes) — the resolved index is 0 even then, so slice
        // defensively.
        let next_in_section: Vec<std::path::PathBuf> = old_items
            .get(old_idx + 1..)
            .unwrap_or_default()
            .iter()
            .take_while(|item| matches!(item, FilesPaneItem::File(_)))
            .filter_map(|item| match item {
                FilesPaneItem::File(f) => Some(f.path.clone()),
                _ => None,
            })
            .collect();

        // Previous files in same section (backward until header)
        let prev_in_section: Vec<std::path::PathBuf> = old_items[..old_idx]
            .iter()
            .rev()
            .take_while(|item| matches!(item, FilesPaneItem::File(_)))
            .filter_map(|item| match item {
                FilesPaneItem::File(f) => Some(f.path.clone()),
                _ => None,
            })
            .collect();

        self.refresh(false)?;
        // Recompute quick diff for the new staging state
        self.diff_cache.set_quick_uncommitted(self.repo.repo());
        // Reclassify the existing full diff's staging status in place
        // (avoids a redundant async reload — line counts don't change).
        // Then seal the cache key so poll() won't trigger a reload.
        self.diff_cache.reclassify_uncommitted_staging(self.working_tree_status.as_ref());
        self.sync_file_list_cache();

        // Find best target: next in same section, then prev, then any file
        let new_items = self.files_pane.display_items();
        let target = next_in_section
            .iter()
            .chain(prev_in_section.iter())
            .find_map(|path| {
                let i = new_items.iter().position(
                    |item| matches!(item, FilesPaneItem::File(f) if f.path == *path),
                )?;
                if section_of(new_items, i) == old_section {
                    Some((path.clone(), old_section.map(|s| s.to_string())))
                } else {
                    None
                }
            });

        if let Some((path, sec)) = target {
            self.files_pane.set_selection(Some(path), sec);
        }
        // Otherwise file_selection keeps its current path; resolve() will
        // find it in whatever section it landed in, or fall back to first file.

        Ok(())
    }

    pub(crate) fn current_diff_target(&self) -> Option<DiffTarget> {
        // An active two-commit comparison overrides the selected node's diff
        // until it's cleared (Esc), so the files pane / detail keep showing the
        // comparison regardless of where the cursor roams in the graph.
        if let Some((old, new)) = self.compare_range {
            return Some(DiffTarget::Range(old, new));
        }

        let node = self
            .graph_nav.graph_list_state
            .selected()
            .and_then(|idx| self.graph_layout.nodes.get(idx))?;

        if node.is_uncommitted {
            Some(DiffTarget::Uncommitted)
        } else {
            node.commit
                .as_ref()
                .map(|commit| DiffTarget::Commit(commit.oid))
        }
    }

    /// Whether the current diff target is the working tree. Used by the files
    /// pane to decide staged/unstaged sectioning — a range/commit diff has no
    /// staging concept even when the selected node happens to be uncommitted.
    pub(crate) fn diff_target_is_uncommitted(&self) -> bool {
        self.current_diff_target() == Some(DiffTarget::Uncommitted)
    }

    /// Returns the new target and whether it changed since the last sync.
    fn sync_selected_diff_target(&mut self) -> (Option<DiffTarget>, bool) {
        let target = self.current_diff_target();
        self.diff_cache.sync_selected_target(target, self.repo.repo())
    }

    /// Refresh repository data
    /// If `force` is true, always clears diff cache (for manual refresh)
    /// If `force` is false, keeps cache when the same content is selected (for auto-refresh)
    /// Reload git state (branches, commits, graph). Timed as `refresh` on the
    /// perf path; the actual work lives in `refresh_inner`.
    pub fn refresh(&mut self, force: bool) -> Result<()> {
        let started = std::time::Instant::now();
        let result = self.refresh_inner(force);
        self.perf.record("refresh", started.elapsed());
        result
    }

    fn refresh_inner(&mut self, force: bool) -> Result<()> {
        // Re-open the libgit2 handle so we observe on-disk state written by other
        // processes since the last refresh: pushes/fetches creating or updating
        // remote-tracking refs, upstream config changes, or pruned refs. A
        // long-lived handle caches this, which otherwise leaves a just-pushed
        // branch looking unpushed. Best-effort: on failure keep the old handle
        // and continue rather than aborting the refresh.
        let _ = self.repo.reopen();

        // Save the current selection state for restoration
        let was_uncommitted_selected = self
            .graph_nav.graph_list_state
            .selected()
            .and_then(|idx| self.graph_layout.nodes.get(idx))
            .is_some_and(|node| node.is_uncommitted);
        let prev_selected_commit_oid = self
            .graph_nav.graph_list_state
            .selected()
            .and_then(|idx| self.graph_layout.nodes.get(idx))
            .and_then(|node| node.commit.as_ref())
            .map(|commit| commit.oid);

        let prev_branch_name = self
            .graph_nav.selected_branch_position
            .and_then(|pos| self.graph_nav.branch_positions.get(pos))
            .map(|(_, name)| name.clone());

        // Get working tree status once and reuse. Report a failure once per
        // episode (latched) so a persistently-failing status query doesn't
        // re-flash the status bar on every periodic refresh; re-arm on success.
        let (working_tree_status, status_message) = Self::working_tree_status_snapshot(&self.repo);
        match status_message {
            Some(message) => {
                if !self.refresh_latches.wt_status {
                    self.refresh_latches.wt_status = true;
                    self.set_message(message);
                }
            }
            None => self.refresh_latches.wt_status = false,
        }
        let uncommitted_count = working_tree_status
            .as_ref()
            .map(|s| s.accurate_file_count());
        self.working_tree_status = working_tree_status;

        // Refresh in-progress operation state (merge/rebase/…) and conflict
        // count so the status bar and conflict keybindings stay accurate.
        self.op_state = self.repo.operation_state();
        self.conflict_count = self.repo.conflicted_count();

        let stashes = self.repo.get_stashes();
        let branches = self.repo.get_branches()?;
        self.remotes = self.repo.remotes();
        // Merged-branch classification runs off the UI thread; this refresh uses
        // the most recently delivered set (`self.merged.branches`) for both the
        // dimmed rendering and the hide filter. When branch tips have moved the
        // set may be one generation stale until the worker returns, at which point
        // `update_merged_classification` triggers another refresh.
        let merged = self.merged.branches.clone();
        // Excluded branches are dropped from the revwalk, so their exclusive
        // commits are removed from the graph — not merely their labels. Three
        // filters compose: the per-branch picker (`hidden_branches`), the
        // show/hide-remotes toggle (every remote-only ref), and the hide-merged
        // toggle (branches already landed on the trunk). A branch is visible iff
        // no filter excludes it.
        let remote_only = if self.hide_remote_branches {
            remote_only_branch_names(&branches)
        } else {
            std::collections::HashSet::new()
        };
        let visible_branches: Vec<BranchInfo> = branches
            .iter()
            .filter(|b| !self.hidden_branches.contains(&b.name))
            .filter(|b| !remote_only.contains(&b.name))
            .filter(|b| !(self.merged.hide && merged.contains(&b.name)))
            .cloned()
            .collect();
        self.branches = branches;
        // Re-run classification against the just-loaded branches (no-op when the
        // inputs are unchanged, per the classifier's signature guard).
        self.kick_merged_classification();
        // The base branch tip may have advanced, changing which PR-branch merges
        // count as base-updates (issue #55); recompute (signature-guarded no-op
        // when unchanged).
        self.recompute_base_update_merges();
        self.commits = self
            .repo
            .get_commits(self.commit_load_limit, &visible_branches, &stashes)?;
        // The whole history is loaded once the walk yields fewer than the limit.
        self.all_commits_loaded = self.commits.len() < self.commit_load_limit;
        let tags = self.repo.get_tags();
        let head_commit_oid = self.repo.head_oid();
        self.graph_layout = build_graph(
            &self.commits,
            &visible_branches,
            &tags,
            &stashes,
            uncommitted_count,
            head_commit_oid,
        );
        // Invalidate the pixel-graph spec cache: the layout changed.
        self.graph_generation = self.graph_generation.wrapping_add(1);
        self.head_name = self.repo.head_name();
        self.head_detached = self.repo.is_head_detached();

        // Rebuild branch positions
        self.graph_nav
            .rebuild_branch_positions(&self.graph_layout, &self.repo.remotes());

        // Restore selection state
        // Check if uncommitted node still exists in the new graph
        let has_uncommitted_node = self
            .graph_layout
            .nodes
            .first()
            .is_some_and(|node| node.is_uncommitted);

        if was_uncommitted_selected && has_uncommitted_node {
            // Restore uncommitted node selection
            self.graph_nav.graph_list_state.select(Some(0));
            self.graph_nav.selected_branch_position = None;
        } else {
            // Restore branch selection if the branch still exists
            self.graph_nav.selected_branch_position = prev_branch_name
                .and_then(|name| self.graph_nav.branch_positions.iter().position(|(_, n)| n == &name));

            // Sync node selection with branch selection
            if let Some(pos) = self.graph_nav.selected_branch_position {
                if let Some((node_idx, _)) = self.graph_nav.branch_positions.get(pos) {
                    self.graph_nav.graph_list_state.select(Some(*node_idx));
                }
            } else if let Some(oid) = prev_selected_commit_oid {
                let node_idx =
                    self.graph_layout.nodes.iter().position(|node| {
                        node.commit.as_ref().is_some_and(|commit| commit.oid == oid)
                    });
                if let Some(idx) = node_idx {
                    self.graph_nav.graph_list_state.select(Some(idx));
                } else if let Some(prev) = self.graph_nav.graph_list_state.selected() {
                    // OID pushed out of range — keep cursor at the nearest
                    // valid row instead of clearing the selection.
                    let max = self.graph_layout.nodes.len().saturating_sub(1);
                    self.graph_nav.graph_list_state.select(Some(prev.min(max)));
                }
            }
        }

        // If no branch is selected but the selected node has branches, pick the first one.
        // This happens after committing from the uncommitted node — the selection lands
        // on the new HEAD commit but selected_branch_position was never set.
        if self.graph_nav.selected_branch_position.is_none() {
            if let Some(selected_idx) = self.graph_nav.graph_list_state.selected() {
                if let Some(pos) = self
                    .graph_nav.branch_positions
                    .iter()
                    .position(|(node_idx, _)| *node_idx == selected_idx)
                {
                    self.graph_nav.selected_branch_position = Some(pos);
                }
            }
        }

        // Handle diff cache based on force flag
        if force {
            self.diff_cache.clear_all();
        } else {
            // Auto-refresh: smart cache - only clear if selection changed
            let selected_oid = self
                .graph_nav.graph_list_state
                .selected()
                .and_then(|idx| self.graph_layout.nodes.get(idx))
                .and_then(|n| n.commit.as_ref())
                .map(|c| c.oid);

            self.diff_cache.invalidate_for_auto_refresh(
                selected_oid,
                was_uncommitted_selected,
                has_uncommitted_node,
                self.working_tree_status.as_ref(),
            );
        }

        // Clear search state on refresh to avoid stale indices
        // Skip if in search mode to prevent clearing active search results
        if !self.is_in_search_mode() {
            self.search_state = SearchState::default();
        }

        // Clamp the selection
        let max_commit = self.graph_layout.nodes.len().saturating_sub(1);
        if let Some(selected) = self.graph_nav.graph_list_state.selected() {
            if selected > max_commit {
                self.graph_nav.graph_list_state.select(Some(max_commit));
            }
        }

        if !self.commit_filter.is_empty() {
            self.recompute_visible_commits();
        }

        Ok(())
    }

    /// Update diff info for the selected node (commit or uncommitted changes, async)
    pub fn update_diff_cache(&mut self) -> bool {
        let (target, target_changed) = self.sync_selected_diff_target();
        let events = self.diff_cache.poll(
            target,
            &self.repo_path,
            self.working_tree_status.as_ref(),
        );
        let has_message = events.message.is_some();
        if let Some(msg) = events.message {
            self.set_message(msg);
        }
        if events.uncommitted_diff_loaded {
            self.sync_file_list_with_uncommitted_diff();
        }
        if events.diff_loaded {
            self.sync_file_list_cache();
        }
        // target_changed means a fresh quick diff was computed and must be rendered
        events.diff_loaded || has_message || target_changed
    }

    /// Get cached diff info for the currently selected node
    pub fn cached_diff(&self) -> Option<&CommitDiffInfo> {
        self.diff_cache.cached_diff(self.current_diff_target())
    }

    /// Get the best available diff: full if cached, otherwise quick file list
    pub fn cached_diff_or_quick(&self) -> Option<&CommitDiffInfo> {
        self.diff_cache.cached_diff_or_quick(self.current_diff_target())
    }

    /// Whether line stats are still loading (full diff not yet available but quick is)
    pub fn is_line_stats_loading(&self) -> bool {
        self.diff_cache.is_line_stats_loading(self.current_diff_target())
    }

    /// Whether diff is loading or pending (debouncing) for the selected node
    pub fn is_diff_loading(&self) -> bool {
        self.diff_cache.is_diff_loading(self.current_diff_target())
    }
}
