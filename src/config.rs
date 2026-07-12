use crate::providers::ProviderConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// User-editable settings, persisted as JSON at `%APPDATA%\AlfredWriter\config.json`.
/// Any field missing from the file on disk (e.g. after adding a new setting) falls back
/// to its `#[serde(default = ...)]` value rather than failing to load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Which LLM backend to use for checks, and its settings (API key, model, etc).
    #[serde(default)]
    pub provider: ProviderConfig,
    /// Global on/off toggle for checking, mirrored in the tray menu.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Apps to never check: executable basenames, case-insensitive, `.exe` optional
    /// (e.g. `keepass`, `1Password.exe`). Consulted live on every poll, so edits apply
    /// without restarting — see [`crate::targets::classify`] for matching rules.
    #[serde(default)]
    pub blacklist: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: ProviderConfig::default(),
            enabled: true,
            blacklist: Vec::new(),
        }
    }
}

/// Set by integration tests (and only integration tests) to redirect config storage to a
/// scratch directory instead of the real per-user `%APPDATA%\AlfredWriter`, so running the
/// test suite never touches a developer's actual saved settings.
const CONFIG_DIR_OVERRIDE_ENV: &str = "ALFRED_WRITER_CONFIG_DIR";

fn config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var(CONFIG_DIR_OVERRIDE_ENV) {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).ok()?;
        return Some(dir.join("config.json"));
    }
    let dirs = directories::ProjectDirs::from("dev", "local", "AlfredWriter")?;
    let dir = dirs.config_dir();
    std::fs::create_dir_all(dir).ok()?;
    Some(dir.join("config.json"))
}

impl Config {
    /// Loads settings from disk, falling back to [`Config::default`] if the config
    /// directory can't be resolved, the file doesn't exist yet, or its contents don't
    /// parse as valid JSON. Never fails or panics — always returns a usable `Config`.
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    /// Writes settings to disk as pretty-printed JSON, creating the config directory if
    /// needed. This is the only place API keys are persisted — plaintext, local to this
    /// machine, same trust model as any other desktop app's saved settings.
    ///
    /// Errors:
    /// Returns an error if the config directory can't be resolved or the write fails
    /// (e.g. permissions).
    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::LocalConfig;

    #[test]
    fn default_config_is_enabled_with_the_external_command_provider() {
        let c = Config::default();
        assert!(c.enabled);
        assert!(matches!(c.provider, ProviderConfig::ExternalCommand(_)));
    }

    #[test]
    fn round_trips_through_json() {
        let c = Config {
            provider: ProviderConfig::Local(LocalConfig {
                base_url: "http://localhost:11434/v1".to_string(),
                model: "llama3.1".to_string(),
                timeout_secs: 180,
            }),
            enabled: false,
            blacklist: vec!["keepass.exe".to_string()],
        };
        let bytes = serde_json::to_vec(&c).unwrap();
        let back: Config = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert!(c.enabled);
        assert!(matches!(c.provider, ProviderConfig::ExternalCommand(_)));
    }

    #[test]
    fn partial_json_only_fills_in_the_missing_field() {
        let c: Config = serde_json::from_str(r#"{"enabled": false}"#).unwrap();
        assert!(!c.enabled);
        assert!(matches!(c.provider, ProviderConfig::ExternalCommand(_)));
    }

    #[test]
    fn load_falls_back_to_default_when_bytes_are_malformed() {
        // Mirrors what Config::load() does with unparseable disk contents: never panic
        // or propagate an error, just start fresh.
        let parsed: Config = serde_json::from_slice(b"not json").unwrap_or_default();
        assert!(parsed.enabled);
    }
}
