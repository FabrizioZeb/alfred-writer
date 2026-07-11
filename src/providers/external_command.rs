//! Runs a user-configured external command as the "LLM provider": we don't call any API
//! directly and don't manage authentication — that's entirely the external tool's own
//! concern, by design (rule of thumb: if it needs a login, it does its own login). The
//! only provider-specific knowledge here is the *mapping*, and that mapping lives
//! entirely in [`ExternalCommandConfig`], not in code: how our fixed
//! (model, system_prompt, schema, text) request becomes the command's argv/stdin, and
//! how its stdout maps back onto our common issue-list shape. Nothing in this file
//! assumes which vendor, if any, is on the other end of the command.

use super::{CancellationToken, IssueList, LlmProvider, ProviderCapabilities, ProviderResponse, PromptRequest, RateLimitInfo};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// Prevents the child process from allocating/flashing its own console window, which
// would steal OS focus away from both the popup and whatever field the user is typing in.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Where the prompt text is delivered to the external command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    /// Written to the child's stdin (with a trailing newline).
    Stdin,
    /// Substituted into `args_template` via the `{prompt}` placeholder; stdin is closed
    /// immediately so a command that happens to read it doesn't block waiting for input.
    Args,
}

/// Settings for driving an arbitrary external command as a provider.
///
/// `args_template` entries may contain the placeholders `{model}`, `{system_prompt}`,
/// `{schema}` (the JSON Schema, minified to one line), and `{prompt}` (only meaningful
/// when `input_mode` is [`InputMode::Args`]) — each is substituted verbatim before the
/// process is spawned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalCommandConfig {
    pub command: String,
    pub args_template: Vec<String>,
    pub input_mode: InputMode,
    pub model: String,
    /// Dot-separated path into the command's parsed stdout JSON where the issue list (or
    /// a JSON *string* containing it) lives, e.g. `"structured_output"`. `None` means the
    /// whole of stdout is the issue list.
    pub response_path: Option<String>,
    /// Dot-separated path to a boolean flag in stdout JSON that marks the response as an
    /// error; when that flag is `true`, `response_path` is instead read as a plain error
    /// string rather than an issue list.
    pub error_path: Option<String>,
    pub timeout_secs: u64,
    /// Extra environment variables set on the child process, on top of whatever it
    /// inherits from Alfred Writer itself. This is how a preset can shave startup-latency
    /// overhead specific to its command (e.g. telling a CLI to skip an update check)
    /// without that vendor-specific knowledge leaking into this file's actual code — see
    /// `claude_code_preset` for a concrete example.
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

impl ExternalCommandConfig {
    /// Zero-setup default: the Claude Code CLI, reusing the user's existing login so no
    /// API key is needed. This preset is the *only* place that knows Claude Code's
    /// specific `--json-schema` / `--output-format json` envelope shape (issues nested
    /// under `structured_output`, errors flagged by `is_error`) — the provider code above
    /// has no idea what it's talking to.
    pub fn claude_code_preset() -> Self {
        Self {
            command: "claude".to_string(),
            args_template: vec![
                "-p".to_string(),
                "--output-format".to_string(),
                "json".to_string(),
                "--model".to_string(),
                "{model}".to_string(),
                "--json-schema".to_string(),
                "{schema}".to_string(),
                "--system-prompt".to_string(),
                "{system_prompt}".to_string(),
                "--permission-mode".to_string(),
                "dontAsk".to_string(),
                "{prompt}".to_string(),
            ],
            input_mode: InputMode::Args,
            response_path: Some("structured_output".to_string()),
            error_path: Some("is_error".to_string()),
            model: "claude-haiku-4-5".to_string(),
            timeout_secs: 45,
            // Every check pays the CLI's full cold-start cost (process creation, config
            // and credential loading, and — normally — a version/update check that phones
            // home before doing anything else). This env var is Claude Code's documented
            // switch for skipping that non-essential network round trip; it directly cuts
            // per-check latency and is harmless if a given CLI version doesn't recognize
            // it. Worth re-verifying against `claude --help`/current docs if a future CLI
            // version renames or removes it.
            env: vec![("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(), "1".to_string())],
        }
    }
}

impl Default for ExternalCommandConfig {
    fn default() -> Self {
        Self::claude_code_preset()
    }
}

pub struct ExternalCommandProvider {
    config: ExternalCommandConfig,
}

impl ExternalCommandProvider {
    pub fn new(config: ExternalCommandConfig) -> Self {
        Self { config }
    }
}

impl LlmProvider for ExternalCommandProvider {
    fn id(&self) -> &'static str {
        "external-command"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            requires_api_key: false,
            // Only as structured as the external command chooses to honor the schema we
            // hand it — we can't enforce anything on a black-box process.
            structured_json: false,
            cancellable_mid_flight: true,
            reports_rate_limits: false,
        }
    }

