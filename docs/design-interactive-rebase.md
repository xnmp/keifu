# Interactive rebase — design (issue #25)

Status: **design only**. This document proposes the architecture; no execution
code ships on this branch. Every claim below is grounded in existing keifu code
(cited as `path:line`).

Interactive rebase is the one genuinely multi-session feature on the slate, and
it rewrites history — so we design first. The good news, established during the
survey: **most of the moving parts already exist.** keifu already runs git
rebases (`rebase_branch`, `operations.rs:307`), already treats a rebase conflict
as a first-class outcome (`OpOutcome::Conflicts`, `operations.rs:329`), already
detects an in-progress interactive rebase (`operation_state()` maps
`RebaseInteractive → OperationState::Rebase`, `repository.rs:51`), already drives
rebase abort/continue (`abort_operation`/`continue_operation`,
`operations.rs:604,628`), already has a conflict-resolution UI
(`focus_conflict_files`, accept ours/theirs, continue/abort —
`conflict_actions.rs:44,76,127`), and — since the reflog-undo work — already has
a verified pre-op snapshot mechanism (`UndoLedger`, `src/undo.rs`). The new work
is a **plan model + editor UI** and a thin **execution driver** that hands a todo
list to git.

---

## 1. Scope

### v1 (this feature, delivered across slices)

Over the linear range **`<selected base commit>..HEAD`** on the **current
branch**, offer per-commit actions:

- **pick** (keep as-is)
- **reword** (keep the diff, change the message)
- **squash** (meld into the previous commit, combine messages)
- **fixup** (meld into the previous commit, discard this message)
- **drop** (remove the commit)
- **reorder** (move a commit up/down in the sequence)

No **edit-stops** in v1: we never pause the rebase to let the user amend a
commit's *contents* mid-flight. The only interactive pauses are conflicts.

Conflicts are surfaced through the existing conflict UI; **abort restores the
pre-rebase state cleanly** (git owns the on-disk rebase state — see §3).

### Explicit non-goals (v1)

- `--onto <newbase>` (relocating onto a different base)
- multi-branch / stacked rebases
- autosquash (`--autosquash`, `fixup!`/`squash!` message detection)
- `edit` and `break` todo actions (edit-stops)
- rebasing merge commits (`--rebase-merges`) — v1 flattens/ refuses ranges that
  contain merges (detect via `commit.parent_oids.len() >= 2`, the same test
  `GraphNode::is_merge` uses, `git/graph.rs`)

---

## 2. UX

### Entry point

The selected commit is the **base** (everything *above* it gets replayed). Two
entry points, both cheap to add:

1. **Commit menu** — a new `CommitMenuItem::InteractiveRebase` labelled
   *"Rebase commits above this…"*, alongside the existing menu items
   (`commit_menu_actions.rs`, `execute_menu_item`). This is the primary,
   discoverable path: the user is already looking at the base commit.
2. **Command palette** — a `"Interactive rebase from here"` entry gated on
   `has_selected_commit` (mirrors the merge-base / mark-for-compare entries in
   `src/palette.rs`), dispatching a new `Action::StartInteractiveRebase`.

No dedicated single-key binding in v1 — the graph keymap is already at its
crowding budget (we made the same call for merge-base `^` and undo `Ctrl+Z`).

Eligibility (hide/deny when not applicable): current HEAD is a branch (not
detached), the working tree is clean, and the range contains ≥1 commit and no
merge commits. See the failure matrix in §3.

### The plan editor — `AppMode::RebasePlan`

A new full-screen-ish overlay mode (a sibling of the existing list-driven modes
like `CommitMenu`/`BranchFilter` in the `AppMode` enum, `app/mod.rs`). It holds
the editable plan:

```
AppMode::RebasePlan {
    entries: Vec<PlanEntry>,   // display order: newest commit first (top)
    cursor: usize,             // highlighted row
    base_oid: Oid,             // the rebase base (unchanged)
}
```

Rendered by a new `ui/rebase_plan.rs` widget, styled like `CommitMenuWidget`
(`ui/commit_menu.rs`): one row per commit, `<action-tag> <short-hash> <subject>`,
the cursor row highlighted via `theme.list_selection_style()`.

