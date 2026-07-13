# VSCode Parity — Feature Gap Analysis (2026-07-13)

Reference: **Git Graph** extension (`mhutchie.git-graph`) + VSCode **built-in
Source Control**. GitLens excluded. Sizes: S ≈ hours, M ≈ 1-2 days,
L ≈ several days, XL ≈ week+.

Infra note that drives estimates: fetch/push already shell out to the `git`
CLI (`run_git()` in `git/operations.rs`), so most missing mutations are
"another shell-out + a confirm/input flow." Diff hunks are already parsed
(`git/diff.rs`) but only used for display.

## Priority gaps

1. ~~**Merge-conflict awareness + abort/continue + accept ours/theirs**~~ —
   **DONE (2026-07-13).** Op-state detection (`OperationState`), conflicted
   files in a "Merge Changes" section (`!` marker), status-bar indicator,
   `o` ours / `t` theirs / `c` continue / `A` abort (via Confirm). Conflicts
   are a typed `OpOutcome`, not an error. Full 3-way merge editor remains
   XL and deferred.
2. ~~**Pull (M)**~~ — **DONE (2026-07-13).** `p` pulls (background `git pull`,
   honoring `pull.rebase`); Pull also in the commit menu on the HEAD tip.
   Fast-forward / merge-commit / conflict all handled — a conflicting pull
   returns the typed `OpOutcome::Conflicts` and lands in the guided resolve
   flow via op-state detection (#1). `GIT_EDITOR=true` so it never blocks.
3. ~~**Hunk / line-level staging (L)**~~ — **DONE (2026-07-13).** In the
   FileDiff viewer for uncommitted changes: `s` stage hunk, `u` unstage hunk,
   `x` discard hunk (via Confirm). Patch synthesis in `git/patch.rs` →
   `git apply --cached` / `--cached -R` / `-R`. See architecture.md.
4. ~~**Real branch filtering (M)**~~ — **DONE (2026-07-13).** `get_commits()`
   takes the visible branch set and walks only those tips (HEAD always
   pushed), so hiding a branch removes its exclusive commits, not just labels.
5. ~~**Multi-remote + push -u / upstream / publish (M)**~~ — **DONE
   (2026-07-13).** Fetch/pull/push resolve the remote from the branch's
   upstream, prompting via a `RemotePicker` only when several remotes and no
   upstream disambiguate (single-remote repos never prompt). `P` pushes to the
   configured upstream, or publishes with `git push -u <remote> <branch>` when
   none is set. Status bar shows HEAD ahead/behind (`↑2 ↓1`). Extras: `git
   remote prune` menu action, and remote-branch delete (`git push <remote>
   --delete`) from the delete picker behind Confirm.

Near-free S wins — **DONE (2026-07-13)**: branch rename (commit-menu
"Rename branch" → prefilled Input → `git branch -m`), tag delete/push
(commit-menu "Delete tag" behind Confirm / "Push tag" to origin-or-sole-remote,
picker when a commit carries several tags), stash-all (`Ctrl+S` now opens a
stash-options menu: staged / all / all+untracked, each with an optional Input
message), create-branch-from-stash (stash-node menu "Branch from stash" →
`git stash branch`), copy file path (`y` in the files pane) plus "Copy commit
message" in the commit menu. (stage-all/unstage-all done earlier: `S`/`U` in the
files pane.)

## Viewer features (added 2026-07-13)

- **Compare two arbitrary commits** — graph `m` marks the selected commit
  (◆ marker + status message); `m` on a second commit opens the comparison.
  The pair is ordered older → newer by commit time; the files pane and commit
  detail show the tree-to-tree diff (direction noted in the detail pane).
  Implemented by extending `DiffTarget` with `Range(old, new)`, so it reuses the
  two-tier quick/full diff cache. Opening a file (Space) shows the file's
  two-commit diff. `Esc` on the graph clears the comparison. Also reachable via
  the commit menu ("Mark for compare" / "Compare with marked commit").
- **Per-file history** — files pane `h` lists commits that touched the selected
  path via `git log --follow` (rename-aware, capped at 200) in a picker; `Enter`
  opens that commit's diff for the file, `Esc` returns.
- **Signature status** — commit detail shows a `Sig:` line from `git log -1
  --format=%G?`, memoized per OID (`sig_status_cache`). Unsigned commits render
  subtly; valid/bad signatures stand out.

## Notable full-parity areas

Graph with uncommitted row + remote branches, commit detail + syntax-highlit
file diff, amend, copy hash, branch create/checkout/delete, reset
soft/mixed/hard, cherry-pick/revert/merge/rebase (conflict-blind — see #1),
stash list/apply/pop/drop as graph nodes, commit filter (Ctrl+F,
message/author/hash), fuzzy branch search (/).

## Deliberately skipped (terminal-impractical or low value)

Avatars, code-review tracking, issue links/PR creation, emoji/markdown in
messages, column resize cosmetics.

## Capabilities keifu has that Git Graph does NOT

Archive to `.archive/` with auto-gitignore, trash-untracked to recycle bin
(safer than `git clean`), single-slot undo for file ops, add-to-gitignore
from UI, folder grouping + folder-level staging, open-in-default-app,
fuzzy-filtered commit menu, SSH-friendly TUI.
