//! Settings registry — the single source of truth behind the Ctrl+, menu.
//!
//! Each user-facing setting is described exactly once, by one
//! [`SettingDescriptor`] in [`descriptors`]. A descriptor bundles everything the
//! rest of the app needs to know about that setting:
//!
//! - display metadata ([`label`](SettingDescriptor::label), group, [`SettingKind`], note),
//! - `get`/`set` accessors that read and write the live value directly on [`App`], and
//! - a [`SettingStore`] that says where the value persists — and, for state.toml
//!   settings, carries the read/write lens into [`UiState`].
//!
//! Because the App accessors and the persistence lens live in the *same* entry,
//! there is no hand-written projection to keep in sync: `App::settings_snapshot`
//! and `App::save_ui_state` are simple loops over this list. Adding a setting
//! touches one descriptor entry (plus the `App` field and, if state-persisted,
//! the `UiState` field it points at).
//!
//! Pure value operations ([`cycle_value`], [`clamp_int`], [`format_value`]) are
//! shared by the widget and the action handler so both agree on behavior, and
//! stay unit-testable without an `App`.

use crate::app::App;
use crate::config::{GraphRenderer, UiState};

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

/// The `graph_width_cap` encoding, defined once so every store and both
/// directions agree: the live/on-disk value is `Option<usize>` (`None` =
/// uncapped), while the menu edits it as an integer where `0` means uncapped.
fn cap_to_value(cap: Option<usize>) -> SettingValue {
    SettingValue::Int(cap.unwrap_or(0) as u64)
}