Keybindings (routed in `keybindings.rs` under a new `AppMode::RebasePlan` arm,
following `map_commit_menu_mode`):

| Key    | Effect                                             |
| ------ | -------------------------------------------------- |
| `j`/`k` or ↑/↓ | move the **cursor**                        |
| `J`/`K`        | move the **commit** up/down (reorder)      |
| `p`            | set action = pick                          |
| `s`            | set action = squash (invalid on the oldest row — nothing to squash into; no-op + status hint) |
| `f`            | set action = fixup (same constraint)       |
| `r`            | set action = reword → open the reword editor |
| `d`            | set action = drop                          |
| `Enter`        | → summary `Confirm`                        |
| `Esc`          | cancel, discard the plan                   |

**Reword editing** reuses the compose editor already built for PRs: the
`TextEditor` (`src/text_editor.rs`) + `PrComposeWidget` (`ui/pr_compose.rs`)
driven by a `ComposePurpose` variant (`app/mod.rs`). We add
`ComposePurpose::RebaseReword { row: usize }`; submitting stores the new message
on that `PlanEntry` and returns to `RebasePlan`. No new editor is built.

### Summary confirmation

`Enter` from the plan raises the existing `AppMode::Confirm` (`app/mod.rs`) with a
generated summary, e.g.:

```
Rebase 5 commits onto abc1234:
  reword 2, squash 1, drop 1, reorder.
  ⚠ 3 of these are already pushed to origin/feature — you'll need to force-push.
Proceed?
```

The force-push warning is **surfaced, not blocking** (§3), computed from
`BranchInfo.upstream`/`ahead`/`behind` (`git/branch.rs`, already populated by
`get_branches`). Confirming dispatches `ConfirmAction::RunRebase(plan)`.

### Progress / conflict states

Because git stops the rebase at the first conflict and returns (see §3), the run
is a sequence of bounded segments, each mapping onto the **existing**
conflict-resolution flow:

- On `OpOutcome::Completed` → toast *"Rebased N commits"* (mirrors the pull/merge
  toasts in `network_ops.rs`), refresh, done.
- On `OpOutcome::Conflicts { count }` → set `self.op_state = OperationState::Rebase`
  (via `operation_state()`), `focus_conflict_files()`
  (`conflict_actions.rs:44`), and let the **existing** UI take over: accept ours
  / accept theirs per file (`accept_ours`/`accept_theirs`), then **continue**
  (`continue_operation(Rebase)`) or **abort** (`abort_operation(Rebase)` behind
  the existing `ConfirmAction::AbortOperation` confirm, `conflict_actions.rs:127`).
  Continue replays the next segment until `Completed` or the next conflict.

This is the crux of why the feature is tractable: **interactive-rebase conflicts
are just rebase conflicts, and keifu already resolves those.** The status bar
already renders a `REBASING` label for `OperationState::Rebase`
(`repository.rs`), so the in-progress state is already communicated.

---

## 3. Execution strategy — the core decision

### The options

**(a) `git rebase -i` with `GIT_SEQUENCE_EDITOR` writing our plan.**
Shell out to the git CLI; hand it a fully-authored todo list.

**(b) Native execution via git2 cherry-pick sequence onto a temp/detached ref.**
Re-implement interactive rebase ourselves: detach at base, cherry-pick/amend each
plan entry in order, move the branch ref on success.

**(c) `git rebase` non-interactive, per-step.** Rejected outright: non-interactive
rebase is a linear replay onto a base; it cannot express reorder/squash/drop, so
it doesn't implement the feature.

### Recommendation: **(a) `git rebase -i` with an injected todo.**

**Reasoning, grounded in how keifu already runs git:**

