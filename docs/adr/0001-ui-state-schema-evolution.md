# ADR 0001: UiState schema evolution

Status: Accepted

Date: 2026-08-02

Scope: `src/config.rs` (`UiState`) and state-only settings registered by `src/settings.rs`.

## Context

`state.toml` stores user-interface preferences across launches. Existing users
may have files written before a newly introduced preference existed, so a schema
addition must not change their existing experience merely because its key is
absent.

## Decision

Add state-only preferences as fields on `UiState` with a serde default that
matches the prior observable behavior. Register the setting through the pure
settings registry and persist it through `UiState::save`.

For `status_bar_visible`, the default is `true`: state files without this key
continue to show the status bar. Once a user changes the preference, the saved
`state.toml` includes the boolean and a subsequent load restores that choice.

## Consequences

Schema additions remain backwards-compatible with existing `state.toml` files.
The configuration persistence test must cover both the absent-key default and a
save/load round trip for every newly added UI-state field.
