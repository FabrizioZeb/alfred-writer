//! End-to-end check against a real external command, using the default Claude Code CLI
//! preset. Ignored by default — it costs real time/money and requires Claude Code
//! installed and logged in on the machine running the test. Run explicitly with:
//!   cargo test --test external_command_cli_integration -- --ignored

use alfred_writer::providers::{self, CancellationToken, ExternalCommandConfig, ProviderConfig, ProviderResponse};

#[test]
#[ignore]
fn real_claude_code_cli_flags_an_obvious_typo() {
    let config = ProviderConfig::ExternalCommand(ExternalCommandConfig::claude_code_preset());
    let provider = providers::build(&config);
    let request = providers::PromptRequest {
        model: config.model().to_string(),
        system_prompt: providers::default_system_prompt().to_string(),
        schema: providers::issue_schema(),
        text: "I has went to the store yesterday.".to_string(),
    };
    let (token, _handle) = CancellationToken::new();
    match provider.execute(&request, &token) {
        ProviderResponse::Issues(issues) => {
            assert!(!issues.is_empty(), "expected at least one grammar issue to be flagged");
        }
        ProviderResponse::Error(e) => panic!("external command call failed: {e}"),
        ProviderResponse::Cancelled => panic!("unexpectedly cancelled"),
    }
}