1. **The precedent is already in the tree.** `run_git_allow_conflict`
   (`operations.rs:60`) runs a git subcommand with `GIT_EDITOR=true` and
   **`GIT_SEQUENCE_EDITOR=true`** already set (`operations.rs:66-67`) precisely so
   interactive-capable commands don't block the TUI on an editor. Interactive
   rebase is the same pattern with the sequence editor pointed at *our* plan
   instead of `true`:

   ```
   GIT_SEQUENCE_EDITOR = cp <our-todo-file>      # git runs: cp <our-todo> <git's todo path>
   GIT_EDITOR          = true                      # accept default squash messages
   ```

   `git` invokes `$GIT_SEQUENCE_EDITOR "<todofile>"`, so `cp <ours>` overwrites
   git's generated todo with ours. No custom editor binary, no script file — just
   a coreutil already assumed present (keifu already shell-outs to `xclip`/`curl`
   etc. and targets Unix). The op function is a sibling of the existing ones:
   `rebase_interactive(repo_path, base_oid, todo_path) -> Result<OpOutcome>` via
   `run_git_allow_conflict(repo_path, &["rebase", "-i", &base_oid.to_string()])`.

2. **Conflicts + abort + continue are already wired for this exact state.**
   `operation_state()` maps `RebaseInteractive → OperationState::Rebase`
   (`repository.rs:51`). So a `git rebase -i` that stops at a conflict is already
   recognised by keifu, already routed to the conflict UI, and already
   continuable/abortable. Option (b) would re-implement all of this in-process.

3. **git owns the on-disk rebase state → crash recovery is free.** `git rebase -i`
   persists its todo + progress in `.git/rebase-merge/`. If keifu is killed
   mid-rebase, the repo is left in a **standard, resumable** git rebase: the user
   can `git rebase --continue/--abort` from any shell, and on relaunch keifu's
   `operation_state()` detects the in-progress rebase and offers the same
   continue/abort. With option (b), keifu owns the sequence state *in memory*; a
   crash strands the user on a detached HEAD with no todo (recoverable only via
   `ORIG_HEAD` by hand). For a history-rewriting feature, git-owned state is the
   safer default.

4. **Fidelity we'd otherwise re-implement.** git handles, for free: GPG/SSH commit
   **signing** (`commit.gpgsign`/`-S`), pre-commit/commit-msg **hooks** where
   applicable, **committer vs author** date/identity handling, and the
   **empty-commit** policy (`--empty=drop` etc.). Option (b) via `git2` commits
   would silently produce **unsigned** commits for users who sign — a real trust
   regression on a rewrite feature — plus we'd own every empty-commit and
   date-preservation edge case.

**Handling reword/squash without an editor script.** The one wrinkle with (a) is
that `reword`/`squash` todo verbs normally open `$GIT_EDITOR`. We avoid editor
scripting entirely:

- **reword** → emit `pick <sha>` followed by
  `exec git commit --amend -F <tmp-message-file>`. The new message is collected in
  the plan editor *before* execution and written to a temp file (handles
  multi-line/special chars safely; a single-line `-m` would not).
- **squash** → emit the native `squash <sha>` verb and let `GIT_EDITOR=true`
  accept git's default concatenated message. Customising the squashed message is
  expressed as a `reword` on the squash target — no special case.
- **fixup** / **drop** → native `fixup <sha>` / omit the line. **pick** →
  `pick <sha>`.

So our todo generator emits only `pick`/`squash`/`fixup`/`exec` lines — a small,
**purely-testable** serialization (§5).

### Strongest counterargument (and why it loses)

**(b) native execution gives full, synchronous control:** a real per-commit
progress bar, direct control of squash/reword messages without the
`GIT_SEQUENCE_EDITOR`/`exec-amend` indirection, and no dependency on `cp`/editor
semantics. It's "more honest" — no shelling out through an editor hook.

It loses because it trades a small indirection for **large ownership**: we'd
re-implement commit signing, hooks, date/identity preservation, empty-commit
policy, *and* on-disk crash-recovery state — reproducing what `git rebase`
already does correctly. For a feature whose failure mode is *corrupting history*,
"let git do the rewrite" is the conservative, correct call. Progress granularity
is a non-issue: local rebases of a handful of commits are near-instant, and the
meaningful checkpoint (a conflict pause) is already surfaced.

### Required adjustment (small, low-risk)

The existing rebase **abort/continue** route through **libgit2**
(`abort_operation` → `repo.open_rebase()?.abort()`, `operations.rs:606-611`;
`continue_rebase` opens the libgit2 rebase, `operations.rs:642`). libgit2's rebase
handling does not reliably understand a **CLI interactive** todo (with `exec`
lines). For a `git rebase -i`, abort/continue must use the **CLI**
(`git rebase --abort` / `git rebase --continue`, with `GIT_EDITOR=true`). `git`
can abort/continue a libgit2-started rebase too, so the cleanest change is to
route `OperationState::Rebase` continue/abort through the CLI uniformly — an
additive branch in `abort_operation`/`continue_operation`. This is the only
existing-code change the design requires.