    fn execute(&self, request: &PromptRequest, cancel: &CancellationToken) -> ProviderResponse {
        if cancel.is_cancelled() {
            return ProviderResponse::Cancelled;
        }

        let args = substitute_args(&self.config.args_template, request);
        let stdin_cfg = if self.config.input_mode == InputMode::Stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        };

        let mut command = Command::new(&self.config.command);
        command
            .args(&args)
            .envs(self.config.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .current_dir(std::env::temp_dir())
            .stdin(stdin_cfg)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW);

        let start = Instant::now();
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                return ProviderResponse::Error(format!(
                    "Couldn't run `{}` ({e}). Check the command is installed and on PATH.",
                    self.config.command
                ));
            }
        };

        if self.config.input_mode == InputMode::Stdin {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = writeln!(stdin, "{}", request.text);
            }
        }

        // Drain stdout/stderr concurrently on their own threads, rather than only reading
        // them after the process exits. Without this, a command that writes more than the
        // OS pipe buffer can hold (commonly ~64KB) blocks on write() as soon as it fills
        // up, while we're over here just polling try_wait() without reading anything —
        // neither side makes progress, and every check against a chatty command "hangs"
        // for the full timeout before erroring out.
        let mut stdout_pipe = child.stdout.take().expect("stdout was requested as piped");
        let mut stderr_pipe = child.stderr.take().expect("stderr was requested as piped");
        let stdout_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout_pipe.read_to_end(&mut buf);
            buf
        });
        let stderr_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut buf);
            buf
        });

        let timeout = Duration::from_secs(self.config.timeout_secs);
        loop {
            if cancel.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return ProviderResponse::Cancelled;
            }
            match child.try_wait() {
                Ok(Some(_status)) => break,
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = stdout_reader.join();
                        let _ = stderr_reader.join();
                        eprintln!(
                            "[alfred-writer] {} timed out after {:.1}s (timeout_secs={})",
                            self.config.command,
                            start.elapsed().as_secs_f32(),
                            self.config.timeout_secs
                        );
                        return ProviderResponse::Error(format!("`{}` took too long to respond.", self.config.command));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return ProviderResponse::Error(format!("Failed to wait on `{}`: {e}", self.config.command));
                }
            }
        }

        // Silent in a normal release run (no attached console) — only visible if stderr
        // is redirected when launching. This is the number to look at when a check
        // "feels slow": it's wall-clock spawn-to-exit for the whole external process, so
        // it separates "the command itself is slow" from "our own gating (debounce/
        // cooldown) made it feel slow", which is a much easier thing to misdiagnose from
        // the UI alone.
        eprintln!(
            "[alfred-writer] {} finished in {:.2}s",
            self.config.command,
            start.elapsed().as_secs_f32()
        );

        let stdout_bytes = stdout_reader.join().unwrap_or_default();
        let stderr_bytes = stderr_reader.join().unwrap_or_default();
        let stdout = String::from_utf8_lossy(&stdout_bytes);
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        parse_output(&stdout, &stderr, &self.config)
    }

    fn rate_limit(&self) -> Option<RateLimitInfo> {
        // Authentication and quota are entirely the external tool's own concern; it has
        // no channel back to us to report rate-limit state.
        None
    }
}

fn substitute_args(template: &[String], request: &PromptRequest) -> Vec<String> {
    let schema_str = serde_json::to_string(&request.schema).unwrap_or_default();
    template
        .iter()
        .map(|arg| {
            arg.replace("{model}", &request.model)
                .replace("{system_prompt}", &request.system_prompt)
                .replace("{schema}", &schema_str)
                .replace("{prompt}", &request.text)
        })
        .collect()
}

fn parse_output(stdout: &str, stderr: &str, config: &ExternalCommandConfig) -> ProviderResponse {
    let value: serde_json::Value = match serde_json::from_str(stdout.trim()) {
        Ok(v) => v,
        Err(_) => {
            let stderr = stderr.trim();
            let msg = if !stderr.is_empty() {
                stderr.to_string()
            } else {
                format!("Unexpected output from `{}`.", config.command)
            };
            return ProviderResponse::Error(msg);
        }
    };

    if let Some(error_path) = &config.error_path {
        if json_path(&value, error_path).and_then(|v| v.as_bool()).unwrap_or(false) {
            let msg = config
                .response_path
                .as_deref()
                .and_then(|p| json_path(&value, p))
                .and_then(|v| v.as_str())
                .unwrap_or("The command reported an error.")
                .to_string();
            return ProviderResponse::Error(msg);
        }
    }

    let target = config
        .response_path
        .as_deref()
        .and_then(|p| json_path(&value, p))
        .unwrap_or(&value);

    let list: Result<IssueList, _> = if let Some(s) = target.as_str() {
        serde_json::from_str(s)
    } else {
        serde_json::from_value(target.clone())
    };

    match list {
        Ok(list) => ProviderResponse::Issues(list.issues),
        Err(_) => ProviderResponse::Error(format!("Couldn't find an issue list in `{}`'s output.", config.command)),
    }
}

