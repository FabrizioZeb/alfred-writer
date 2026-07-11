//! Local model server provider — Ollama, LM Studio, or anything else exposing an
//! OpenAI-compatible `/chat/completions` endpoint on localhost. No API key required.
//! Structured output isn't reliably enforceable across arbitrary local runtimes, so
//! rather than relying on a schema-constrained response format, the schema is embedded
//! as instructions in the system prompt and we ask for `json_object` mode as a
//! best-effort nudge toward valid JSON.

use super::{CancellationToken, IssueList, LlmProvider, ProviderCapabilities, ProviderResponse, PromptRequest, RateLimitInfo};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalConfig {
    /// Base URL of an OpenAI-compatible endpoint, e.g. Ollama's
    /// `http://localhost:11434/v1` or LM Studio's `http://localhost:1234/v1`.
    pub base_url: String,
    pub model: String,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434/v1".to_string(),
            model: "llama3.1".to_string(),
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

        let body = serde_json::json!({
            "model": request.model,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": request.text },
            ],
            "response_format": { "type": "json_object" },
        });

        let result = ureq::post(&url)
            .set("content-type", "application/json")
            .timeout(REQUEST_TIMEOUT)
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
    match serde_json::from_str::<IssueList>(content) {
        Ok(list) => ProviderResponse::Issues(list.issues),
        Err(e) => ProviderResponse::Error(format!("Couldn't parse the local model's JSON payload: {e}")),
    }
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
    fn no_api_key_is_ever_required() {
        assert!(!LocalProvider::new(LocalConfig::default()).capabilities().requires_api_key);
    }
}