### Failure matrix

| Situation | Behaviour |
| --- | --- |
| **Conflict mid-plan** | git stops, non-zero exit, `.git/rebase-merge/` present → `OpOutcome::Conflicts` (`run_git_allow_conflict`). `operation_state()` = `Rebase` → existing conflict UI. Continue/abort via CLI. |
| **Process killed mid-rebase** | git's on-disk state persists. Relaunch → `operation_state()` detects the in-progress rebase; keifu surfaces "rebase in progress" and offers continue/abort (already the flow for `op_state`). User may also resolve from any shell. |
| **Dirty tree at start** | **Block.** Reuse `is_working_tree_clean` (`operations.rs`, added for undo). Message: "Commit or stash changes before rebasing." |
| **Detached HEAD** | **Block** in v1: "Check out a branch to rebase." (`head_detached` is already tracked on `App`.) |
| **Range contains a merge commit** | **Block** in v1 (no `--rebase-merges`): "Range contains a merge commit; not supported." Detect via `is_merge`. |
| **Branch already pushed upstream** | **Warn, don't block.** The summary confirm notes the force-push implication, computed from `BranchInfo.upstream/ahead/behind`. Rewriting is the user's call. |
| **Empty commit produced** (e.g. drop leaves a redundant patch) | Delegate to git (`--empty=drop` by default); note the dropped commit in the completion toast. |

---

## 4. Safety integration (reuses the reflog-undo work)

- **Pre-rebase snapshot into the `UndoLedger`.** Before running, capture
  `pre = self.repo.head_oid()`. On `OpOutcome::Completed` with HEAD moved, record
  the *same* entry shape the merge/pull undos already use
  (`confirm_actions.rs`, merge arm; `src/undo.rs`):

  ```
  UndoEntry {
      description: "Interactive rebase (N commits onto abc1234)",
      confirm:     "Undo: rebase → reset to <pre>?",
      plan:  UndoPlan::ResetHard { to: pre },
      check: UndoCheck::HeadAtCleanTree(post),   // HEAD still at the rebased tip AND tree clean
  }
  ```

  Ctrl+Z then reverts a *successful* rebase by `reset_hard_checked` to the
  pre-rebase HEAD — the guard refuses if the tree is dirty or HEAD moved since,
  exactly as designed for the other reversible ops.

- **`ORIG_HEAD` semantics.** `git rebase` sets `ORIG_HEAD` to the pre-rebase HEAD,
  and `git rebase --abort` restores it. We rely on git's `ORIG_HEAD` for *abort*,
  but for *undo-after-success* we prefer our **own** ledger snapshot: `ORIG_HEAD`
  is clobbered by later operations (merge, reset, other rebases), whereas the
  ledger entry is stable and verified. The two are complementary, not redundant.

- **Why undo-after-abort isn't needed.** Abort (`git rebase --abort`) already
  resets the branch to the pre-rebase state (`ORIG_HEAD`) and clears the rebase.
  There is nothing to undo — so we record an undo entry **only on `Completed`**,
  never on abort. (Same principle as recording merge undo only on
  `OpOutcome::Completed`, `confirm_actions.rs`.)

---

## 5. Testing plan

### Pure (no repo) — the bulk of correctness

- **Plan model** (`src/rebase_plan.rs`): `PlanEntry { oid, action, message }`,
  action enum, construction from a commit range.
- **Reorder / squash state machine**: `move_up`/`move_down` bounds and stability;
  `set_action` validity (squash/fixup illegal on the oldest row — nothing beneath
  to meld into); drop/reorder interactions. Deterministic, table-driven.
- **Todo-file serialization**: plan → git todo text. Asserts the display order
  (newest-first in the UI) is **reversed** to git's oldest-first todo; that
  `reword` expands to `pick` + `exec git commit --amend -F <file>`; that
  `squash`/`fixup` use native verbs; that `drop` omits the line; and that message
  temp-file paths are wired correctly. This is the highest-value pure surface and
  where a serialization bug would corrupt a rebase.
