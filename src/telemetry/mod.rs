//! Local, file-based metrics: one JSONL line per grammar check, appended to
//! `%LOCALAPPDATA%\local\AlfredWriter\data\telemetry\checks.jsonl` (the `local` segment
//! is the organization in our `ProjectDirs` triple — easy to misread as spurious, and
//! pointing anything at the org-less path silently watches an empty dir). Nothing ever leaves the
//! machine — "telemetry" here means *your own* latency/cache numbers, written so the
//! docker/ log-viewer stack (Grafana + Loki tailing this file) or any `jq` one-liner can
//! answer "why did that check feel slow?" with data instead of guesswork.
//!
//! Recording is strictly best-effort: a full disk or unresolvable data dir silently
//! drops the record. A metrics write must never break, block, or panic a check.

use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Set by integration tests to redirect telemetry writes to a scratch directory, same
/// pattern as `ALFRED_WRITER_CONFIG_DIR` in `config.rs`.
pub const TELEMETRY_DIR_OVERRIDE_ENV: &str = "ALFRED_WRITER_TELEMETRY_DIR";

/// Which of the check pipeline's exits produced this record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePath {
    /// The exact whole-field text was already cached — answered instantly.
    FullHit,
    /// Every paragraph was individually cached — merged, no provider call.
    SegmentsHit,
    /// At least one paragraph was new — the provider was called with just those.
    Provider,
}

/// How the check ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Issues,
    Clean,
    Error,
    Cancelled,
    Stale,
}

/// One grammar check, start to finish. Timestamps are epoch milliseconds (`ts_ms`) so
/// log shippers can promote them to the entry timestamp without date parsing.
#[derive(Debug, Serialize)]
pub struct CheckRecord {
    pub ts_ms: u64,
    /// Provider id (`"local"`, `"external-command"`) — not the model, the backend kind.
    pub provider: String,
    pub model: String,
    pub text_chars: usize,
    pub segments_total: usize,
    pub segments_cached: usize,
    pub segments_sent: usize,
    pub cache_path: CachePath,
    /// Wall-clock milliseconds spent inside `LlmProvider::execute`. `None` when no
    /// provider call happened (cache hits) or the result arrived after cancellation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_ms: Option<u64>,
    pub outcome: Outcome,
    pub issues: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Appends `record` as one JSON line. Best-effort by contract: all failures (no data
/// dir, locked file, disk full, serialization bug) are swallowed after an eprintln.
pub fn record(record: &CheckRecord) {
    let Some(path) = telemetry_file() else { return };
    let Ok(line) = serde_json::to_string(record) else { return };
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(e) = result {
        eprintln!("[alfred-writer] telemetry write failed: {e}");
    }
}

/// Resolves (and creates) the telemetry directory; `None` if it can't be resolved or
/// created — callers just skip recording in that case.
pub fn telemetry_dir() -> Option<PathBuf> {
    let dir = if let Ok(dir) = std::env::var(TELEMETRY_DIR_OVERRIDE_ENV) {
        PathBuf::from(dir)
    } else {
        directories::ProjectDirs::from("dev", "local", "AlfredWriter")?
            .data_local_dir()
            .join("telemetry")
    };
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn telemetry_file() -> Option<PathBuf> {
    Some(telemetry_dir()?.join("checks.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(outcome: Outcome, cache_path: CachePath) -> CheckRecord {
        CheckRecord {
            ts_ms: 1_700_000_000_000,
            provider: "external-command".to_string(),
            model: "claude-haiku-4-5".to_string(),
            text_chars: 120,
            segments_total: 3,
            segments_cached: 2,
            segments_sent: 1,
            cache_path,
            provider_ms: Some(2237),
            outcome,
            issues: 2,
            error: None,
        }
    }

    #[test]
    fn serializes_to_flat_snake_case_json() {
        let json = serde_json::to_string(&sample(Outcome::Issues, CachePath::Provider)).unwrap();
        assert!(json.contains("\"cache_path\":\"provider\""));
        assert!(json.contains("\"outcome\":\"issues\""));
        assert!(json.contains("\"ts_ms\":1700000000000"));
        assert!(!json.contains("error"), "None fields should be omitted, not null");
    }

    #[test]
    fn record_appends_one_line_per_call() {
        let dir = std::env::temp_dir().join(format!("aw-telemetry-test-{}", std::process::id()));
        // Env vars are process-global; this test is the only writer of this one, and it
        // points at a unique per-process scratch dir.
        std::env::set_var(TELEMETRY_DIR_OVERRIDE_ENV, &dir);
        record(&sample(Outcome::Issues, CachePath::Provider));
        record(&sample(Outcome::Clean, CachePath::FullHit));
        let contents = std::fs::read_to_string(dir.join("checks.jsonl")).unwrap();
        std::env::remove_var(TELEMETRY_DIR_OVERRIDE_ENV);
        let _ = std::fs::remove_dir_all(&dir);

        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            serde_json::from_str::<serde_json::Value>(line).expect("every line is standalone JSON");
        }
    }
}
