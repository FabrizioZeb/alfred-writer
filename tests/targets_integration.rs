//! Exercises `targets::classify` against the real Windows process APIs (OpenProcess /
//! QueryFullProcessImageNameW) instead of the pure name-matching logic already covered by
//! unit tests in src/targets.rs.

use alfred_writer::targets::{classify, Policy};

#[test]
fn current_test_process_is_not_classified_as_a_terminal() {
    // The test binary itself is some cargo-generated *.exe, never one of the terminal
    // emulator executables, so this should resolve through the real OS lookup to Standard.
    let pid = std::process::id() as i32;
    assert_eq!(classify(pid), Policy::Standard);
}

#[test]
fn nonexistent_pid_falls_back_to_standard() {
    // No process can plausibly have this pid, so OpenProcess fails, the name lookup
    // returns None, and classify() must not panic — it should default to Standard.
    assert_eq!(classify(i32::MAX), Policy::Standard);
}