fn value_to_cap(v: SettingValue) -> Option<usize> {
    match v {
        SettingValue::Int(n) if n != 0 => Some(n as usize),
        _ => None,
    }
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

/// Where a setting is persisted. For state.toml settings this also carries the
/// read/write lens into [`UiState`], so the persistence mapping lives in the
/// same descriptor as the live-state accessors (no separate `save_ui_state`
/// projection to drift out of sync).
#[derive(Clone, Copy)]
pub enum SettingStore {
    /// state.toml (UI state). Carries the lens between the setting value and its
    /// field in [`UiState`].
    State {
        read: fn(&UiState) -> SettingValue,
        write: fn(&mut UiState, SettingValue),
    },
    /// config.toml — persisted wholesale via `Config::save` (the `set` accessor
    /// writes `app.config.*` directly, so there is no per-field lens to carry;
    /// comments and unknown keys are preserved on write).
    Config,
}

/// One row in the settings menu: display metadata, live-state accessors over
/// [`App`], and the persistence store.
pub struct SettingDescriptor {
    pub label: &'static str,
    pub group: SettingGroup,
    pub kind: SettingKind,
    /// Dim hint rendered after the value, e.g. "restart" for restart-only keys.
    pub note: Option<&'static str>,
    /// Persistence destination, shown truthfully in the menu footer/help.
    pub store: SettingStore,
    get: fn(&App) -> SettingValue,
    set: fn(&mut App, SettingValue),
}

impl SettingDescriptor {
    /// Read this setting's current value off the live app state.
    pub fn get(&self, app: &App) -> SettingValue {
        (self.get)(app)
    }

    /// Write a new value back onto the live app state.
    pub fn set(&self, app: &mut App, v: SettingValue) {
        (self.set)(app, v)
    }
}

/// Human-readable current value (right-aligned column in the menu).
pub fn format_value(kind: SettingKind, v: SettingValue) -> String {
    match (kind, v) {
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

fn set_bool(v: SettingValue, target: &mut bool) {
    if let SettingValue::Bool(b) = v {
        *target = b;
    }
}

/// A bool setting whose live value lives at `$app_field` on `App` and whose
/// persisted value lives at `$ui_field` on `UiState`.
macro_rules! state_bool {
    ($label:expr, $group:expr, $note:expr, $app:ident $(. $app_rest:ident)*, $ui:ident $(. $ui_rest:ident)*) => {
        SettingDescriptor {
            label: $label,
            group: $group,
            kind: SettingKind::Bool,
            note: $note,
            store: SettingStore::State {
                read: |u| SettingValue::Bool(u.$ui $(. $ui_rest)*),
                write: |u, v| set_bool(v, &mut u.$ui $(. $ui_rest)*),
            },
            get: |a| SettingValue::Bool(a.$app $(. $app_rest)*),
            set: |a, v| set_bool(v, &mut a.$app $(. $app_rest)*),
        }
    };
}

/// A bool setting persisted in config.toml, whose live value lives at
/// `app.config.$path` (so it is saved wholesale via `Config::save`).
macro_rules! config_bool {
    ($label:expr, $group:expr, $note:expr, $($path:ident).+) => {
        SettingDescriptor {
            label: $label,
            group: $group,
            kind: SettingKind::Bool,
            note: $note,
            store: SettingStore::Config,
            get: |a| SettingValue::Bool(a.config.$($path).+),
            set: |a, v| set_bool(v, &mut a.config.$($path).+),
        }
    };
}

/// A config.toml integer setting whose live value lives at `app.config.$path`.
macro_rules! config_int {
    ($label:expr, $group:expr, $kind:expr, $($path:ident).+) => {
        SettingDescriptor {
            label: $label,
            group: $group,
            kind: $kind,
            note: None,
            store: SettingStore::Config,
            get: |a| SettingValue::Int(a.config.$($path).+),
            set: |a, v| {
                if let SettingValue::Int(n) = v {
                    a.config.$($path).+ = n;
                }
            },
        }
    };
}

/// The full, ordered settings registry — the single source of truth. Rows are
/// grouped by [`SettingGroup`]; the menu renders a section header when the group
/// changes. The list order is the navigation order (headers are not selectable).
pub fn descriptors() -> Vec<SettingDescriptor> {
    use SettingGroup::*;
    vec![
        // ── Graph ──────────────────────────────────────────────────
        state_bool!("Branch tracing", Graph, None, trace_enabled, trace_enabled),
        state_bool!(
            "Hide remote branches",
            Graph,
            None,
            hide_remote_branches,
            hide_remote_branches
        ),
        state_bool!(
            "Hide merged branches",
            Graph,
            None,
            merged.hide,
            hide_merged_branches
        ),
        state_bool!(
            "Mute merge commits",
            Graph,
            None,
            metadata_columns.mute_merges,
            metadata_columns.mute_merges
        ),
        state_bool!(
            "Mute base-update merges",
            Graph,
            None,
            metadata_columns.mute_base_merges,
            metadata_columns.mute_base_merges
        ),
        state_bool!(
            "Collapse merge messages",
            Graph,
            None,
            metadata_columns.collapse_merges,
            metadata_columns.collapse_merges
        ),
        state_bool!(
            "Author avatars",
            Graph,
            Some("pixel mode"),
            metadata_columns.avatars,
            metadata_columns.avatars
        ),
        state_bool!(
            "Show author column",
            Graph,
            None,
            metadata_columns.author,
            metadata_columns.author
        ),
        state_bool!(
            "Show hash column",
            Graph,
            None,
            metadata_columns.hash,
            metadata_columns.hash
        ),
        state_bool!(
            "Show date column",
            Graph,
            None,
            metadata_columns.date,
            metadata_columns.date
        ),
        SettingDescriptor {
            label: "Graph renderer",
            group: Graph,
            kind: SettingKind::Enum {
                options: RENDERER_OPTIONS,
            },
            note: Some("restart"),
            store: SettingStore::Config,
            get: |a| SettingValue::Enum(renderer_index(a.config.ui.graph_renderer)),
            set: |a, v| {
                if let SettingValue::Enum(i) = v {
                    a.config.ui.graph_renderer = renderer_from_index(i);
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
            store: SettingStore::State {
                read: |u| SettingValue::Int(u.graph_split_ratio as u64),
                write: |u, v| {
                    if let SettingValue::Int(n) = v {
                        u.graph_split_ratio = n as u16;
                    }
                },
            },
            get: |a| SettingValue::Int(a.graph_split_ratio as u64),
            set: |a, v| {
                if let SettingValue::Int(n) = v {
                    a.graph_split_ratio = n as u16;
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
            store: SettingStore::State {
                read: |u| cap_to_value(u.graph_width_cap),
                write: |u, v| u.graph_width_cap = value_to_cap(v),
            },
            get: |a| cap_to_value(a.graph_width_cap),
            set: |a, v| a.graph_width_cap = value_to_cap(v),
        },
        // ── Files ──────────────────────────────────────────────────
        state_bool!("Diff line wrap", Files, None, diff_word_wrap, diff_word_wrap),
        state_bool!(
            "Group files by folder",
            Files,
            None,
            files_pane.files_group_by_folder,
            files_group_by_folder
        ),
        // ── Refresh ────────────────────────────────────────────────
        config_bool!("Auto-refresh", Refresh, None, refresh.auto_refresh),
        config_int!(
            "Refresh interval (s)",
            Refresh,
            SettingKind::Int {
                min: 1,
                max: 3600,
                step: 5,
                zero_label: None,
            },
            refresh.refresh_interval
        ),
        config_bool!("Auto-fetch", Refresh, None, refresh.auto_fetch),
        config_int!(
            "Fetch interval (s)",
            Refresh,
            SettingKind::Int {
                min: 10,
                max: 3600,
                step: 10,
                zero_label: None,
            },
            refresh.fetch_interval
        ),
        config_bool!(
            "Fast-forward on refresh",
            Refresh,
            None,
            refresh.fast_forward_on_refresh
        ),
        // ── Interface ──────────────────────────────────────────────
        state_bool!(
            "Side-panel layout",
            Interface,
            None,
            side_panel_layout,
            side_panel_layout
        ),
        SettingDescriptor {
            label: "Theme",
            group: Interface,
            kind: SettingKind::Enum {
                options: ThemeChoice::OPTIONS,
            },
            note: None,
            store: SettingStore::Config,
            get: |a| SettingValue::Enum(ThemeChoice::from_str(&a.config.ui.theme).index()),
            set: |a, v| {
                if let SettingValue::Enum(i) = v {
                    a.config.ui.theme = ThemeChoice::from_index(i).as_str().to_string();
                    // Both caches bake theme colors in; drop them so the new
                    // palette shows without waiting for a graph rebuild or a
                    // selection move.
                    a.pixel_specs_cache = None;
                    a.trace_cache = None;
                }
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MetadataColumns, UiState};

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
    fn state_settings_persistence_round_trips_every_ui_state_field() {
        // The single-source guarantee: writing each state setting's read value
        // back into a fresh UiState reproduces the original — proving the read/
        // write lenses agree and together cover every UiState field. This is what
        // used to break when settings_model()/apply_settings_model() drifted.
        let custom = UiState {
            side_panel_layout: true,
            graph_width_cap: Some(12),
            graph_split_ratio: 40,
            trace_enabled: false,
            hide_remote_branches: true,
            diff_word_wrap: true,
            hide_merged_branches: true,
            files_group_by_folder: true,
            metadata_columns: MetadataColumns {
                author: false,
                hash: false,
                date: false,
                mute_merges: true,
                mute_base_merges: true,
                collapse_merges: true,
                avatars: true,
            },
        };
        let mut rebuilt = UiState::default();
        for d in descriptors() {
            if let SettingStore::State { read, write } = d.store {
                write(&mut rebuilt, read(&custom));
            }
        }
        assert_eq!(
            rebuilt, custom,
            "state persistence lenses must cover and round-trip every UiState field"
        );
    }

    #[test]
    fn graph_width_cap_encoding_round_trips_both_ways() {
        // Uncapped <-> 0 in a single place (cap_to_value / value_to_cap).
        assert_eq!(cap_to_value(None), SettingValue::Int(0));
        assert_eq!(cap_to_value(Some(8)), SettingValue::Int(8));
        assert_eq!(value_to_cap(SettingValue::Int(0)), None);
        assert_eq!(value_to_cap(SettingValue::Int(8)), Some(8));
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
    fn format_value_renders_each_kind() {
        assert_eq!(
            format_value(SettingKind::Bool, SettingValue::Bool(true)),
            "On"
        );
        assert_eq!(
            format_value(SettingKind::Bool, SettingValue::Bool(false)),
            "Off"
        );

        let theme = SettingKind::Enum {
            options: ThemeChoice::OPTIONS,
        };
        assert_eq!(format_value(theme, SettingValue::Enum(1)), "dark");

        let interval = SettingKind::Int {
            min: 1,
            max: 3600,
            step: 5,
            zero_label: None,
        };
        assert_eq!(format_value(interval, SettingValue::Int(42)), "42");

        // Zero-label special case.
        let cap = SettingKind::Int {
            min: 0,
            max: 40,
            step: 2,
            zero_label: Some("uncapped"),
        };
        assert_eq!(format_value(cap, SettingValue::Int(0)), "uncapped");
        assert_eq!(format_value(cap, SettingValue::Int(8)), "8");
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
}
