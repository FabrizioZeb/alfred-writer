//! Local model server provider — Ollama, LM Studio, or anything else exposing an
//! OpenAI-compatible `/chat/completions` endpoint on localhost. No API key required.
//!
//! No `response_format` is sent at all; the schema is embedded as instructions in the
//! system prompt and the reply is cleaned up before parsing. Both constrained modes
//! were tried against a real LM Studio (0.3.x, Qwen3.5-9B) and failed in different
//! ways: `json_object` is rejected outright ("'response_format.type' must be
//! 'json_schema' or 'text'"), and `json_schema` hangs indefinitely on thinking models
//! (grammar-constrained sampling deadlocks against the reasoning channel — a 5-minute
//! wall-clock test never returned). Plain prompting returned clean, parseable JSON.

use super::{CancellationToken, IssueList, LlmProvider, ProviderCapabilities, ProviderResponse, PromptRequest, RateLimitInfo};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Generous because local models can be slow in ways cloud APIs never are: thinking
/// models (e.g. Qwen3.5) spend 1k+ mandatory reasoning tokens per reply, and a first
/// request may also pay a cold model load. Measured: ~80s for one grammar check on
/// Qwen3.5-9B Q8 at ~16 tok/s. Overridable per-config via `timeout_secs`.
fn default_timeout_secs() -> u64 {
    180
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalConfig {
    /// Base URL of an OpenAI-compatible endpoint, e.g. Ollama's
    /// `http://localhost:11434/v1` or LM Studio's `http://localhost:1234/v1`.
    pub base_url: String,
    pub model: String,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434/v1".to_string(),
            model: "llama3.1".to_string(),
            timeout_secs: default_timeout_secs(),
        }
    }
}

pub struct LocalProvider {
    config: LocalConfig,
}

impl LocalProvider {
    pub fn new(config: LocalConfig) -> Self {
        Self { config }
    }
}

impl LlmProvider for LocalProvider {
    fn id(&self) -> &'static str {
        "local"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            requires_api_key: false,
            structured_json: false,
            cancellable_mid_flight: false,
            reports_rate_limits: false,
        }
    }

    fn execute(&self, request: &PromptRequest, cancel: &CancellationToken) -> ProviderResponse {
        if cancel.is_cancelled() {
            return ProviderResponse::Cancelled;
        }

        let url = format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'));
        let system_prompt = format!(
            "{}\n\nRespond with JSON matching exactly this schema:\n{}",
            request.system_prompt, request.schema
        );

        // Deliberately no `response_format` — see module docs for why both constrained
        // modes fail against real local runtimes.
        let body = serde_json::json!({
            "model": request.model,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": request.text },
            ],
        });

        let result = ureq::post(&url)
            .set("content-type", "application/json")
            .timeout(Duration::from_secs(self.config.timeout_secs))
            .send_json(body);

        match result {
            Ok(resp) => match resp.into_json::<serde_json::Value>() {
                Ok(value) => extract_issues(&value),
                Err(e) => ProviderResponse::Error(format!("Couldn't parse local model response: {e}")),
            },
            Err(ureq::Error::Status(code, resp)) => {
                let message = resp.into_string().unwrap_or_default();
                ProviderResponse::Error(format!("Local model server error ({code}): {message}"))
            }
            Err(e) => ProviderResponse::Error(format!(
                "Couldn't reach the local model server at {}: {e}",
                self.config.base_url
            )),
        }
    }

    fn rate_limit(&self) -> Option<RateLimitInfo> {
        None
    }
}

fn extract_issues(value: &serde_json::Value) -> ProviderResponse {
    let Some(content) = value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
    else {
        return ProviderResponse::Error("Local model response had no message content.".to_string());
    };
    match serde_json::from_str::<IssueList>(clean_content(content)) {
        Ok(list) => ProviderResponse::Issues(list.issues),
        Err(e) => ProviderResponse::Error(format!("Couldn't parse the local model's JSON payload: {e}")),
    }
}

/// Trims the non-JSON wrapping local models are prone to: a leading
/// `<think>…</think>` block (thinking models on runtimes that inline reasoning into
/// `content`, e.g. Ollama — LM Studio splits it out) and Markdown code fences.
fn clean_content(content: &str) -> &str {
    let mut s = content.trim();
    if let Some(rest) = s.strip_prefix("<think>") {
        if let Some((_, after)) = rest.split_once("</think>") {
            s = after.trim();
        }
    }
    if let Some(rest) = s.strip_prefix("```") {
        // Opening fence may carry a language tag ("```json"); drop that first line,
        // then the closing fence.
        let rest = rest.split_once('\n').map(|(_, r)| r).unwrap_or(rest);
        s = rest.rsplit_once("```").map(|(body, _)| body).unwrap_or(rest).trim();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_issues_from_message_content() {
        let value = serde_json::json!({
            "choices": [{
                "message": { "content": "{\"issues\": [{\"original\": \"teh\", \"suggestion\": \"the\", \"explanation\": \"typo\"}]}" }
            }]
        });
        match extract_issues(&value) {
            ProviderResponse::Issues(issues) => assert_eq!(issues[0].original, "teh"),
            other => panic!("expected Issues, got {other:?}"),
        }
    }

    #[test]
    fn default_base_url_points_at_ollama() {
        assert_eq!(LocalConfig::default().base_url, "http://localhost:11434/v1");
    }

    #[test]
    fn cleans_think_blocks_and_code_fences() {
        assert_eq!(clean_content("<think>hmm, let me see</think>\n{\"issues\":[]}"), "{\"issues\":[]}");
        assert_eq!(clean_content("```json\n{\"issues\":[]}\n```"), "{\"issues\":[]}");
        assert_eq!(clean_content("<think>x</think>\n```json\n{\"issues\":[]}\n```"), "{\"issues\":[]}");
        assert_eq!(clean_content("{\"issues\":[]}"), "{\"issues\":[]}");
    }

    #[test]
    fn missing_timeout_in_saved_config_falls_back_to_default() {
        // Configs saved before timeout_secs existed must still load.
        let c: LocalConfig = serde_json::from_str(r#"{"base_url":"http://x/v1","model":"m"}"#).unwrap();
        assert_eq!(c.timeout_secs, 180);
    }

    #[test]
    fn no_api_key_is_ever_required() {
        assert!(!LocalProvider::new(LocalConfig::default()).capabilities().requires_api_key);
    }
}
