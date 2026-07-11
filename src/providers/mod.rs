//! Pluggable LLM backends behind one interface, so `automation.rs` never needs to know
//! whether a check is answered by a local model server or an arbitrary external command.
//! `automation.rs` only ever talks to `dyn LlmProvider`; adding a new backend means adding
//! a new submodule + a new [`ProviderConfig`] arm, not touching the orchestration loop.
//!
//! Deliberately no commercial cloud-API backends: everything here either talks to a
//! model server running on the user's own machine, or hands off entirely to a
//! user-configured external command (which may itself call out to a cloud API, but that's
//! the external tool's decision and the external tool's credentials, not this app's).

mod external_command;
mod local;

pub use external_command::{ExternalCommandConfig, InputMode};
pub use local::LocalConfig;

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// One issue flagged in the checked text, common to every provider regardless of how it
/// got the answer out of the underlying model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    /// Exact, verbatim substring of the checked text that has an issue.
    pub original: String,
    /// Minimal replacement text that fixes the issue.
    pub suggestion: String,
    /// One short sentence explaining the issue, shown in the popup.
    #[serde(default)]
    pub explanation: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct IssueList {
    #[serde(default)]
    pub(crate) issues: Vec<Issue>,
}

/// A single grammar-check round trip to hand to a provider. The same request shape is
/// sent regardless of backend; each provider adapts it to its own wire format.
pub struct PromptRequest {
    pub model: String,
    pub system_prompt: String,
    pub schema: serde_json::Value,
    pub text: String,
}

/// Outcome of [`LlmProvider::execute`].
#[derive(Debug)]
pub enum ProviderResponse {
    /// Issues found (possibly empty — that means the text was clean).
    Issues(Vec<Issue>),
    /// A user-facing error message (auth failure, network error, malformed response, ...).
    Error(String),
    /// The request was abandoned because of a cancellation request. Never shown to the
    /// user as an error — the caller just treats it like there's nothing to report.
    Cancelled,
}

/// What a provider supports, so callers can adapt without knowing the concrete backend.
pub struct ProviderCapabilities {
    /// Whether this backend needs a user-supplied API key to function.
    pub requires_api_key: bool,
    /// Whether the backend can be asked to constrain its output to a JSON Schema
    /// natively, rather than us just hoping a prompted request produces valid JSON.
    pub structured_json: bool,
    /// Whether a cancellation request can actually interrupt in-flight work (e.g. killing
    /// a child process) rather than only being honored before the request is sent. HTTP
    /// backends here are pre-flight-only: once a request is on the wire we let it finish
    /// and simply discard the result.
    pub cancellable_mid_flight: bool,
    /// Whether [`LlmProvider::rate_limit`] can ever return `Some`.
    pub reports_rate_limits: bool,
}

/// Best-known rate-limit state as of the last completed call.
#[derive(Debug, Clone, Default)]
pub struct RateLimitInfo {
    pub requests_remaining: Option<u64>,
    pub requests_limit: Option<u64>,
    pub tokens_remaining: Option<u64>,
    pub tokens_limit: Option<u64>,
    pub retry_after: Option<Duration>,
}

/// Cooperative cancellation flag: the caller holds a [`CancellationHandle`] and calls
/// `.cancel()`; the provider is handed a cloneable [`CancellationToken`] and polls
/// `.is_cancelled()` wherever it's able to act on it.
#[derive(Clone)]
pub struct CancellationToken(Arc<AtomicBool>);

