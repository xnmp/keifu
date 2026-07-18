//! Configuration management

use std::fs;

use serde::{Deserialize, Serialize};

/// Application configuration
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub refresh: RefreshConfig,
    pub ui: UiConfig,
}

/// UI configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// Theme name: "dark" or "light"
    pub theme: String,
    /// How the commit graph lines are rendered.
    pub graph_renderer: GraphRenderer,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "auto".to_string(),
            graph_renderer: GraphRenderer::default(),
        }
    }
}

/// Commit graph rendering strategy.
///
/// - `Auto`: use pixel rendering when the terminal supports a graphics protocol,
///   otherwise fall back to Unicode box-drawing glyphs.
/// - `Unicode`: always use box-drawing glyphs.
/// - `Pixel`: force pixel rendering; silently falls back to Unicode when no
///   graphics protocol is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphRenderer {
    #[default]
    Auto,
    Unicode,
    Pixel,
}

/// Auto-refresh configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RefreshConfig {
    /// Enable auto-refresh for local state (commits, branches, working tree)
    pub auto_refresh: bool,
    /// Interval in seconds for local refresh (minimum: 1, default: 10)
    #[serde(deserialize_with = "deserialize_refresh_interval")]
    pub refresh_interval: u64,
    /// Enable auto-fetch from remote
    pub auto_fetch: bool,
    /// Interval in seconds for remote fetch (minimum: 10, default: 60)
    #[serde(deserialize_with = "deserialize_fetch_interval")]
    pub fetch_interval: u64,
}

impl Default for RefreshConfig {
    fn default() -> Self {
        Self {
            auto_refresh: true,
            refresh_interval: 10,
            auto_fetch: true,
            fetch_interval: 60,
        }
    }
}

fn deserialize_refresh_interval<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = u64::deserialize(deserializer)?;
    Ok(value.max(1))
}

fn deserialize_fetch_interval<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = u64::deserialize(deserializer)?;
    Ok(value.max(10))
}

impl Config {
    /// Load config from ~/.config/keifu/config.toml
    /// Returns default config if file doesn't exist or is invalid
    pub fn load() -> Self {
        let path = dirs::config_dir()
            .map(|p| p.join("keifu/config.toml"))
            .filter(|p| p.exists());

        let Some(path) = path else {
            return Self::default();
        };

        fs::read_to_string(&path)
            .ok()
            .and_then(|content| toml::from_str(&content).ok())
            .unwrap_or_default()
    }
}

/// Which right-aligned metadata columns are shown on each commit row. A hidden
/// column's width flows to the commit message. Defaults to all visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetadataColumns {
    pub author: bool,
    pub hash: bool,
    pub date: bool,
}

impl Default for MetadataColumns {
    fn default() -> Self {
        Self {
            author: true,
            hash: true,
            date: true,
        }
    }
}

/// A single toggleable metadata column. `ALL` is also the menu display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataColumn {
    Author,
    Hash,
    Date,
}

impl MetadataColumn {
    pub const ALL: [MetadataColumn; 3] = [Self::Author, Self::Hash, Self::Date];

    pub fn label(self) -> &'static str {
        match self {
            Self::Author => "Author",
            Self::Hash => "Hash",
            Self::Date => "Date",
        }
    }
}

impl MetadataColumns {
    pub fn is_visible(&self, col: MetadataColumn) -> bool {
        match col {
            MetadataColumn::Author => self.author,
            MetadataColumn::Hash => self.hash,
            MetadataColumn::Date => self.date,
        }
    }

    pub fn toggle(&mut self, col: MetadataColumn) {
        match col {
            MetadataColumn::Author => self.author = !self.author,
            MetadataColumn::Hash => self.hash = !self.hash,
            MetadataColumn::Date => self.date = !self.date,
        }
    }
}

/// Persistent UI state saved between sessions.
///
/// Field order matters for TOML: scalar values must be emitted before the
/// `[metadata_columns]` table, so keep `metadata_columns` last.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    pub side_panel_layout: bool,
    /// Cap on the graph column width in cells; `None` = uncapped (fit all lanes,
    /// the default). A cap wider than a later graph's needed width is uncapped.
    pub graph_width_cap: Option<usize>,
    pub metadata_columns: MetadataColumns,
}

impl UiState {
    fn state_path() -> Option<std::path::PathBuf> {
        dirs::config_dir().map(|p| p.join("keifu/state.toml"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::state_path() else {
            return Self::default();
        };
        if !path.exists() {
            return Self::default();
        }
        fs::read_to_string(&path)
            .ok()
            .and_then(|content| toml::from_str(&content).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::state_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(content) = toml::to_string(self) {
            let _ = fs::write(&path, content);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_toml_with_all_fields() {
        let toml_str = r#"
            [refresh]
            auto_refresh = false
            refresh_interval = 30
            auto_fetch = false
            fetch_interval = 120

            [ui]
            theme = "light"
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert!(!cfg.refresh.auto_refresh);
        assert_eq!(cfg.refresh.refresh_interval, 30);
        assert!(!cfg.refresh.auto_fetch);
        assert_eq!(cfg.refresh.fetch_interval, 120);
        assert_eq!(cfg.ui.theme, "light");
    }

    #[test]
    fn refresh_interval_zero_clamps_to_one() {
        let toml_str = r#"
            [refresh]
            refresh_interval = 0
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.refresh.refresh_interval, 1);
    }

    #[test]
    fn fetch_interval_below_minimum_clamps_to_ten() {
        let toml_str = r#"
            [refresh]
            fetch_interval = 3
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.refresh.fetch_interval, 10);
    }

    #[test]
    fn interval_values_above_minimum_pass_through_unchanged() {
        let toml_str = r#"
            [refresh]
            refresh_interval = 45
            fetch_interval = 300
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.refresh.refresh_interval, 45);
        assert_eq!(cfg.refresh.fetch_interval, 300);
    }

    #[test]
    fn malformed_toml_fails_to_parse() {
        // Unterminated inline table — invalid TOML syntax.
        let bad = "refresh = { auto_refresh = tru";
        let result: Result<Config, _> = toml::from_str(bad);
        assert!(result.is_err());
        // Config::load() converts this Err via `.ok()` into `None`, then
        // `.unwrap_or_default()` falls back to `Config::default()` — a
        // malformed config file on disk is silently replaced by defaults
        // rather than surfacing a parse error to the user.
    }

    #[test]
    fn missing_sections_fall_back_to_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.refresh.auto_refresh);
        assert_eq!(cfg.refresh.refresh_interval, 10);
        assert!(cfg.refresh.auto_fetch);
        assert_eq!(cfg.refresh.fetch_interval, 60);
        assert_eq!(cfg.ui.theme, "auto");
    }

