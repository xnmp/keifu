//! Settings registry — the pure domain layer behind the Ctrl+, settings menu.
//!
//! This module has **no dependency on `App`**. It defines:
//!
//! - [`SettingsModel`]: a flat, plain-data snapshot of every user-facing setting
//!   the menu can edit. `App` projects its scattered state into this and applies
//!   edits back (see `App::settings_model` / `App::apply_settings_model`).
//! - [`descriptors`]: the ordered, grouped list of settings shown in the menu,
//!   each with a typed [`SettingKind`] plus pure get/set accessors.
//! - Pure operations ([`cycle_value`], [`clamp_int`], display formatting) that
//!   the widget and the action handler share, so both agree on behavior.
//!
//! Keeping this layer pure makes the menu's logic unit-testable without spinning
//! up a TUI or an `App`.

use crate::config::GraphRenderer;

/// Which area of the app a setting belongs to. Also the menu's section order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingGroup {
    Graph,
    Files,
    Refresh,
    Interface,
}

impl SettingGroup {
    /// Section order in the menu.
    pub const ALL: [SettingGroup; 4] = [
        Self::Graph,
        Self::Files,
        Self::Refresh,
        Self::Interface,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Graph => "Graph",
            Self::Files => "Files",
            Self::Refresh => "Refresh",
            Self::Interface => "Interface",
        }
    }
}

/// The theme selection stored in `config.ui.theme` (a free-form string on disk).
/// Modeled as a closed enum here so the menu can cycle it safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    Auto,
    Dark,
    Light,
}

impl ThemeChoice {
    pub const OPTIONS: &'static [&'static str] = &["auto", "dark", "light"];

    /// Parse from the on-disk string; anything unrecognized falls back to Auto.
    /// Deliberately infallible (unknown → Auto), so `std::str::FromStr` — whose
    /// `Result` would force callers to handle an error that can't happen — is a
    /// poor fit here.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "dark" => Self::Dark,
            "light" => Self::Light,
            _ => Self::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Auto => 0,
            Self::Dark => 1,
            Self::Light => 2,
        }
    }

    fn from_index(i: usize) -> Self {
        match i {
            1 => Self::Dark,
            2 => Self::Light,
            _ => Self::Auto,
        }
    }
}

/// Index mapping for the graph renderer enum (order matches [`RENDERER_OPTIONS`]).
pub const RENDERER_OPTIONS: &[&str] = &["auto", "unicode", "pixel"];

fn renderer_index(r: GraphRenderer) -> usize {
    match r {
        GraphRenderer::Auto => 0,
        GraphRenderer::Unicode => 1,
        GraphRenderer::Pixel => 2,
    }
}

fn renderer_from_index(i: usize) -> GraphRenderer {
    match i {
        1 => GraphRenderer::Unicode,
        2 => GraphRenderer::Pixel,
        _ => GraphRenderer::Auto,
    }
}

/// Flat snapshot of every editable setting. Plain data — no behavior beyond the
/// descriptor accessors. `graph_width_cap == 0` means "uncapped" (`None`).
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsModel {
    // Graph
    pub trace_enabled: bool,
    pub hide_remote_branches: bool,
    pub hide_merged_branches: bool,
    pub mute_merges: bool,
    pub mute_base_merges: bool,
    pub collapse_merges: bool,
    pub avatars: bool,
    pub col_author: bool,
    pub col_hash: bool,
    pub col_date: bool,
    pub graph_renderer: GraphRenderer,
    pub graph_split_ratio: u16,
    /// 0 = uncapped.
    pub graph_width_cap: u16,
    // Files
    pub diff_word_wrap: bool,
    // Refresh
    pub auto_refresh: bool,
    pub refresh_interval: u64,
    pub auto_fetch: bool,
    pub fetch_interval: u64,
    // Interface
    pub side_panel_layout: bool,
    pub theme: ThemeChoice,
}

/// The typed shape of a setting — drives display and the generic cycle/edit ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    Bool,
    /// One of a fixed set of option tokens (cycled forward, wrapping).
    Enum {
        options: &'static [&'static str],
    },
    /// Unsigned integer with inclusive bounds. `step` is the amount a single
    /// Space press adds (wrapping from `max` back to `min`). `zero_label`, when
    /// set, is shown instead of `0` (e.g. "uncapped").
    Int {
        min: u64,
        max: u64,
        step: u64,
        zero_label: Option<&'static str>,
    },
}