- **Summary generation**: action counts + force-push warning text from a fake
  `BranchInfo` (ahead/behind).

### Fixture-repo integration — reuse the `tests/undo_test.rs` harness

Same pattern: `git2` builds a real working-tree repo, `App::from_repo`, drive the
real `AppMode`/`Action` flow, assert on the resulting refs/trees. One test per
action, end-to-end:

- **reorder** two commits → assert the new commit order and that both trees/diffs
  are preserved.
- **squash** → one commit, combined message; **fixup** → combined, second message
  gone.
- **reword** → message changed, diff identical.
- **drop** → the commit is absent, later commits reparented.
- **conflict → abort restores exactly**: construct a conflicting reorder, run,
  hit `OpOutcome::Conflicts`, `abort` → assert HEAD == pre-rebase and tree intact
  (the safety-critical test).
- **dirty tree blocks**; **detached HEAD blocks**; **merge-in-range blocks**.
- **undo-after-success** resets to the pre-rebase HEAD (drives the ledger, like
  the existing undo merge test).

No test drives the TUI render loop; all assert on git state (matching
`undo_test.rs`).

---

## 6. Implementation slices

Each slice is independently green (compiles, tests pass, clippy clean) and
mergeable on its own.

1. **Plan model + read-only editor UI** — `rebase_plan.rs` (pure `PlanEntry`/
   actions), `AppMode::RebasePlan`, `ui/rebase_plan.rs` widget, entry from the
   commit menu, `j`/`k` cursor. No editing, no execution; `Enter`/`Esc` just
   close. Pure model tests. **Small.**
2. **Actions + reorder** — `s`/`f`/`r`(stub)/`d`/`p` set actions, `J`/`K` move,
   squash/fixup validity, action tags rendered. Pure state-machine tests.
   **Small–medium.**
3. **Todo serialization + execution happy path** — the serializer, the
   `rebase_interactive` op (`GIT_SEQUENCE_EDITOR=cp`), summary `Confirm`,
   `ConfirmAction::RunRebase`, refresh + toast, block dirty/detached/merge-range.
   Pure serializer tests + fixture happy-path tests (reorder/squash/fixup/drop —
   reword deferred). **Medium.**
4. **Conflicts + abort/continue** — route `OpOutcome::Conflicts` to the existing
   conflict UI; add the CLI branch to `abort_operation`/`continue_operation` for
   interactive rebase; detect an in-progress rebase at startup. Fixture
   conflict-abort test (the safety test). **Medium.**
5. **Reword** — `ComposePurpose::RebaseReword`, per-commit message collection,
   temp-file + `exec git commit --amend -F` lines. Fixture reword test.
   **Small–medium.**
6. **Undo integration** — pre-rebase snapshot → `UndoLedger::ResetHard`, help +
   README. Fixture undo-after-rebase test. **Small.**

Rough total: ~2–3 focused sessions, front-loaded on the pure plan/serialization
core (slices 1–3) where correctness is cheapest to lock down.

---

## Nothing in the current codebase blocks this design

Everything needed is either present or additive:

- **Present & reused:** `run_git_allow_conflict` + `GIT_SEQUENCE_EDITOR`
  precedent; `OpOutcome::Conflicts`; `operation_state()` → `RebaseInteractive`;
  the conflict UI (`focus_conflict_files`, accept ours/theirs, continue/abort);
  `UndoLedger` + `reset_hard_checked` + `is_working_tree_clean`; the
  `TextEditor`/`PrCompose` compose flow; `BranchInfo` upstream tracking; the
  commit menu and command palette; panic-safe teardown (`tui::restore`,
  `tui.rs:26`) so a crash still restores the terminal even if git left a rebase
  in progress.
- **One existing-code change:** route `OperationState::Rebase` abort/continue
  through the git CLI (not libgit2 `open_rebase`) so it handles a CLI interactive
  todo — small and low-risk.
- **Additive:** new `AppMode::RebasePlan`, `rebase_plan.rs` model, the
  `rebase_interactive` op, a `ComposePurpose::RebaseReword` variant, and the
  menu/palette/undo wiring.