pub struct CancellationHandle(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> (CancellationToken, CancellationHandle) {
        let flag = Arc::new(AtomicBool::new(false));
        (CancellationToken(flag.clone()), CancellationHandle(flag))
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl CancellationHandle {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Common interface every backend (a local model server, or an arbitrary external
/// command) implements.
pub trait LlmProvider: Send + Sync {
    /// Stable machine id, e.g. `"local"`, `"external-command"`.
    fn id(&self) -> &'static str;

    /// What this provider supports (capability discovery), so callers can adapt instead
    /// of assuming every backend behaves the same way.
    fn capabilities(&self) -> ProviderCapabilities;

    /// Runs one prompt-and-parse round trip against `request.text`, checking `cancel`
    /// wherever this backend is able to act on it (see
    /// [`ProviderCapabilities::cancellable_mid_flight`]).
    ///
    /// Returns: [`ProviderResponse::Issues`] (possibly empty) on success,
    /// [`ProviderResponse::Error`] with a user-facing message on failure, or
    /// [`ProviderResponse::Cancelled`] if `cancel` was observed.
    fn execute(&self, request: &PromptRequest, cancel: &CancellationToken) -> ProviderResponse;

    /// Best-known rate-limit snapshot from the most recently completed call, if this
    /// backend reports one. `None` for providers that don't expose rate-limit metadata
    /// (both built-in providers today: a local model server typically has no quota to
    /// report, and the external-command provider's auth/quota is entirely the external
    /// tool's own concern, not something it reports back to us).
    fn rate_limit(&self) -> Option<RateLimitInfo>;
}

/// System prompt shared by every provider.
pub fn default_system_prompt() -> &'static str {
    "You are a grammar, spelling, and clarity checker embedded in a writing tool. \
Detect the language the input text is written in and reply with corrections in that SAME language — never translate. \
Only flag genuine grammar mistakes, spelling errors, punctuation problems, or clearly awkward/unclear phrasing. \
Do not nitpick stylistic preferences, and do not invent issues in text that is already correct. \
Each 'original' value you return MUST be an exact, verbatim substring copied from the input text (same characters, same case, same whitespace) so it can be located programmatically — do not paraphrase it. \
Keep 'suggestion' minimal: replace only what's needed to fix the issue, preserving the surrounding style and tone. \
Keep 'explanation' to a single short sentence. \
If the text has no issues, return an empty issues array. \
Respond with nothing but the JSON — no commentary, no tool calls."
}

/// JSON Schema every provider is asked to conform its response to:
/// `{ "issues": [{ original, suggestion, explanation }, ...] }`.
pub fn issue_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "issues": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "original": { "type": "string", "description": "Exact verbatim substring from the input text that has an issue" },
                        "suggestion": { "type": "string", "description": "Minimal corrected replacement for that substring" },
                        "explanation": { "type": "string", "description": "One short sentence explaining the issue" }
                    },
                    "required": ["original", "suggestion", "explanation"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["issues"],
        "additionalProperties": false
    })
}

/// Per-provider settings persisted in [`crate::config::Config`], tagged by `type` in JSON
/// so the saved settings file self-describes which provider each block belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderConfig {
    Local(LocalConfig),
    ExternalCommand(ExternalCommandConfig),
}

impl ProviderConfig {
    /// Zero-setup default: drive the local Claude Code CLI as an external command, since
    /// it needs no API key (reuses the user's existing `claude` login) — see
    /// [`ExternalCommandConfig::claude_code_preset`].
    pub fn default_external_command() -> Self {
        ProviderConfig::ExternalCommand(ExternalCommandConfig::claude_code_preset())
    }

    pub fn model(&self) -> &str {
        match self {
            ProviderConfig::Local(c) => &c.model,
            ProviderConfig::ExternalCommand(c) => &c.model,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ProviderConfig::Local(_) => "Local (Ollama / LM Studio)",
            ProviderConfig::ExternalCommand(_) => "External command",
        }
    }
}

impl Default for ProviderConfig {
    fn default() -> Self {
        ProviderConfig::default_external_command()
    }
}

/// Builds the concrete backend for `config`.
pub fn build(config: &ProviderConfig) -> Box<dyn LlmProvider> {
    match config {
        ProviderConfig::Local(c) => Box::new(local::LocalProvider::new(c.clone())),
        ProviderConfig::ExternalCommand(c) => Box::new(external_command::ExternalCommandProvider::new(c.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_provider_config_is_external_command() {
        assert!(matches!(ProviderConfig::default(), ProviderConfig::ExternalCommand(_)));
    }

    #[test]
    fn model_and_label_delegate_to_the_active_variant() {
        let local = ProviderConfig::Local(LocalConfig {
            base_url: "http://localhost:11434/v1".to_string(),
            model: "llama3.1".to_string(),
        });
        assert_eq!(local.model(), "llama3.1");
        assert_eq!(local.label(), "Local (Ollama / LM Studio)");
    }

    #[test]
    fn issue_schema_requires_the_expected_fields() {
        let schema = issue_schema();
        let required = schema["properties"]["issues"]["items"]["required"].as_array().unwrap();
        for field in ["original", "suggestion", "explanation"] {
            assert!(required.iter().any(|v| v == field), "{field} should be required");
        }
    }
}
