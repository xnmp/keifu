# VSCode Parity — Feature Gap Analysis (2026-07-13)

Reference: **Git Graph** extension (`mhutchie.git-graph`) + VSCode **built-in
Source Control**. GitLens excluded. Sizes: S ≈ hours, M ≈ 1-2 days,
L ≈ several days, XL ≈ week+.

Infra note that drives estimates: fetch/push already shell out to the `git`
CLI (`run_git()` in `git/operations.rs`), so most missing mutations are
"another shell-out + a confirm/input flow." Diff hunks are already parsed
(`git/diff.rs`) but only used for display.

## Priority gaps

1. **Merge-conflict awareness + abort/continue + accept ours/theirs (M-L)** —
   today `merge_branch` bails with "resolve manually" AFTER libgit2 has
   written conflict markers and MERGE_HEAD (`operations.rs:168`), stranding
   the repo mid-merge with no in-app recovery; conflicted files render as
   plain "Modified". Minimal slice: detect conflict/in-progress state
   (MERGE_HEAD/REBASE_HEAD/CHERRY_PICK_HEAD), show a conflicted-files group,
   offer `--abort`/`--continue` and `git checkout --ours/--theirs -- path`.
   Full 3-way merge editor is XL and deferred.
2. **Pull (M)** — no fetch+integrate orchestration; both halves exist.
   Needs #1 for the conflict path.
3. **Hunk / line-level staging (L)** — signature SCM feature. Hunks are
   parsed; work is patch synthesis → `git apply --cached` (and reverse-apply
   for partial discard).
4. **Real branch filtering (M)** — Shift+B only removes labels; commits from
   hidden branches still walk into the graph. Fix: pass visible branch tips
   into `get_commits()` (already flagged in TODO.md).
5. **Multi-remote + push -u / upstream / publish (M)** — origin-only and
   hardcoded today (`git push origin HEAD`).

Near-free S wins to bundle: branch rename, tag delete/list/push (tags aren't
even shown as graph refs — S-M), stash-all (`git stash push [-u]`; today only
`--staged`), create-branch-from-stash, stage-all/unstage-all, copy file path.

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