/// A value of a setting, tagged to match its [`SettingKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingValue {
    Bool(bool),
    /// Index into the descriptor's `Enum { options }`.
    Enum(usize),
    Int(u64),
}

/// One row in the settings menu: metadata plus pure accessors over the model.
pub struct SettingDescriptor {
    pub label: &'static str,
    pub group: SettingGroup,
    pub kind: SettingKind,
    /// Dim hint rendered after the value, e.g. "restart" for restart-only keys.
    pub note: Option<&'static str>,
    /// Persistence destination, shown truthfully in the menu footer/help.
    pub store: SettingStore,
    get: fn(&SettingsModel) -> SettingValue,
    set: fn(&mut SettingsModel, SettingValue),
}

/// Where a setting is persisted — surfaced so the UI can tell the truth about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingStore {
    /// state.toml (UI state).
    State,
    /// config.toml (user config; comments/unknown keys preserved on write).
    Config,
}

impl SettingDescriptor {
    pub fn get(&self, m: &SettingsModel) -> SettingValue {
        (self.get)(m)
    }

    pub fn set(&self, m: &mut SettingsModel, v: SettingValue) {
        (self.set)(m, v)
    }

    /// Human-readable current value (right-aligned column in the menu).
    pub fn display_value(&self, m: &SettingsModel) -> String {
        match (self.kind, self.get(m)) {
            (SettingKind::Bool, SettingValue::Bool(b)) => {
                if b { "On".into() } else { "Off".into() }
            }
            (SettingKind::Enum { options }, SettingValue::Enum(i)) => {
                options.get(i).copied().unwrap_or("?").to_string()
            }
            (SettingKind::Int { zero_label, .. }, SettingValue::Int(n)) => match zero_label {
                Some(label) if n == 0 => label.to_string(),
                _ => n.to_string(),
            },
            // Kind/value mismatch is a programming error; render defensively.
            _ => "?".to_string(),
        }
    }
}

/// Advance a value one step for its kind: bools flip, enums cycle forward with
/// wraparound, ints add `step` and wrap from beyond `max` back to `min`.
pub fn cycle_value(kind: &SettingKind, v: SettingValue) -> SettingValue {
    match (kind, v) {
        (SettingKind::Bool, SettingValue::Bool(b)) => SettingValue::Bool(!b),
        (SettingKind::Enum { options }, SettingValue::Enum(i)) => {
            let n = options.len().max(1);
            SettingValue::Enum((i + 1) % n)
        }
        (SettingKind::Int { min, max, step, .. }, SettingValue::Int(n)) => {
            let next = n.saturating_add(*step);
            SettingValue::Int(if next > *max { *min } else { next })
        }
        // Mismatched kind/value: leave unchanged.
        (_, other) => other,
    }
}

/// Clamp a typed integer into the setting's inclusive bounds.
pub fn clamp_int(kind: &SettingKind, n: u64) -> u64 {
    match kind {
        SettingKind::Int { min, max, .. } => n.clamp(*min, *max),
        _ => n,
    }
}

