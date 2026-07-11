//! Exercises Config::save()/load() against the real filesystem (serde_json + std::fs +
//! the `directories` crate glue), rather than just Config's Deserialize behavior in
//! isolation (already covered by unit tests in src/config.rs).
//!
//! Uses the ALFRED_WRITER_CONFIG_DIR override so the suite never touches a developer's
//! real %APPDATA%\AlfredWriter\config.json. Both scenarios live in one #[test] fn because
//! std::env::set_var is process-global — running them as separate tests would race under
//! cargo's default parallel test execution.

use alfred_writer::config::Config;
use alfred_writer::providers::{LocalConfig, ProviderConfig};

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("alfred-writer-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn save_load_and_corrupted_file_recovery_against_the_real_filesystem() {
    let dir = scratch_dir("config");
    std::env::set_var("ALFRED_WRITER_CONFIG_DIR", &dir);

    // No config file yet: load() should hand back defaults, not error or panic.
    let loaded = Config::load();
    assert!(loaded.enabled);
    assert!(matches!(loaded.provider, ProviderConfig::ExternalCommand(_)));

    // Save a non-default config, then confirm a fresh load() sees exactly that.
    let to_save = Config {
        provider: ProviderConfig::Local(LocalConfig {
            base_url: "http://localhost:11434/v1".to_string(),
            model: "llama3.1".to_string(),
        }),
        enabled: false,
    };
    to_save.save().expect("save should succeed against a writable scratch dir");

    let reloaded = Config::load();
    assert_eq!(reloaded, to_save);
    assert!(dir.join("config.json").exists());

    // Corrupt the file on disk directly, then confirm load() recovers to defaults
    // instead of panicking or propagating the parse error.
    std::fs::write(dir.join("config.json"), b"{ this is not valid json").unwrap();
    let after_corruption = Config::load();
    assert!(after_corruption.enabled);
    assert!(matches!(after_corruption.provider, ProviderConfig::ExternalCommand(_)));

    std::env::remove_var("ALFRED_WRITER_CONFIG_DIR");
    let _ = std::fs::remove_dir_all(&dir);
}
