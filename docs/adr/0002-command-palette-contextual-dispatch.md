# ADR 0002: Command-palette contextual dispatch safety

Status: Accepted

Date: 2026-08-02

Governs: `src/app/palette_actions.rs`, `src/palette.rs`, and contextual command-palette integration tests.

## Context

The command palette can expose the context-sensitive operations normally
reached through the selected commit's Enter menu. Those operations include
durable Git effects such as push, pull, branch and tag mutation, reset, revert,
and stash changes. A palette row is rendered from a snapshot of the graph, but
the selection or repository data can change before the user presses Enter.

## Decision

Contextual palette rows carry a fingerprint of the selected graph target: its
commit identity, stash identity, and selected branch identity. Immediately
before dispatch, Keifu revalidates that fingerprint and that the row is still
available from `available_commit_menu_items`. If either check fails, Keifu
performs no operation and shows a non-blocking error toast asking the user to
reopen the palette.

On a successful revalidation, the palette calls the same
`execute_menu_item` path as the Enter menu. It does not reimplement Git
operations, confirmation handling, input prompts, toast reporting, or
cancellation. Contextual branch and tag prompts retain the validated commit
OID until their final confirmation, so a later selection change cannot
retarget their durable mutation.

## Failure and concurrency analysis

- Input handling is serialized: selection changes, refresh completion, and
  `MenuSelect` are separate actions. A selection or refresh change before
  `MenuSelect` fails fingerprint/availability revalidation, so no durable
  operation is started. The integration test changes the selected commit after
  opening the palette and asserts that dispatch is rejected with an error toast
  instead of opening a confirmation for the new commit.
- Once revalidation succeeds, the existing Enter-menu executor captures the
  commit/branch data into its existing confirmation or input state. A later
  selection change therefore cannot retarget that pending operation. The
  integration test drives the palette through confirmation, prompt, toast, and
  cancellation states, changes selection while branch/tag prompts are open,
  and asserts those mutations still target the original commit.
- A crash before `MenuSelect` cannot mutate the repository because building or
  filtering candidates is read-only. A crash after successful dispatch has the
  same failure boundary as the existing Enter-menu path: confirmation and input
  states have not yet mutated Git, while confirmed operations are owned by the
  established Git operation layer and its existing outcome/toast handling.

## Consequences

Every new contextual palette operation must be sourced from
`available_commit_menu_items`, carry the target fingerprint, and execute only
after revalidation. Tests must use `OpenCommandPalette` and `MenuSelect`, not a
direct call to the menu executor, for confirmation, prompt, toast,
cancellation, stale-selection behavior, and delayed prompt confirmation.
