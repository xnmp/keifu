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
2. **Pull (M)** — no fetch+integrate orchestration; both halves exist.
   Conflict path now exists (#1).
3. ~~**Hunk / line-level staging (L)**~~ — **DONE (2026-07-13).** In the
   FileDiff viewer for uncommitted changes: `s` stage hunk, `u` unstage hunk,
   `x` discard hunk (via Confirm). Patch synthesis in `git/patch.rs` →
   `git apply --cached` / `--cached -R` / `-R`. See architecture.md.
4. ~~**Real branch filtering (M)**~~ — **DONE (2026-07-13).** `get_commits()`
   takes the visible branch set and walks only those tips (HEAD always
   pushed), so hiding a branch removes its exclusive commits, not just labels.
5. **Multi-remote + push -u / upstream / publish (M)** — origin-only and
   hardcoded today (`git push origin HEAD`).

Near-free S wins — **DONE (2026-07-13)**: branch rename (commit-menu
"Rename branch" → prefilled Input → `git branch -m`), tag delete/push
(commit-menu "Delete tag" behind Confirm / "Push tag" to origin-or-sole-remote,
picker when a commit carries several tags), stash-all (`Ctrl+S` now opens a
stash-options menu: staged / all / all+untracked, each with an optional Input
message), create-branch-from-stash (stash-node menu "Branch from stash" →
`git stash branch`), copy file path (`y` in the files pane) plus "Copy commit
message" in the commit menu. (stage-all/unstage-all done earlier: `S`/`U` in the
files pane.)

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
