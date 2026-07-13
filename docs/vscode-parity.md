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

Near-free S wins to bundle: branch rename, tag delete/push (tags are now
shown as graph refs — DONE), stash-all (`git stash push [-u]`; today only
`--staged`), create-branch-from-stash, copy file path. (stage-all/unstage-all
done 2026-07-13: `S`/`U` in the files pane.)

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
