use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const MODELS: [&str; 3] = ["claude-opus-4-8", "claude-sonnet-5", "claude-haiku-4-5"];
// Each check shells out to a fresh `claude -p` process, so a fast/cheap model matters
// more here than in the browser extension — checks fire on every pause in typing.
pub const DEFAULT_MODEL: &str = "claude-haiku-4-5";

/// User-editable settings, persisted as JSON at `%APPDATA%\AlfredWriter\config.json`.
/// Any field missing from the file on disk (e.g. after adding a new setting) falls back
/// to its `#[serde(default = ...)]` value rather than failing to load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// `claude` model id to use for checks; one of [`MODELS`].
    #[serde(default = "default_model")]
    pub model: String,
    /// Global on/off toggle for checking, mirrored in the tray menu.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_model() -> String {
    DEFAULT_MODEL.to_string()
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: default_model(),
            enabled: true,
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
    /// needed.
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

    #[test]
    fn default_config_is_enabled_with_default_model() {
        let c = Config::default();
        assert_eq!(c.model, DEFAULT_MODEL);
        assert!(c.enabled);
        assert!(MODELS.contains(&c.model.as_str()));
    }

    #[test]
    fn round_trips_through_json() {
        let c = Config {
            model: "claude-opus-4-8".to_string(),
            enabled: false,
        };
        let bytes = serde_json::to_vec(&c).unwrap();
        let back: Config = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.model, c.model);
        assert_eq!(back.enabled, c.enabled);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c.model, DEFAULT_MODEL);
        assert!(c.enabled);
    }

    #[test]
    fn partial_json_only_fills_in_the_missing_field() {
        let c: Config = serde_json::from_str(r#"{"enabled": false}"#).unwrap();
        assert_eq!(c.model, DEFAULT_MODEL);
        assert!(!c.enabled);
    }

    #[test]
    fn load_falls_back_to_default_when_bytes_are_malformed() {
        // Mirrors what Config::load() does with unparseable disk contents: never panic
        // or propagate an error, just start fresh.
        let parsed: Config = serde_json::from_slice(b"not json").unwrap_or_default();
        assert_eq!(parsed.model, DEFAULT_MODEL);
        assert!(parsed.enabled);
    }
}
