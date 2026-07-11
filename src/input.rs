use std::time::Duration;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_V,
};

/// Inserts `text` over whatever is currently selected in the focused control by going
/// through the clipboard and sending Ctrl+V, restoring the previous clipboard contents
/// afterwards. This is far more reliable than sending the text as a burst of synthetic
/// per-character keystrokes: a long SendInput unicode-key burst can drop or mismatch a
/// down/up pair (especially right after a focus change), which Windows then reads as a
/// held key and auto-repeats — producing garbage like a suggestion collapsing into a
/// single repeated letter. A paste is just two or three key events, atomic either way.
///
/// Parameters:
/// - `text`: replacement text to paste over the current selection. Caller is responsible
///   for having already selected the target range (see `automation/replace.rs`).
///
/// Returns:
/// `true` if the clipboard was set and the Ctrl+V key events were sent successfully.
/// `false` doesn't necessarily mean nothing happened in the target field — only that this
/// function couldn't confirm success (clipboard access failed, or `SendInput` rejected an
/// event).
pub fn paste_text(text: &str) -> bool {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let previous = clipboard.get_text().ok();

    if clipboard.set_text(text.to_string()).is_err() {
        return false;
    }
    // Give the clipboard a moment to actually update before the target reads it.
    std::thread::sleep(Duration::from_millis(40));

    let ok = send_ctrl_v();

    // Let the paste land before we potentially overwrite the clipboard again.
    std::thread::sleep(Duration::from_millis(80));
    if let Some(prev) = previous {
        let _ = clipboard.set_text(prev);
    }

    ok
}

fn send_ctrl_v() -> bool {
    let inputs = [
        key_input(VK_CONTROL, false),
        key_input(VK_V, false),
        key_input(VK_V, true),
        key_input(VK_CONTROL, true),
    ];
    unsafe {
        let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        sent as usize == inputs.len()
    }
}

fn key_input(vk: VIRTUAL_KEY, key_up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if key_up { KEYEVENTF_KEYUP } else { Default::default() },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}