/// The full, ordered settings registry. Rows are grouped by [`SettingGroup`];
/// the menu renders a section header when the group changes. The list order is
/// the navigation order (headers are not selectable).
pub fn descriptors() -> Vec<SettingDescriptor> {
    use SettingGroup::*;
    vec![
        // ── Graph ──────────────────────────────────────────────────
        SettingDescriptor {
            label: "Branch tracing",
            group: Graph,
            kind: SettingKind::Bool,
            note: None,
            store: SettingStore::State,
            get: |m| SettingValue::Bool(m.trace_enabled),
            set: |m, v| set_bool(v, &mut m.trace_enabled),
        },
        SettingDescriptor {
            label: "Hide remote branches",
            group: Graph,
            kind: SettingKind::Bool,
            note: None,
            store: SettingStore::State,
            get: |m| SettingValue::Bool(m.hide_remote_branches),
            set: |m, v| set_bool(v, &mut m.hide_remote_branches),
        },
        SettingDescriptor {
            label: "Hide merged branches",
            group: Graph,
            kind: SettingKind::Bool,
            note: None,
            store: SettingStore::State,
            get: |m| SettingValue::Bool(m.hide_merged_branches),
            set: |m, v| set_bool(v, &mut m.hide_merged_branches),
        },
        SettingDescriptor {
            label: "Mute merge commits",
            group: Graph,
            kind: SettingKind::Bool,
            note: None,
            store: SettingStore::State,
            get: |m| SettingValue::Bool(m.mute_merges),
            set: |m, v| set_bool(v, &mut m.mute_merges),
        },
        SettingDescriptor {
            label: "Mute base-update merges",
            group: Graph,
            kind: SettingKind::Bool,
            note: None,
            store: SettingStore::State,
            get: |m| SettingValue::Bool(m.mute_base_merges),
            set: |m, v| set_bool(v, &mut m.mute_base_merges),
        },
        SettingDescriptor {
            label: "Collapse merge messages",
            group: Graph,
            kind: SettingKind::Bool,
            note: None,
            store: SettingStore::State,
            get: |m| SettingValue::Bool(m.collapse_merges),
            set: |m, v| set_bool(v, &mut m.collapse_merges),
        },
        SettingDescriptor {
            label: "Author avatars",
            group: Graph,
            kind: SettingKind::Bool,
            note: Some("pixel mode"),
            store: SettingStore::State,
            get: |m| SettingValue::Bool(m.avatars),
            set: |m, v| set_bool(v, &mut m.avatars),
        },
        SettingDescriptor {
            label: "Column: author",
            group: Graph,
            kind: SettingKind::Bool,
            note: None,
            store: SettingStore::State,
            get: |m| SettingValue::Bool(m.col_author),
            set: |m, v| set_bool(v, &mut m.col_author),
        },
        SettingDescriptor {
            label: "Column: hash",
            group: Graph,
            kind: SettingKind::Bool,
            note: None,
            store: SettingStore::State,
            get: |m| SettingValue::Bool(m.col_hash),
            set: |m, v| set_bool(v, &mut m.col_hash),
        },
        SettingDescriptor {
            label: "Column: date",
            group: Graph,
            kind: SettingKind::Bool,
            note: None,
            store: SettingStore::State,
            get: |m| SettingValue::Bool(m.col_date),
            set: |m, v| set_bool(v, &mut m.col_date),
        },
        SettingDescriptor {
            label: "Graph renderer",
            group: Graph,
            kind: SettingKind::Enum {
                options: RENDERER_OPTIONS,
            },
            note: Some("restart"),
            store: SettingStore::Config,
            get: |m| SettingValue::Enum(renderer_index(m.graph_renderer)),
            set: |m, v| {
                if let SettingValue::Enum(i) = v {
                    m.graph_renderer = renderer_from_index(i);
                }
            },
        },
        SettingDescriptor {
            label: "Graph split ratio %",
            group: Graph,
            kind: SettingKind::Int {
                min: 20,
                max: 80,
                step: 5,
                zero_label: None,
            },
            note: None,
            store: SettingStore::State,
            get: |m| SettingValue::Int(m.graph_split_ratio as u64),
            set: |m, v| {
                if let SettingValue::Int(n) = v {
                    m.graph_split_ratio = n as u16;
                }
            },
        },
        SettingDescriptor {
            label: "Graph width cap",
            group: Graph,
            kind: SettingKind::Int {
                min: 0,
                max: 40,
                step: 2,
                zero_label: Some("uncapped"),
            },
            note: None,
            store: SettingStore::State,
            get: |m| SettingValue::Int(m.graph_width_cap as u64),
            set: |m, v| {
                if let SettingValue::Int(n) = v {
                    m.graph_width_cap = n as u16;
                }
            },
        },
        // ── Files ──────────────────────────────────────────────────
        SettingDescriptor {
            label: "Diff line wrap",
            group: Files,
            kind: SettingKind::Bool,
            note: None,
            store: SettingStore::State,
            get: |m| SettingValue::Bool(m.diff_word_wrap),
            set: |m, v| set_bool(v, &mut m.diff_word_wrap),
        },
        // ── Refresh ────────────────────────────────────────────────
        SettingDescriptor {
            label: "Auto-refresh",
            group: Refresh,
            kind: SettingKind::Bool,
            note: None,
            store: SettingStore::Config,
            get: |m| SettingValue::Bool(m.auto_refresh),
            set: |m, v| set_bool(v, &mut m.auto_refresh),
        },
        SettingDescriptor {
            label: "Refresh interval (s)",
            group: Refresh,
            kind: SettingKind::Int {
                min: 1,
                max: 3600,
                step: 5,
                zero_label: None,
            },
            note: None,
            store: SettingStore::Config,
            get: |m| SettingValue::Int(m.refresh_interval),
            set: |m, v| {
                if let SettingValue::Int(n) = v {
                    m.refresh_interval = n;
                }
            },
        },
        SettingDescriptor {
            label: "Auto-fetch",
            group: Refresh,
            kind: SettingKind::Bool,
            note: None,
            store: SettingStore::Config,
            get: |m| SettingValue::Bool(m.auto_fetch),
            set: |m, v| set_bool(v, &mut m.auto_fetch),
        },
        SettingDescriptor {
            label: "Fetch interval (s)",
            group: Refresh,
            kind: SettingKind::Int {
                min: 10,
                max: 3600,
                step: 10,
                zero_label: None,
            },
            note: None,
            store: SettingStore::Config,
            get: |m| SettingValue::Int(m.fetch_interval),
            set: |m, v| {
                if let SettingValue::Int(n) = v {
                    m.fetch_interval = n;
                }
            },
        },
        // ── Interface ──────────────────────────────────────────────
        SettingDescriptor {
            label: "Side-panel layout",
            group: Interface,
            kind: SettingKind::Bool,
            note: None,
            store: SettingStore::State,
            get: |m| SettingValue::Bool(m.side_panel_layout),
            set: |m, v| set_bool(v, &mut m.side_panel_layout),
        },
        SettingDescriptor {
            label: "Theme",
            group: Interface,
            kind: SettingKind::Enum {
                options: ThemeChoice::OPTIONS,
            },
            note: None,
            store: SettingStore::Config,
            get: |m| SettingValue::Enum(m.theme.index()),
            set: |m, v| {
                if let SettingValue::Enum(i) = v {
                    m.theme = ThemeChoice::from_index(i);
                }
            },
        },
    ]
}

