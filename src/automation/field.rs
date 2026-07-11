//! Reading a UIA element: is it something we should even look at, and what's its text.

use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationElement, IUIAutomationTextPattern, IUIAutomationValuePattern,
    UIA_TextPatternId, UIA_ValuePatternId,
};

/// Whether `el` looks like a text field Alfred Writer should watch: not read-only (via
/// `ValuePattern`), or at least exposes `TextPattern` if `ValuePattern` isn't available.
///
/// Returns:
/// `true` if the element should be treated as editable text; `false` for read-only
/// controls or elements with neither pattern.
pub(super) fn is_editable(el: &IUIAutomationElement) -> bool {
    unsafe {
        if let Ok(pat) = el.GetCurrentPattern(UIA_ValuePatternId) {
            if let Ok(vp) = pat.cast::<IUIAutomationValuePattern>() {
                if let Ok(ro) = vp.CurrentIsReadOnly() {
                    return ro.as_bool() == false;
                }
                return true;
            }
        }
        if let Ok(pat) = el.GetCurrentPattern(UIA_TextPatternId) {
            return pat.cast::<IUIAutomationTextPattern>().is_ok();
        }
    }
    false
}

/// Whether `el` is a password field (UIA `IsPassword` property). These are never watched
/// or checked, regardless of `is_editable`.
pub(super) fn is_password(el: &IUIAutomationElement) -> bool {
    use windows::Win32::UI::Accessibility::UIA_IsPasswordPropertyId;
    unsafe {
        el.GetCurrentPropertyValue(UIA_IsPasswordPropertyId)
            .ok()
            .and_then(|v| bool::try_from(&v).ok())
            .unwrap_or(false)
    }
}

/// Reads the current full text of `el`, preferring `ValuePattern::CurrentValue` and
/// falling back to `TextPattern`'s document range.
///
/// Returns:
/// `Some(text)` if either UIA pattern yielded a value, `None` if neither did.
pub(super) fn read_text(el: &IUIAutomationElement) -> Option<String> {
    unsafe {
        if let Ok(pat) = el.GetCurrentPattern(UIA_ValuePatternId) {
            if let Ok(vp) = pat.cast::<IUIAutomationValuePattern>() {
                if let Ok(v) = vp.CurrentValue() {
                    return Some(v.to_string());
                }
            }
        }
        if let Ok(pat) = el.GetCurrentPattern(UIA_TextPatternId) {
            if let Ok(tp) = pat.cast::<IUIAutomationTextPattern>() {
                if let Ok(range) = tp.DocumentRange() {
                    if let Ok(s) = range.GetText(-1) {
                        return Some(s.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Blocks (briefly) until `el` actually reports as the OS-focused element, instead of
/// assuming a fixed sleep after `SetFocus()` was long enough — some apps take a beat to
/// actually switch focus, and pasting into the wrong window is a much worse failure than
/// waiting an extra 50ms.
pub(super) fn wait_for_focus(uia: &IUIAutomation, el: &IUIAutomationElement) {
    let deadline = Instant::now() + Duration::from_millis(400);
    loop {
        let matches = unsafe { uia.GetFocusedElement() }
            .ok()
            .map(|focused| unsafe { uia.CompareElements(&focused, el) }.map(|b| b.as_bool()).unwrap_or(false))
            .unwrap_or(false);
        if matches || Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
