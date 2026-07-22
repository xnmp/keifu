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
    /// Draw a subtle grey link line from a squash-merged branch's tip to the
    /// commit that landed it on the trunk (issue #81). On by default (#100:
    /// users read a missing link line as "squash not detected").
    pub squash_link_lines: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "auto".to_string(),
            graph_renderer: GraphRenderer::default(),
            squash_link_lines: true,
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

impl GraphRenderer {
    /// The lowercase token used in config.toml (matches `serde(rename_all)`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Unicode => "unicode",
            Self::Pixel => "pixel",
        }
    }
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
    /// On a manual refresh (F5), fast-forward local branches strictly behind
    /// their upstream (no divergence). Off by default.
    pub fast_forward_on_refresh: bool,
}

impl Default for RefreshConfig {
    fn default() -> Self {
        Self {
            auto_refresh: true,
            refresh_interval: 10,
            auto_fetch: true,
            fetch_interval: 60,
            fast_forward_on_refresh: false,
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
    fn config_path() -> Option<std::path::PathBuf> {
        dirs::config_dir().map(|p| p.join("keifu/config.toml"))
    }

    /// Load config from ~/.config/keifu/config.toml
    /// Returns default config if file doesn't exist or is invalid
    pub fn load() -> Self {
        let path = Self::config_path().filter(|p| p.exists());

        let Some(path) = path else {
            return Self::default();
        };

        fs::read_to_string(&path)
            .ok()
            .and_then(|content| toml::from_str(&content).ok())
            .unwrap_or_default()
    }

    /// Persist the menu-managed settings back to config.toml, preserving the
    /// file's existing comments, formatting, and any keys keifu doesn't model
    /// (unknown keys). Uses `toml_edit` so only the specific values are rewritten
    /// in place rather than re-serializing the whole struct.
    pub fn save(&self) {
        let Some(path) = Self::config_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let mut doc = fs::read_to_string(&path)
            .ok()
            .and_then(|content| content.parse::<toml_edit::DocumentMut>().ok())
            .unwrap_or_default();

        self.apply_to_document(&mut doc);
        let _ = fs::write(&path, doc.to_string());
    }

    /// Write the menu-managed values into an existing TOML document in place.
    /// Pure (no I/O) so it can be unit-tested and so `save` reuses it. Existing
    /// comments, key ordering, and unknown keys in `doc` are left untouched.
    pub fn apply_to_document(&self, doc: &mut toml_edit::DocumentMut) {
        use toml_edit::value;
        // Seed missing sections as standard (non-inline) tables so a freshly
        // created config.toml reads as `[refresh]` / `[ui]` blocks rather than
        // inline tables. Existing tables (with their comments) are left as-is.
        for section in ["refresh", "ui"] {
            if doc.get(section).is_none() {
                doc[section] = toml_edit::table();
            }
        }
        doc["refresh"]["auto_refresh"] = value(self.refresh.auto_refresh);
        doc["refresh"]["refresh_interval"] = value(self.refresh.refresh_interval as i64);
        doc["refresh"]["auto_fetch"] = value(self.refresh.auto_fetch);
        doc["refresh"]["fetch_interval"] = value(self.refresh.fetch_interval as i64);
        doc["refresh"]["fast_forward_on_refresh"] = value(self.refresh.fast_forward_on_refresh);
        doc["ui"]["theme"] = value(self.ui.theme.clone());
        doc["ui"]["graph_renderer"] = value(self.ui.graph_renderer.as_str());
        doc["ui"]["squash_link_lines"] = value(self.ui.squash_link_lines);
    }
}

/// Toggleable per-row display options shown in the Shift+M menu: which
/// right-aligned metadata columns render (a hidden column's width flows to the
/// message), plus whether merge commits are visually muted. Defaults to all on
/// except avatars (visual noise when most authors resolve to the same fallback
/// disc; opt in via Shift+M).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetadataColumns {
    pub author: bool,
    pub hash: bool,
    pub date: bool,
    /// Dim the message of merge commits (VSCode Git Graph style).
    pub mute_merges: bool,
    /// Strongly mute base-update ("back-merge") commits and their connector: a
    /// merge on an open PR's branch that pulled the updated base branch in
    /// (issue #55). De-emphasizes the noisy back-merge line. Default off.
    pub mute_base_merges: bool,
    /// Collapse merge-commit messages to a bare merge glyph — no message text
    /// (issue #59). A stronger form of `mute_merges`; keeps hash/author/date.
    /// Default off.
    pub collapse_merges: bool,
    /// Show round author avatars (pixel mode only).
    pub avatars: bool,
    /// Rewrite a commit that landed a merged GitHub PR — a PR merge commit or
    /// a squash commit — to show "<icon> #<n> <PR title>" instead of the raw
    /// subject (issue #99). Default on.
    pub pr_subjects: bool,
}

impl Default for MetadataColumns {
    fn default() -> Self {
        Self {
            author: true,
            hash: true,
            date: true,
            mute_merges: true,
            mute_base_merges: false,
            collapse_merges: false,
            avatars: false,
            pr_subjects: true,
        }
    }
}

/// A single toggleable display option. `ALL` is also the menu display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataColumn {
    Author,
    Hash,
    Date,
    MuteMerges,
    MuteBaseMerges,
    CollapseMerges,
    Avatars,
    PrSubjects,
}