fn json_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    path.split('.').try_fold(value, |v, key| v.get(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset() -> ExternalCommandConfig {
        ExternalCommandConfig::claude_code_preset()
    }

    #[test]
    fn parses_claude_code_structured_output_envelope() {
        let stdout = r#"{
            "is_error": false,
            "structured_output": { "issues": [{"original": "teh", "suggestion": "the", "explanation": "typo"}] }
        }"#;
        match parse_output(stdout, "", &preset()) {
            ProviderResponse::Issues(issues) => {
                assert_eq!(issues.len(), 1);
                assert_eq!(issues[0].original, "teh");
            }
            other => panic!("expected Issues, got {other:?}"),
        }
    }

    #[test]
    fn surfaces_error_flag_via_response_path() {
        let stdout = r#"{"is_error": true, "structured_output": "rate limited"}"#;
        match parse_output(stdout, "", &preset()) {
            ProviderResponse::Error(msg) => assert_eq!(msg, "rate limited"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn top_level_issue_list_when_no_response_path_is_configured() {
        let config = ExternalCommandConfig {
            response_path: None,
            error_path: None,
            ..preset()
        };
        let stdout = r#"{"issues": [{"original": "a", "suggestion": "b", "explanation": "c"}]}"#;
        match parse_output(stdout, "", &config) {
            ProviderResponse::Issues(issues) => assert_eq!(issues.len(), 1),
            other => panic!("expected Issues, got {other:?}"),
        }
    }

    #[test]
    fn malformed_stdout_surfaces_stderr() {
        match parse_output("not json", "boom", &preset()) {
            ProviderResponse::Error(msg) => assert_eq!(msg, "boom"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn malformed_stdout_without_stderr_uses_generic_message() {
        match parse_output("not json", "", &preset()) {
            ProviderResponse::Error(msg) => assert!(msg.contains("claude")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn placeholder_substitution_fills_in_the_whole_argv() {
        let request = PromptRequest {
            model: "some-model".to_string(),
            system_prompt: "be nice".to_string(),
            schema: serde_json::json!({"type": "object"}),
            text: "hello world".to_string(),
        };
        let args = substitute_args(&preset().args_template, &request);
        assert!(args.contains(&"some-model".to_string()));
        assert!(args.contains(&"hello world".to_string()));
        assert!(args.contains(&serde_json::to_string(&request.schema).unwrap()));
    }

    #[test]
    fn does_not_deadlock_on_output_larger_than_the_os_pipe_buffer() {
        // Regression test: a command that writes more than the pipe buffer (~64KB on
        // Windows) to stdout used to block forever on write() while we only polled
        // try_wait() without draining the pipe. This generates ~300KB of stdout via cmd,
        // well past that threshold, and asserts we come back in well under the
        // configured timeout instead of stalling until it's killed.
        let long_line = "A".repeat(120);
        let config = ExternalCommandConfig {
            command: "cmd".to_string(),
            args_template: vec![
                "/c".to_string(),
                format!("for /L %i in (1,1,3000) do @echo {long_line}"),
            ],
            input_mode: InputMode::Args,
            response_path: None,
            error_path: None,
            model: "x".to_string(),
            timeout_secs: 20,
            env: vec![],
        };
        let provider = ExternalCommandProvider::new(config);
        let request = PromptRequest {
            model: "x".to_string(),
            system_prompt: "x".to_string(),
            schema: serde_json::json!({}),
            text: "x".to_string(),
        };
        let (token, _handle) = CancellationToken::new();

        let start = Instant::now();
        let result = provider.execute(&request, &token);
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(10),
            "took {elapsed:?}, which suggests it stalled until the timeout instead of draining output as it was produced"
        );
        // The output isn't JSON, so this is expected to be an Error, not a hang or crash.
        assert!(matches!(result, ProviderResponse::Error(_)));
    }

    #[test]
    fn unknown_command_fails_fast_with_a_clear_message() {
        let config = ExternalCommandConfig {
            command: "definitely-not-a-real-command-xyz".to_string(),
            args_template: vec![],
            ..preset()
        };
        let provider = ExternalCommandProvider::new(config);
        let request = PromptRequest {
            model: "x".to_string(),
            system_prompt: "x".to_string(),
            schema: serde_json::json!({}),
            text: "x".to_string(),
        };
        let (token, _handle) = CancellationToken::new();
        match provider.execute(&request, &token) {
            ProviderResponse::Error(msg) => assert!(msg.contains("Couldn't run")),
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
