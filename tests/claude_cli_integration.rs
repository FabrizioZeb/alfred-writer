//! End-to-end check against the real `claude` CLI. Ignored by default — it costs real
//! time/money and requires Claude Code installed and logged in on the machine running the
//! test. Run explicitly with:
//!   cargo test --test claude_cli_integration -- --ignored

use alfred_writer::claude::{check_grammar, CheckResult};
use alfred_writer::config::DEFAULT_MODEL;

#[test]
#[ignore]
fn real_claude_cli_flags_an_obvious_typo() {
    let result = check_grammar(DEFAULT_MODEL, "I has went to the store yesterday.");
    match result {
        CheckResult::Issues(issues) => {
            assert!(!issues.is_empty(), "expected at least one grammar issue to be flagged");
        }
        CheckResult::Error(e) => panic!("claude CLI call failed: {e}"),
    }
}