impl MetadataColumn {
    pub const ALL: [MetadataColumn; 8] = [
        Self::Author,
        Self::Hash,
        Self::Date,
        Self::MuteMerges,
        Self::MuteBaseMerges,
        Self::CollapseMerges,
        Self::Avatars,
        Self::PrSubjects,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Author => "Author",
            Self::Hash => "Hash",
            Self::Date => "Date",
            Self::MuteMerges => "Mute merges",
            Self::MuteBaseMerges => "Mute base-update merges",
            Self::CollapseMerges => "Collapse merge messages",
            Self::Avatars => "Avatars",
            Self::PrSubjects => "PR number & title subjects",
        }
    }
}

impl MetadataColumns {
    pub fn is_visible(&self, col: MetadataColumn) -> bool {
        match col {
            MetadataColumn::Author => self.author,
            MetadataColumn::Hash => self.hash,
            MetadataColumn::Date => self.date,
            MetadataColumn::MuteMerges => self.mute_merges,
            MetadataColumn::MuteBaseMerges => self.mute_base_merges,
            MetadataColumn::CollapseMerges => self.collapse_merges,
            MetadataColumn::Avatars => self.avatars,
            MetadataColumn::PrSubjects => self.pr_subjects,
        }
    }

    pub fn toggle(&mut self, col: MetadataColumn) {
        match col {
            MetadataColumn::Author => self.author = !self.author,
            MetadataColumn::Hash => self.hash = !self.hash,
            MetadataColumn::Date => self.date = !self.date,
            MetadataColumn::MuteMerges => self.mute_merges = !self.mute_merges,
            MetadataColumn::MuteBaseMerges => self.mute_base_merges = !self.mute_base_merges,
            MetadataColumn::CollapseMerges => self.collapse_merges = !self.collapse_merges,
            MetadataColumn::Avatars => self.avatars = !self.avatars,
            MetadataColumn::PrSubjects => self.pr_subjects = !self.pr_subjects,
        }
    }
}

/// Default graph-pane share of the graph/detail split, as a percentage.
pub const DEFAULT_GRAPH_SPLIT_RATIO: u16 = 65;

