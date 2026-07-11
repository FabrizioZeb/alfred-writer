use serde::{Deserialize, Serialize};
use serde_json::json;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// Prevents the child `claude` process from allocating/flashing its own console
// window, which would steal OS focus away from both the popup and whatever
// field the user is typing in.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const SYSTEM_PROMPT: &str = "You are a grammar, spelling, and clarity checker embedded in a writing tool. \
Detect the language the input text is written in and reply with corrections in that SAME language — never translate. \
Only flag genuine grammar mistakes, spelling errors, punctuation problems, or clearly awkward/unclear phrasing. \
Do not nitpick stylistic preferences, and do not invent issues in text that is already correct. \
Each 'original' value you return MUST be an exact, verbatim substring copied from the input text (same characters, same case, same whitespace) so it can be located programmatically — do not paraphrase it. \
Keep 'suggestion' minimal: replace only what's needed to fix the issue, preserving the surrounding style and tone. \
Keep 'explanation' to a single short sentence. \
If the text has no issues, return an empty issues array. \
Respond with nothing but the JSON — no commentary, no tool calls.";

const CHECK_TIMEOUT: Duration = Duration::from_secs(45);

/// A single flagged grammar/style problem, as returned by the model.
///
/// `original` must be an exact substring of the text that was checked — it's how
/// `automation/replace.rs` locates the range to select and replace.
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
struct IssueList {
    #[serde(default)]
    issues: Vec<Issue>,
}

/// Outcome of a [`check_grammar`] call: either the list of flagged issues (which may be
/// empty when the text has no problems), or a human-readable error to show the user.
pub enum CheckResult {
    Issues(Vec<Issue>),
    Error(String),
}

/// JSON Schema passed to `claude -p --json-schema`, constraining the model's response to
/// `{ "issues": [{ original, suggestion, explanation }, ...] }`.
fn issue_schema() -> serde_json::Value {
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

/// Runs `claude -p` as a subprocess to get a grammar check, reusing the user's existing
/// Claude Code login instead of requiring a separate Anthropic API key.
///
/// Parameters:
/// - `model`: the `claude` model id to pass via `--model` (see [`crate::config::MODELS`]).
/// - `text`: the field's current text to check; sent verbatim as the prompt.
///
/// Returns:
/// [`CheckResult::Issues`] (possibly empty) on a successful response, or
/// [`CheckResult::Error`] with a user-facing message if the subprocess couldn't be spawned,
/// timed out (45s), or returned output that couldn't be parsed.
pub fn check_grammar(model: &str, text: &str) -> CheckResult {
    let schema = serde_json::to_string(&issue_schema()).unwrap();

    let mut child = match Command::new("claude")
        .args([
            "-p",
            "--output-format",
            "json",
            "--model",
            model,
            "--json-schema",
            &schema,
            "--system-prompt",
            SYSTEM_PROMPT,
            "--permission-mode",
            "dontAsk",
            text,
        ])
        // Run outside any project directory so no local CLAUDE.md / project context
        // gets pulled into a task that doesn't need it.
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return CheckResult::Error(format!(
                "Couldn't run the `claude` CLI ({e}). Make sure Claude Code is installed and `claude` is on your PATH."
            ));
        }
    };

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if start.elapsed() >= CHECK_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return CheckResult::Error("Claude took too long to respond.".to_string());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return CheckResult::Error(format!("Failed to wait on claude process: {e}")),
        }
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return CheckResult::Error(format!("Failed to read claude output: {e}")),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    parse_response(&stdout, &stderr)
}

