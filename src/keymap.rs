//! Configurable keyboard shortcut domain types.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::Action;

/// A normalized, single-key terminal shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyBinding {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyBinding {
    pub fn matches(self, event: KeyEvent) -> bool {
        self.code == event.code && self.modifiers == event.modifiers
    }
}

/// One non-fatal problem found while resolving `[keymap]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeymapWarning {
    pub entry: String,
    pub reason: String,
}

/// One public command exposed to configuration and shortcut-aware UI.
#[derive(Debug, Clone)]
pub struct BindingDescriptor {
    pub id: &'static str,
    pub action: Action,
}

/// Startup-resolved keyboard configuration.
#[derive(Debug, Clone, Default)]
pub struct ResolvedKeymap {
    warnings: Vec<KeymapWarning>,
}

impl ResolvedKeymap {
    pub fn warnings(&self) -> &[KeymapWarning] {
        &self.warnings
    }
}
