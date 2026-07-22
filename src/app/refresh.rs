//! Repository refresh and diff-cache access.

use super::*;

/// The pre-rebuild selection, captured from the old graph so it can be
/// re-resolved against the freshly-built layout (see `restore_selection`).
struct SelectionSnapshot {
    /// The selected node was the synthetic uncommitted-changes node.
    was_uncommitted_selected: bool,
    /// OID of the selected commit, if any (used to re-find the same commit).
    prev_selected_commit_oid: Option<Oid>,
    /// Name of the selected branch, if any (preferred over the OID on restore).
    prev_branch_name: Option<String>,
}

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
        // The working tree may have changed under us (that's what a refresh
        // is for) — re-walk `.archive/` on the next file-list sync.
        self.files_pane.invalidate_archived_cache();
        let result = self.refresh_inner(force);
        self.perf.record("refresh", started.elapsed());
        result
    }

    // ── Refresh phases ─────────────────────────────────────────────────────
    //
    // `refresh_inner` is split into four sequential phases with explicit
    // contracts so each can be reasoned about — and the cheap ones reused — in
    // isolation:
    //
    //   1. `reload_refs`      — pull fresh git state off disk into `self`.
    //   2. `rebuild_graph`    — revwalk + layout from the already-loaded state.
    //   3. `restore_selection`— re-point the cursor onto the equivalent row.
    //   4. `invalidate_caches`— reconcile diff/search caches with the new graph.
    //
    // A full refresh runs all four; merged-classification delivery reruns only
    // 2–4 (via `rebuild_and_restore`), skipping the expensive reload_refs
    // (reopen + `git status` + branch enumeration) since only the merged filter
    // changed.

    fn refresh_inner(&mut self, force: bool) -> Result<()> {
        self.reload_refs(force)?;
        self.rebuild_and_restore(force)
    }

    /// Phase 1 — reload git state from disk into `self`.
    ///
    /// Inputs: `force` (whether this is a manual/forced refresh). Reads on-disk
    /// git state via `self.repo`.
    ///
    /// Writes: `self.working_tree_status`, `op_state`, `conflict_count`,
    /// `branches`, `remotes`; may reopen the repo handle; kicks the background
    /// merged classifier and recomputes the base-update-merge set (both
    /// signature-guarded no-ops when their inputs are unchanged). Sets/clears
    /// the `wt_status` and `reopen` error latches.
    ///
    /// Must NOT touch: the graph layout, the selection, or the diff/search
    /// caches — those are the later phases' responsibility.
    fn reload_refs(&mut self, force: bool) -> Result<()> {
        self.maybe_reopen_repo(force);

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
        self.working_tree_status = working_tree_status;

        // Refresh in-progress operation state (merge/rebase/…) and conflict
        // count so the status bar and conflict keybindings stay accurate.
        self.op_state = self.repo.operation_state();
        self.conflict_count = self.repo.conflicted_count();

        self.branches = self.repo.get_branches()?;
        self.remotes = self.repo.remotes();
        // Re-run classification against the just-loaded branches (no-op when the
        // inputs are unchanged, per the classifier's signature guard).
        self.kick_merged_classification();
        // The base branch tip may have advanced, changing which PR-branch merges
        // count as base-updates (issue #55); recompute (signature-guarded no-op
        // when unchanged).
        self.recompute_base_update_merges();
        Ok(())
    }

    /// Re-open the libgit2 handle so we observe on-disk state written by other
    /// processes since the last refresh: pushes/fetches creating or updating
    /// remote-tracking refs, upstream config changes, or pruned refs. A
    /// long-lived handle caches this, which otherwise leaves a just-pushed
    /// branch looking unpushed.
    ///
    /// Gated: reopen only on a `force` refresh, when the fs-watcher flagged a
    /// `.git` change (`repo_dirty`), or when there is no active watcher to
    /// provide that signal — so a working-tree-only watcher tick or a quiet
    /// auto-refresh timer doesn't pay to re-open every time. Best-effort: on
    /// failure keep the old handle and continue rather than aborting the
    /// refresh, but surface it once per episode via the `reopen` latch and
    /// leave `repo_dirty` set so the next refresh retries.
    fn maybe_reopen_repo(&mut self, force: bool) {
        if !(force || self.repo_dirty || self.watcher.is_none()) {
            return;
        }
        match self.repo.reopen() {
            Ok(()) => {
                self.refresh_latches.reopen = false;
                self.repo_dirty = false;
            }
            Err(e) => {
                if !self.refresh_latches.reopen {
                    self.refresh_latches.reopen = true;
                    self.set_message(format!("Repo reopen failed: {e}"));
                }
            }
        }
    }

    /// Phases 2–4: rebuild the graph from the already-loaded refs, restore the
    /// selection, and reconcile caches. Shared by `refresh_inner` and the cheap
    /// merged-classification delivery path so both re-point the cursor and
    /// invalidate diff caches identically.
    pub(crate) fn rebuild_and_restore(&mut self, force: bool) -> Result<()> {
        // Snapshot the pre-rebuild selection while the OLD graph is still live.
        let snapshot = self.capture_selection_snapshot();
        self.rebuild_graph()?;
        let has_uncommitted_node = self
            .graph_layout
            .nodes
            .first()
            .is_some_and(|node| node.is_uncommitted);
        self.restore_selection(&snapshot, has_uncommitted_node);
        self.invalidate_caches(force, snapshot.was_uncommitted_selected, has_uncommitted_node);
        Ok(())
    }

    /// Snapshot the current selection so it can be re-resolved against a fresh
    /// graph. Reads only the CURRENT graph/nav state; must be captured before
    /// `rebuild_graph` replaces the layout.
    fn capture_selection_snapshot(&self) -> SelectionSnapshot {
        let selected_node = self
            .graph_nav
            .graph_list_state
            .selected()
            .and_then(|idx| self.graph_layout.nodes.get(idx));
        SelectionSnapshot {
            was_uncommitted_selected: selected_node.is_some_and(|node| node.is_uncommitted),
            prev_selected_commit_oid: selected_node
                .and_then(|node| node.commit.as_ref())
                .map(|commit| commit.oid),
            prev_branch_name: self
                .graph_nav
                .selected_branch_position
                .and_then(|pos| self.graph_nav.branch_positions.get(pos))
                .map(|(_, name)| name.clone()),
        }
    }

    /// Phase 2 — rebuild the commit graph from the already-loaded refs.
    ///
    /// Inputs: the loaded `self.branches`, `self.merged` (delivered
    /// classification + hide toggle), the branch-visibility toggles, and
    /// `self.working_tree_status`; plus on-disk commits/tags/HEAD. Re-fetches
    /// cheap ref-derived data (stashes, tags, HEAD OID) itself so it is
    /// self-contained and callable without `reload_refs`.
    ///
    /// Writes: `self.commits`, `all_commits_loaded`, `graph_layout`,
    /// `graph_generation` (bumped — invalidates the pixel-graph spec cache),
    /// `head_name`, `head_detached`, and the graph-nav branch positions.
    ///
    /// Must NOT touch: the selection index or the diff/search caches. May fail
    /// if the revwalk fails.
    fn rebuild_graph(&mut self) -> Result<()> {
        // Hide-stashes filters at the source: an empty slice pushes no stash
        // tips into the revwalk (so stash-only commits vanish) and yields no
        // stash nodes in `build_graph`. When off, the graph is byte-identical.
        let stashes = if self.hide_stashes {
            Vec::new()
        } else {
            self.repo.get_stashes()
        };
        let uncommitted_count = self
            .working_tree_status
            .as_ref()
            .map(|s| s.accurate_file_count());
        // Excluded branches are dropped from the revwalk, so their exclusive
        // commits are removed from the graph — not merely their labels. Three
        // filters compose: the per-branch picker (`hidden_branches`), the
        // show/hide-remotes toggle (every remote-only ref), and the hide-merged
        // toggle (branches already landed on the trunk, per the most recently
        // delivered `self.merged.branches`). A branch is visible iff no filter
        // excludes it.
        let remote_only = if self.hide_remote_branches {
            remote_only_branch_names(&self.branches)
        } else {
            std::collections::HashSet::new()
        };
        let visible_branches: Vec<BranchInfo> = self
            .branches
            .iter()
            .filter(|b| !self.hidden_branches.contains(&b.name))
            .filter(|b| !remote_only.contains(&b.name))
            .filter(|b| !(self.merged.hide && self.merged.branches.contains(&b.name)))
            // Optimistically-deleted remote branches are hidden until their
            // async `git push --delete` resolves (see `pending_remote_deletions`).
            .filter(|b| !self.pending_remote_deletions.contains(&b.name))
            .cloned()
            .collect();
        self.commits = self.repo.get_commits(
            self.commit_load_limit,
            &visible_branches,
            &stashes,
            self.merged.hide,
        )?;
        // The whole history is loaded once the walk yields fewer than the limit.
        self.all_commits_loaded = self.commits.len() < self.commit_load_limit;
        let tags = self.repo.get_tags();
        let head_commit_oid = self.repo.head_oid();
        let squash_links = self.squash_link_edges();
        self.graph_layout = build_graph(
            &self.commits,
            &visible_branches,
            &tags,
            &stashes,
            uncommitted_count,
            head_commit_oid,
            &squash_links,
        );
        // Invalidate the pixel-graph spec cache: the layout changed.
        self.graph_generation = self.graph_generation.wrapping_add(1);
        self.head_name = self.repo.head_name();
        self.head_detached = self.repo.is_head_detached();

        // Rebuild branch positions
        self.graph_nav
            .rebuild_branch_positions(&self.graph_layout, &self.repo.remotes());

        // Recompute which commits are exclusive to a merged branch's lane, so
        // the "dim merged branches" setting (#108) can grey their rows and graph
        // strokes. Derived from the just-loaded commits + the current merged
        // classification, so it stays in lock-step with every rebuild.
        self.recompute_merged_lane_oids();
        Ok(())
    }

    /// Recompute `merged.lane_oids` — the loaded commits hide-merged would
    /// remove (issue #108, semantics #111: the complement of the live refs'
    /// first-parent chains, exactly the #91 walk's keep-set). Includes the side
    /// lanes of already-deleted merged-in branches, which have no classified
    /// ref to walk from — so this runs even when the classification is empty.
    /// Independent of the `dim`/`hide` toggles: the render path gates on
    /// `dim && !hide`, so flipping the setting reflects instantly without a
    /// rebuild. With `hide` on the excluded commits aren't loaded, so the
    /// complement is naturally empty. Pure over `self.commits` — no revwalk.
    fn recompute_merged_lane_oids(&mut self) {
        // Live tips: every non-merged branch, plus HEAD (a detached or
        // hidden-branch HEAD still anchors a first-parent line to protect) and
        // the stash entries (their nodes are loaded off-chain and must never
        // read as "merged work").
        let mut live_tips: Vec<git2::Oid> = self
            .branches
            .iter()
            .filter(|b| !self.merged.branches.contains(&b.name))
            .map(|b| b.tip_oid)
            .collect();
        if let Some(head) = self.repo.head_oid() {
            live_tips.push(head);
        }
        live_tips.extend(self.repo.get_stashes().iter().map(|s| s.oid));
        self.merged.lane_oids = crate::git::graph::merged_lane_oids(&self.commits, &live_tips);
    }

    /// Phase 3 — re-point the cursor onto the equivalent row in the fresh graph.
    ///
    /// Inputs: the pre-rebuild `snapshot` and whether the new graph still has an
    /// uncommitted node. Resolution order: the uncommitted node if it was
    /// selected and still exists; else the same branch by name; else the same
    /// commit by OID; else the nearest still-valid row. Finally, if a node is
    /// selected with no branch position, adopt the node's first branch.
    ///
    /// Writes: only `graph_nav.graph_list_state` selection and
    /// `selected_branch_position`. Must NOT touch data or caches.
    fn restore_selection(&mut self, snapshot: &SelectionSnapshot, has_uncommitted_node: bool) {
        if snapshot.was_uncommitted_selected && has_uncommitted_node {
            // Restore uncommitted node selection
            self.graph_nav.graph_list_state.select(Some(0));
            self.graph_nav.selected_branch_position = None;
        } else {
            // Restore branch selection if the branch still exists
            self.graph_nav.selected_branch_position = snapshot
                .prev_branch_name
                .as_ref()
                .and_then(|name| {
                    self.graph_nav.branch_positions.iter().position(|(_, n)| n == name)
                });

            // Sync node selection with branch selection
            if let Some(pos) = self.graph_nav.selected_branch_position {
                if let Some((node_idx, _)) = self.graph_nav.branch_positions.get(pos) {
                    self.graph_nav.graph_list_state.select(Some(*node_idx));
                }
            } else if let Some(oid) = snapshot.prev_selected_commit_oid {
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
    }

    /// Phase 4 — reconcile the diff/search caches with the new graph and clamp
    /// the selection into range.
    ///
    /// Inputs: `force` (manual refresh clears the whole diff cache; auto-refresh
    /// keeps it when the same content stays selected), plus the pre-rebuild
    /// `was_uncommitted_selected` and the new `has_uncommitted_node`.
    ///
    /// Writes: `diff_cache`, `search_state` (cleared unless a search is active),
    /// the selection clamp, and `visible_commit_indices` (when a commit filter
    /// is active). Must NOT touch the loaded refs or the graph layout.
    fn invalidate_caches(
        &mut self,
        force: bool,
        was_uncommitted_selected: bool,
        has_uncommitted_node: bool,
    ) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GitRepository;
    use git2::{Repository, Signature};
    use std::path::Path;

    fn commit_file(repo: &Repository, path: &str, contents: &str, message: &str) -> git2::Oid {
        let workdir = repo.workdir().unwrap();
        std::fs::write(workdir.join(path), contents).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = Signature::now("Test", "test@example.com").unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents = parent.iter().collect::<Vec<_>>();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap()
    }

    /// Collect the commit OIDs currently present as graph nodes.
    fn node_oids(app: &App) -> Vec<git2::Oid> {
        app.graph_layout
            .nodes
            .iter()
            .filter_map(|n| n.commit.as_ref().map(|c| c.oid))
            .collect()
    }

    /// Delivering a merged-branch classification takes the cheap
    /// `rebuild_and_restore` path (no `reload_refs`) yet still applies the
    /// hide-merged filter to the revwalk: a merged branch's exclusive commits
    /// disappear from the graph, while the base commit — and the selection —
    /// survive. This is the observable contract behind issue #69's cheaper
    /// classifier delivery.
    #[test]
    fn cheap_rebuild_applies_merged_hide_filter_and_keeps_selection() {
        let tempdir = tempfile::tempdir().unwrap();
        let git = Repository::init(tempdir.path()).unwrap();

        // Base commit on the default branch (HEAD).
        let oid_base = commit_file(&git, "a.txt", "a", "base");
        let default_branch = git.head().unwrap().shorthand().unwrap().to_string();

        // A `feature` branch with a commit that is exclusive to it.
        let base_commit = git.find_commit(oid_base).unwrap();
        git.branch("feature", &base_commit, false).unwrap();
        git.set_head("refs/heads/feature").unwrap();
        let oid_feat = commit_file(&git, "f.txt", "f", "feature work");
        // Leave HEAD on the default branch so the feature tip is only reachable
        // via `feature` (HEAD is always walked, regardless of the filter).
        git.set_head(&format!("refs/heads/{default_branch}")).unwrap();

        let repo = GitRepository::open(tempdir.path()).unwrap();
        let mut app = App::from_repo(repo).unwrap();

        // Both commits are visible before any branch is hidden.
        assert!(node_oids(&app).contains(&oid_base));
        assert!(
            node_oids(&app).contains(&oid_feat),
            "feature's exclusive commit should be visible initially"
        );

        // Select the base commit so we can assert the selection is preserved.
        let base_idx = app
            .graph_layout
            .nodes
            .iter()
            .position(|n| n.commit.as_ref().is_some_and(|c| c.oid == oid_base))
            .unwrap();
        app.graph_nav.graph_list_state.select(Some(base_idx));

        // Simulate a classifier delivery marking `feature` merged, with hiding on.
        app.merged.hide = true;
        app.merged.branches = std::collections::HashSet::from(["feature".to_string()]);

        // The cheap delivery path: rebuild + restore only, no reload_refs.
        app.rebuild_and_restore(false).unwrap();

        let after = node_oids(&app);
        assert!(after.contains(&oid_base), "base commit must remain visible");
        assert!(
            !after.contains(&oid_feat),
            "hiding merged `feature` must drop its exclusive commit from the graph"
        );

        // Selection stays on the base commit (resolved by OID against the new graph).
        let selected_oid = app
            .graph_nav
            .graph_list_state
            .selected()
            .and_then(|i| app.graph_layout.nodes.get(i))
            .and_then(|n| n.commit.as_ref())
            .map(|c| c.oid);
        assert_eq!(selected_oid, Some(oid_base), "selection should follow the base commit");
    }
}