/// Interprets the JSON envelope `claude -p --output-format json` prints on stdout. Split
/// out from `check_grammar` (which spawns a real subprocess) so this — the part that
/// actually decides what an issue list, an error, or malformed output means — is testable
/// without shelling out.
fn parse_response(stdout: &str, stderr: &str) -> CheckResult {
    let value: serde_json::Value = match serde_json::from_str(stdout.trim()) {
        Ok(v) => v,
        Err(_) => {
            let stderr = stderr.trim();
            let msg = if !stderr.is_empty() {
                stderr.to_string()
            } else {
                "Unexpected output from the claude CLI.".to_string()
            };
            return CheckResult::Error(msg);
        }
    };

    let is_error = value.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
    if is_error {
        let msg = value
            .get("result")
            .and_then(|r| r.as_str())
            .unwrap_or("claude reported an error")
            .to_string();
        return CheckResult::Error(msg);
    }

    if let Some(structured) = value.get("structured_output") {
        if let Ok(list) = serde_json::from_value::<IssueList>(structured.clone()) {
            return CheckResult::Issues(list.issues);
        }
    }

    if let Some(result_str) = value.get("result").and_then(|r| r.as_str()) {
        if let Ok(list) = serde_json::from_str::<IssueList>(result_str) {
            return CheckResult::Issues(list.issues);
        }
    }

    CheckResult::Issues(vec![])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issues_or_panic(result: CheckResult) -> Vec<Issue> {
        match result {
            CheckResult::Issues(issues) => issues,
            CheckResult::Error(e) => panic!("expected Issues, got Error({e})"),
        }
    }

    fn error_or_panic(result: CheckResult) -> String {
        match result {
            CheckResult::Error(e) => e,
            CheckResult::Issues(i) => panic!("expected Error, got Issues({i:?})"),
        }
    }

    #[test]
    fn parses_structured_output_issues() {
        let stdout = r#"{
            "is_error": false,
            "structured_output": {
                "issues": [
                    {"original": "teh", "suggestion": "the", "explanation": "typo"}
                ]
            }
        }"#;
        let issues = issues_or_panic(parse_response(stdout, ""));
        assert_eq!(
            issues,
            vec![Issue {
                original: "teh".to_string(),
                suggestion: "the".to_string(),
                explanation: "typo".to_string(),
            }]
        );
    }

    #[test]
    fn empty_issues_array_means_no_problems() {
        let stdout = r#"{"is_error": false, "structured_output": {"issues": []}}"#;
        assert!(issues_or_panic(parse_response(stdout, "")).is_empty());
    }

    #[test]
    fn is_error_flag_surfaces_the_result_message() {
        let stdout = r#"{"is_error": true, "result": "rate limited"}"#;
        assert_eq!(error_or_panic(parse_response(stdout, "")), "rate limited");
    }

    #[test]
    fn is_error_flag_without_result_uses_generic_message() {
        let stdout = r#"{"is_error": true}"#;
        assert_eq!(error_or_panic(parse_response(stdout, "")), "claude reported an error");
    }

    #[test]
    fn falls_back_to_parsing_result_string_as_json_when_no_structured_output() {
        let stdout = r#"{"is_error": false, "result": "{\"issues\": [{\"original\": \"a\", \"suggestion\": \"b\", \"explanation\": \"c\"}]}"}"#;
        let issues = issues_or_panic(parse_response(stdout, ""));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].original, "a");
    }

    #[test]
    fn missing_structured_output_and_unparseable_result_yields_empty_issues() {
        let stdout = r#"{"is_error": false, "result": "not json"}"#;
        assert!(issues_or_panic(parse_response(stdout, "")).is_empty());
    }

    #[test]
    fn malformed_stdout_with_stderr_surfaces_stderr() {
        let result = parse_response("not json at all", "claude: command failed\n");
        assert_eq!(error_or_panic(result), "claude: command failed");
    }

    #[test]
    fn malformed_stdout_without_stderr_uses_generic_message() {
        let result = parse_response("", "");
        assert_eq!(error_or_panic(result), "Unexpected output from the claude CLI.");
    }

    #[test]
    fn issue_schema_requires_the_expected_fields() {
        let schema = issue_schema();
        let item_props = &schema["properties"]["issues"]["items"]["properties"];
        assert!(item_props.get("original").is_some());
        assert!(item_props.get("suggestion").is_some());
        assert!(item_props.get("explanation").is_some());
        let required = schema["properties"]["issues"]["items"]["required"].as_array().unwrap();
        for field in ["original", "suggestion", "explanation"] {
            assert!(required.iter().any(|v| v == field), "{field} should be required");
        }
    }
}
