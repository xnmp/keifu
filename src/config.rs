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
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
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

/// Persistent UI state saved between sessions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    pub side_panel_layout: bool,
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
