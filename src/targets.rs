//! Per-application policy: whether/how Alfred Writer should treat the focused field of a
//! given process differently from a plain generic UIA text control.
//!
//! This is the seam new app-specific behavior should hang off of. Today it only has one
//! rule (skip terminals), but it's built as a `classify()` -> `Policy` match rather than a
//! single boolean so a future adapter (e.g. "this is VS Code, prefer the editor pane
//! element over the whole window") has an obvious, low-risk place to add a new arm instead
//! of growing conditionals through `automation.rs`. See SKILLS.md, "Adding an app-specific
//! adapter", before extending this.

use std::collections::HashMap;
use std::sync::Mutex;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Generic UIA text control handling: read/check/apply as normal.
    Standard,
    /// Don't watch or check this field at all. Used for terminal emulators: the visible
    /// "text" there is mostly immutable command history/output, not prose someone is
    /// composing, and only the last line (the live prompt) is actually editable — trying
    /// to select-and-replace inside historical output silently lands wherever the real
    /// input caret is instead (visually: the correction gets appended at the end), which
    /// is worse than not offering a correction at all.
    Skip,
}

/// Executable basenames (case-insensitive, no path) treated as terminal emulators.
const TERMINAL_EXECUTABLES: &[&str] = &[
    "windowsterminal.exe",
    "conhost.exe",
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "wt.exe",
    "conemu.exe",
    "conemu64.exe",
    "mintty.exe",
    "alacritty.exe",
    "wezterm-gui.exe",
    "hyper.exe",
    "terminal.exe",
];

static PROCESS_NAME_CACHE: Mutex<Option<HashMap<i32, String>>> = Mutex::new(None);

/// Looks up the executable behind `pid` and decides how Alfred Writer should treat its
/// currently focused field.
///
/// Parameters:
/// - `pid`: OS process id of the focused UIA element's owning process.
///
/// Returns:
/// [`Policy::Skip`] for known terminal emulators, [`Policy::Standard`] otherwise
/// (including when the process name can't be looked up at all).
pub fn classify(pid: i32) -> Policy {
    match process_executable_name(pid) {
        Some(name) if is_terminal_executable(&name) => Policy::Skip,
        _ => Policy::Standard,
    }
}

/// Pure name-matching logic split out from `classify` so it's testable without a real
/// process handle. `name` is expected pre-lowercased, as `query_process_executable_name`
/// produces.
fn is_terminal_executable(name: &str) -> bool {
    TERMINAL_EXECUTABLES.contains(&name)
}

fn process_executable_name(pid: i32) -> Option<String> {
    {
        let mut cache = PROCESS_NAME_CACHE.lock().unwrap();
        let map = cache.get_or_insert_with(HashMap::new);
        if let Some(name) = map.get(&pid) {
            return Some(name.clone());
        }
    }

    let name = query_process_executable_name(pid)?;
    let mut cache = PROCESS_NAME_CACHE.lock().unwrap();
    cache.get_or_insert_with(HashMap::new).insert(pid, name.clone());
    Some(name)
}

fn query_process_executable_name(pid: i32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid as u32).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let result = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(handle);
        result.ok()?;
        let full_path = String::from_utf16_lossy(&buf[..len as usize]);
        let base = full_path.rsplit(['\\', '/']).next().unwrap_or(&full_path);
        Some(base.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_known_terminal_executables() {
        for name in TERMINAL_EXECUTABLES {
            assert!(is_terminal_executable(name), "{name} should be classified as a terminal");
        }
    }

    #[test]
    fn does_not_flag_ordinary_apps() {
        for name in ["notepad.exe", "chrome.exe", "code.exe", "winword.exe"] {
            assert!(!is_terminal_executable(name), "{name} should not be classified as a terminal");
        }
    }

    #[test]
    fn matching_is_case_sensitive_on_the_pre_lowercased_input() {
        // process_executable_name always lowercases before this check, so an uppercase
        // variant reaching is_terminal_executable directly (bypassing that step) should
        // not match — this pins the contract that callers must lowercase first.
        assert!(!is_terminal_executable("CMD.EXE"));
        assert!(is_terminal_executable("cmd.exe"));
    }

    #[test]
    fn unknown_pid_classifies_as_standard() {
        // No process should ever plausibly have this pid, so process_executable_name
        // returns None and classify() falls back to Standard.
        assert_eq!(classify(-1), Policy::Standard);
    }
}