fn set_bool(v: SettingValue, target: &mut bool) {
    if let SettingValue::Bool(b) = v {
        *target = b;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SettingsModel {
        SettingsModel {
            trace_enabled: true,
            hide_remote_branches: false,
            hide_merged_branches: false,
            mute_merges: true,
            mute_base_merges: false,
            collapse_merges: false,
            avatars: false,
            col_author: true,
            col_hash: true,
            col_date: true,
            graph_renderer: GraphRenderer::Auto,
            graph_split_ratio: 65,
            graph_width_cap: 0,
            diff_word_wrap: false,
            auto_refresh: true,
            refresh_interval: 10,
            auto_fetch: true,
            fetch_interval: 60,
            side_panel_layout: false,
            theme: ThemeChoice::Auto,
        }
    }

    #[test]
    fn descriptors_cover_every_group_and_have_unique_labels() {
        let ds = descriptors();
        assert!(ds.len() >= 15, "expected the full settings inventory");
        for g in SettingGroup::ALL {
            assert!(
                ds.iter().any(|d| d.group == g),
                "group {:?} has no settings",
                g
            );
        }
        let mut labels: Vec<_> = ds.iter().map(|d| d.label).collect();
        labels.sort_unstable();
        let before = labels.len();
        labels.dedup();
        assert_eq!(before, labels.len(), "duplicate setting labels");
    }

    #[test]
    fn descriptors_are_grouped_contiguously_in_section_order() {
        // Each group appears as a single contiguous run, in SettingGroup::ALL
        // order, so the menu can render one header per group.
        let ds = descriptors();
        let order: Vec<SettingGroup> = ds.iter().map(|d| d.group).collect();
        let mut runs: Vec<SettingGroup> = Vec::new();
        for g in order {
            if runs.last() != Some(&g) {
                runs.push(g);
            }
        }
        assert_eq!(runs, SettingGroup::ALL.to_vec());
    }

    #[test]
    fn every_descriptor_round_trips_get_then_set() {
        // set(get(m)) must be a no-op for all settings — accessors are consistent.
        let m = sample();
        for d in descriptors() {
            let mut copy = m.clone();
            let v = d.get(&copy);
            d.set(&mut copy, v);
            assert_eq!(copy, m, "descriptor '{}' get/set not consistent", d.label);
        }
    }

    #[test]
    fn cycle_bool_flips() {
        let k = SettingKind::Bool;
        assert_eq!(
            cycle_value(&k, SettingValue::Bool(false)),
            SettingValue::Bool(true)
        );
        assert_eq!(
            cycle_value(&k, SettingValue::Bool(true)),
            SettingValue::Bool(false)
        );
    }

    #[test]
    fn cycle_enum_advances_and_wraps() {
        let k = SettingKind::Enum {
            options: RENDERER_OPTIONS,
        };
        assert_eq!(cycle_value(&k, SettingValue::Enum(0)), SettingValue::Enum(1));
        assert_eq!(cycle_value(&k, SettingValue::Enum(1)), SettingValue::Enum(2));
        // wraps 2 -> 0
        assert_eq!(cycle_value(&k, SettingValue::Enum(2)), SettingValue::Enum(0));
    }

    #[test]
    fn cycle_int_steps_and_wraps_at_max() {
        let k = SettingKind::Int {
            min: 20,
            max: 80,
            step: 5,
            zero_label: None,
        };
        assert_eq!(cycle_value(&k, SettingValue::Int(20)), SettingValue::Int(25));
        assert_eq!(cycle_value(&k, SettingValue::Int(75)), SettingValue::Int(80));
        // 80 + 5 > 80 → wrap to min
        assert_eq!(cycle_value(&k, SettingValue::Int(80)), SettingValue::Int(20));
    }

    #[test]
    fn clamp_int_respects_bounds() {
        let k = SettingKind::Int {
            min: 10,
            max: 3600,
            step: 10,
            zero_label: None,
        };
        assert_eq!(clamp_int(&k, 0), 10);
        assert_eq!(clamp_int(&k, 5), 10);
        assert_eq!(clamp_int(&k, 100), 100);
        assert_eq!(clamp_int(&k, 999_999), 3600);
    }

    #[test]
    fn display_value_formats_each_kind() {
        let mut m = sample();
        let ds = descriptors();
        let find = |label: &str| ds.iter().find(|d| d.label == label).unwrap();

        m.trace_enabled = true;
        assert_eq!(find("Branch tracing").display_value(&m), "On");
        m.trace_enabled = false;
        assert_eq!(find("Branch tracing").display_value(&m), "Off");

        m.theme = ThemeChoice::Dark;
        assert_eq!(find("Theme").display_value(&m), "dark");

        m.refresh_interval = 42;
        assert_eq!(find("Refresh interval (s)").display_value(&m), "42");

        // Zero-label special case.
        m.graph_width_cap = 0;
        assert_eq!(find("Graph width cap").display_value(&m), "uncapped");
        m.graph_width_cap = 8;
        assert_eq!(find("Graph width cap").display_value(&m), "8");
    }

    #[test]
    fn theme_choice_parses_and_round_trips() {
        assert_eq!(ThemeChoice::from_str("dark"), ThemeChoice::Dark);
        assert_eq!(ThemeChoice::from_str("light"), ThemeChoice::Light);
        assert_eq!(ThemeChoice::from_str("auto"), ThemeChoice::Auto);
        // Unknown falls back to Auto.
        assert_eq!(ThemeChoice::from_str("solarized"), ThemeChoice::Auto);
        for t in [ThemeChoice::Auto, ThemeChoice::Dark, ThemeChoice::Light] {
            assert_eq!(ThemeChoice::from_index(t.index()), t);
            assert_eq!(ThemeChoice::from_str(t.as_str()), t);
        }
    }

    #[test]
    fn renderer_index_round_trips() {
        for r in [
            GraphRenderer::Auto,
            GraphRenderer::Unicode,
            GraphRenderer::Pixel,
        ] {
            assert_eq!(renderer_from_index(renderer_index(r)), r);
        }
    }

    #[test]
    fn cycling_the_renderer_via_descriptor_advances_the_enum() {
        // End-to-end pure flow: read, cycle, write, read back.
        let mut m = sample();
        let ds = descriptors();
        let d = ds.iter().find(|d| d.label == "Graph renderer").unwrap();
        let next = cycle_value(&d.kind, d.get(&m));
        d.set(&mut m, next);
        assert_eq!(m.graph_renderer, GraphRenderer::Unicode);
    }
}