/// Persistent UI state saved between sessions.
///
/// Field order matters for TOML: scalar values must be emitted before the
/// `[metadata_columns]` table, so keep `metadata_columns` last.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    pub side_panel_layout: bool,
    /// Cap on the graph column width in cells; `None` = uncapped (fit all lanes,
    /// the default). A cap wider than a later graph's needed width is uncapped.
    pub graph_width_cap: Option<usize>,
    /// Graph-pane share of the graph/detail split, as a percentage (clamped
    /// 20–80 when set by dragging the divider).
    pub graph_split_ratio: u16,
    /// Branch tracing: highlight the selected commit's lineage and dim the rest.
    /// On by default; only takes effect on branchy (>2 lane) graphs.
    pub trace_enabled: bool,
    /// Hide remote-only branches (remote refs with no matching local branch)
    /// from the graph. Off by default (remotes shown).
    pub hide_remote_branches: bool,
    /// Soft line-wrapping in the file-diff viewer. Off by default (long lines
    /// truncate and scroll horizontally, the historical behavior).
    pub diff_word_wrap: bool,
    /// Hide branches already merged into the trunk (by merge commit, fast-forward,
    /// or squash) from the graph. Off by default: merged branches are shown but
    /// dimmed, and this toggle removes them entirely.
    pub hide_merged_branches: bool,
    /// Group the files pane by folder (`f` toggles). Off by default: files list
    /// flat with full repo-relative paths, the historical behavior.
    pub files_group_by_folder: bool,
    pub metadata_columns: MetadataColumns,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            side_panel_layout: false,
            graph_width_cap: None,
            graph_split_ratio: DEFAULT_GRAPH_SPLIT_RATIO,
            trace_enabled: true,
            hide_remote_branches: false,
            diff_word_wrap: false,
            hide_merged_branches: false,
            files_group_by_folder: false,
            metadata_columns: MetadataColumns::default(),
        }
    }
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
        // Missing from an older state file → the sensible default, not 0.
        assert_eq!(state.graph_split_ratio, DEFAULT_GRAPH_SPLIT_RATIO);
        assert!(!state.files_group_by_folder);
        assert!(state.metadata_columns.author);
        assert!(state.metadata_columns.hash);
        assert!(state.metadata_columns.date);
        // Avatars default OFF (opt-in via Shift+M), including for an older
        // state file lacking the key.
        assert!(!state.metadata_columns.avatars);
        // PR-landed subjects default ON, including for an older state file
        // lacking the key.
        assert!(state.metadata_columns.pr_subjects);
    }

    #[test]
    fn ui_state_round_trips_metadata_columns() {
        let state = UiState {
            side_panel_layout: true,
            graph_width_cap: Some(8),
            graph_split_ratio: 40,
            trace_enabled: false,
            hide_remote_branches: true,
            diff_word_wrap: false,
            hide_merged_branches: false,
            files_group_by_folder: true,
            metadata_columns: MetadataColumns {
                author: true,
                hash: false,
                date: false,
                mute_merges: false,
                mute_base_merges: true,
                collapse_merges: true,
                avatars: false,
                pr_subjects: false,
            },
        };
        let serialized = toml::to_string(&state).unwrap();
        let restored: UiState = toml::from_str(&serialized).unwrap();
        assert!(restored.side_panel_layout);
        assert_eq!(restored.graph_width_cap, Some(8));
        assert_eq!(restored.graph_split_ratio, 40);
        assert!(!restored.trace_enabled);
        assert!(restored.hide_remote_branches);
        assert!(restored.files_group_by_folder);
        assert!(restored.metadata_columns.author);
        assert!(!restored.metadata_columns.hash);
        assert!(!restored.metadata_columns.date);
        assert!(!restored.metadata_columns.mute_merges);
        assert!(restored.metadata_columns.mute_base_merges);
        assert!(restored.metadata_columns.collapse_merges);
        assert!(!restored.metadata_columns.avatars);
        assert!(!restored.metadata_columns.pr_subjects);
    }

    #[test]
    fn mute_merges_defaults_on_and_round_trips() {
        // Default is ON; an older state.toml (no mute_merges key) keeps it ON.
        assert!(MetadataColumns::default().mute_merges);
        let older: UiState =
            toml::from_str("[metadata_columns]\nauthor = true\nhash = true\ndate = true\n").unwrap();
        assert!(older.metadata_columns.mute_merges, "missing key defaults ON");

        let mut cols = MetadataColumns::default();
        cols.toggle(MetadataColumn::MuteMerges);
        assert!(!cols.mute_merges);
        assert!(!cols.is_visible(MetadataColumn::MuteMerges));
    }

    #[test]
    fn pr_subjects_defaults_on_and_round_trips() {
        // Default is ON; an older state.toml (no pr_subjects key) keeps it ON.
        assert!(MetadataColumns::default().pr_subjects);
        let older: UiState =
            toml::from_str("[metadata_columns]\nauthor = true\nhash = true\ndate = true\n").unwrap();
        assert!(older.metadata_columns.pr_subjects, "missing key defaults ON");

        let mut cols = MetadataColumns::default();
        cols.toggle(MetadataColumn::PrSubjects);
        assert!(!cols.pr_subjects);
        assert!(!cols.is_visible(MetadataColumn::PrSubjects));
    }

    #[test]
    fn hide_remote_branches_defaults_off_and_round_trips() {
        // Remotes are shown by default, including for an older state.toml that
        // predates the key.
        assert!(!UiState::default().hide_remote_branches);
        let older: UiState = toml::from_str("side_panel_layout = true").unwrap();
        assert!(!older.hide_remote_branches, "missing key defaults to shown");

        // Once set, the preference survives a save/load round-trip.
        let hidden = UiState {
            hide_remote_branches: true,
            ..UiState::default()
        };
        let restored: UiState = toml::from_str(&toml::to_string(&hidden).unwrap()).unwrap();
        assert!(restored.hide_remote_branches);
    }

    #[test]
    fn hide_merged_branches_defaults_off_and_round_trips() {
        // Merged branches are shown (dimmed) by default, including for an older
        // state.toml that predates the key.
        assert!(!UiState::default().hide_merged_branches);
        let older: UiState = toml::from_str("side_panel_layout = true").unwrap();
        assert!(!older.hide_merged_branches, "missing key defaults to shown");

        let hidden = UiState {
            hide_merged_branches: true,
            ..UiState::default()
        };
        let restored: UiState = toml::from_str(&toml::to_string(&hidden).unwrap()).unwrap();
        assert!(restored.hide_merged_branches);
    }

    #[test]
    fn diff_word_wrap_defaults_off_and_round_trips() {
        // Wrap is off by default, including for an older state.toml without the key.
        assert!(!UiState::default().diff_word_wrap);
        let older: UiState = toml::from_str("side_panel_layout = true").unwrap();
        assert!(!older.diff_word_wrap, "missing key defaults to off");

        let wrapped = UiState {
            diff_word_wrap: true,
            ..UiState::default()
        };
        let restored: UiState = toml::from_str(&toml::to_string(&wrapped).unwrap()).unwrap();
        assert!(restored.diff_word_wrap);
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

    // ── Config::save round-trip (toml_edit) ─────────────────────────

    #[test]
    fn apply_to_document_preserves_comments_and_unknown_keys() {
        // A config file with comments and a key keifu doesn't model.
        let original = "\
# keifu configuration
[refresh]
# how often to refresh local state
auto_refresh = true
refresh_interval = 10
auto_fetch = true
fetch_interval = 60

[ui]
theme = \"auto\"
graph_renderer = \"auto\"
custom_unknown_key = \"keep me\"
";
        let mut doc = original.parse::<toml_edit::DocumentMut>().unwrap();
        let cfg = Config {
            refresh: RefreshConfig {
                auto_refresh: false,
                refresh_interval: 30,
                auto_fetch: true,
                fetch_interval: 120,
                fast_forward_on_refresh: true,
            },
            ui: UiConfig {
                theme: "dark".to_string(),
                graph_renderer: GraphRenderer::Pixel,
                squash_link_lines: true,
            },
        };
        cfg.apply_to_document(&mut doc);
        let out = doc.to_string();

        // Values updated…
        assert!(out.contains("auto_refresh = false"));
        assert!(out.contains("refresh_interval = 30"));
        assert!(out.contains("fetch_interval = 120"));
        assert!(out.contains("fast_forward_on_refresh = true"));
        assert!(out.contains("theme = \"dark\""));
        assert!(out.contains("graph_renderer = \"pixel\""));
        assert!(out.contains("squash_link_lines = true"));
        // …while comments and unknown keys survive.
        assert!(out.contains("# keifu configuration"));
        assert!(out.contains("# how often to refresh local state"));
        assert!(out.contains("custom_unknown_key = \"keep me\""));

        // And the result re-parses back to the written values.
        let reloaded: Config = toml::from_str(&out).unwrap();
        assert!(!reloaded.refresh.auto_refresh);
        assert_eq!(reloaded.refresh.refresh_interval, 30);
        assert_eq!(reloaded.refresh.fetch_interval, 120);
        assert!(reloaded.refresh.fast_forward_on_refresh);
        assert_eq!(reloaded.ui.theme, "dark");
        assert_eq!(reloaded.ui.graph_renderer, GraphRenderer::Pixel);
        assert!(reloaded.ui.squash_link_lines);
    }

    #[test]
    fn apply_to_document_creates_missing_tables() {
        // An empty document gains both tables and every managed key.
        let mut doc = toml_edit::DocumentMut::new();
        Config::default().apply_to_document(&mut doc);
        let out = doc.to_string();
        // Fresh files use standard section headers, not inline tables.
        assert!(out.contains("[refresh]"), "expected [refresh] header: {out}");
        assert!(out.contains("[ui]"), "expected [ui] header: {out}");
        let reloaded: Config = toml::from_str(&out).unwrap();
        assert!(reloaded.refresh.auto_refresh);
        assert_eq!(reloaded.refresh.refresh_interval, 10);
        assert_eq!(reloaded.ui.theme, "auto");
        assert_eq!(reloaded.ui.graph_renderer, GraphRenderer::Auto);
    }

    #[test]
    fn graph_renderer_as_str_matches_serde_tokens() {
        assert_eq!(GraphRenderer::Auto.as_str(), "auto");
        assert_eq!(GraphRenderer::Unicode.as_str(), "unicode");
        assert_eq!(GraphRenderer::Pixel.as_str(), "pixel");
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