    #[test]
    fn partial_config_missing_refresh_section_uses_defaults() {
        let toml_str = r#"
            [ui]
            theme = "dark"
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.ui.theme, "dark");
        assert!(cfg.refresh.auto_refresh);
        assert_eq!(cfg.refresh.refresh_interval, 10);
        assert_eq!(cfg.refresh.fetch_interval, 60);
    }

    #[test]
    fn partial_config_missing_ui_section_uses_defaults() {
        let toml_str = r#"
            [refresh]
            auto_refresh = false
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert!(!cfg.refresh.auto_refresh);
        assert_eq!(cfg.ui.theme, "auto");
    }

    #[test]
    fn graph_renderer_defaults_to_auto() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.ui.graph_renderer, GraphRenderer::Auto);
    }

    #[test]
    fn graph_renderer_parses_each_variant() {
        for (raw, expected) in [
            ("auto", GraphRenderer::Auto),
            ("unicode", GraphRenderer::Unicode),
            ("pixel", GraphRenderer::Pixel),
        ] {
            let toml_str = format!("[ui]\ngraph_renderer = \"{raw}\"\n");
            let cfg: Config = toml::from_str(&toml_str).unwrap();
            assert_eq!(cfg.ui.graph_renderer, expected, "variant {raw}");
        }
    }

    #[test]
    fn graph_renderer_invalid_value_fails_to_parse() {
        let toml_str = r#"
            [ui]
            graph_renderer = "sixel"
        "#;
        let result: Result<Config, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn unknown_keys_are_ignored() {
        // No `deny_unknown_fields` on Config/UiConfig/RefreshConfig, so serde
        // silently drops keys it doesn't recognize instead of erroring —
        // pin that behavior so a future `deny_unknown_fields` addition is a
        // deliberate, visible change to this test.
        let toml_str = r#"
            unknown_top_level = "surprise"

            [refresh]
            auto_refresh = false
            unknown_refresh_key = 123

            [ui]
            theme = "light"
            unknown_ui_key = true
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert!(!cfg.refresh.auto_refresh);
        assert_eq!(cfg.ui.theme, "light");
    }

    // ── UiState / MetadataColumns ────────────────────────────────────

    #[test]
    fn ui_state_defaults_all_metadata_columns_visible() {
        let state: UiState = toml::from_str("").unwrap();
        assert!(!state.side_panel_layout);
        assert!(state.metadata_columns.author);
        assert!(state.metadata_columns.hash);
        assert!(state.metadata_columns.date);
    }

    #[test]
    fn ui_state_round_trips_metadata_columns() {
        let state = UiState {
            side_panel_layout: true,
            graph_width_cap: Some(8),
            metadata_columns: MetadataColumns {
                author: true,
                hash: false,
                date: false,
            },
        };
        let serialized = toml::to_string(&state).unwrap();
        let restored: UiState = toml::from_str(&serialized).unwrap();
        assert!(restored.side_panel_layout);
        assert_eq!(restored.graph_width_cap, Some(8));
        assert!(restored.metadata_columns.author);
        assert!(!restored.metadata_columns.hash);
        assert!(!restored.metadata_columns.date);
    }

    #[test]
    fn ui_state_uncapped_graph_width_omits_the_field() {
        // None serializes to nothing and round-trips back to None (uncapped).
        let state = UiState::default();
        assert_eq!(state.graph_width_cap, None);
        let serialized = toml::to_string(&state).unwrap();
        let restored: UiState = toml::from_str(&serialized).unwrap();
        assert_eq!(restored.graph_width_cap, None);
    }

    #[test]
    fn ui_state_missing_metadata_section_falls_back_to_all_visible() {
        // An older state.toml (before the fields existed) must still load, with
        // every column visible and the graph uncapped.
        let state: UiState = toml::from_str("side_panel_layout = true").unwrap();
        assert!(state.side_panel_layout);
        assert_eq!(state.graph_width_cap, None);
        assert_eq!(state.metadata_columns, MetadataColumns::default());
    }

    #[test]
    fn metadata_columns_toggle_flips_the_named_column_only() {
        let mut cols = MetadataColumns::default();
        cols.toggle(MetadataColumn::Hash);
        assert!(cols.author);
        assert!(!cols.hash);
        assert!(cols.date);
        assert!(!cols.is_visible(MetadataColumn::Hash));
        cols.toggle(MetadataColumn::Hash);
        assert!(cols.is_visible(MetadataColumn::Hash));
    }
}
