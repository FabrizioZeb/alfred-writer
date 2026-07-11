//! "Where on screen is the thing the user is editing" — used to position the popup near
//! the caret rather than at a corner of the (possibly huge) text control. Three strategies
//! are tried in order by `mod.rs`: UIA caret rect, classic Win32 system caret, then the
//! whole element's bounding rect as a last resort.

use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayUnaccessData};
use windows::Win32::UI::Accessibility::{IUIAutomationElement, IUIAutomationTextPattern, UIA_TextPatternId};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, GUITHREADINFO};

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl From<RECT> for Rect {
    fn from(r: RECT) -> Self {
        Self {
            left: r.left as f32,
            top: r.top as f32,
            right: r.right as f32,
            bottom: r.bottom as f32,
        }
    }
}

/// Bounding rect of the text caret/selection within the focused field, via UI Automation.
/// This is what makes the popup appear right next to where the user is typing instead of
/// at a corner of the whole (possibly huge) text control.
/// Returns:
/// The on-screen bounding rect of the caret/selection in `el`, or `None` if the element
/// has no `TextPattern`, no active selection, or no bounding rectangles to report.
pub(super) fn get_caret_rect(el: &IUIAutomationElement) -> Option<Rect> {
    unsafe {
        let pat = el.GetCurrentPattern(UIA_TextPatternId).ok()?;
        let tp = pat.cast::<IUIAutomationTextPattern>().ok()?;
        let sel = tp.GetSelection().ok()?;
        if sel.Length().ok()? < 1 {
            return None;
        }
        let range = sel.GetElement(0).ok()?;
        let psa = range.GetBoundingRectangles().ok()?;
        parse_first_rect(psa)
    }
}

unsafe fn parse_first_rect(psa: *mut SAFEARRAY) -> Option<Rect> {
    if psa.is_null() {
        return None;
    }
    let lbound = SafeArrayGetLBound(psa, 1).ok()?;
    let ubound = SafeArrayGetUBound(psa, 1).ok()?;
    let count = ubound - lbound + 1;
    if count < 4 {
        let _ = SafeArrayDestroy(psa);
        return None;
    }
    let mut data_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
    if SafeArrayAccessData(psa, &mut data_ptr).is_err() {
        let _ = SafeArrayDestroy(psa);
        return None;
    }
    let data = std::slice::from_raw_parts(data_ptr as *const f64, count as usize);
    let (left, top, width, height) = (data[0], data[1], data[2], data[3]);
    let _ = SafeArrayUnaccessData(psa);
    let _ = SafeArrayDestroy(psa);
    Some(Rect {
        left: left as f32,
        top: top as f32,
        right: (left + width) as f32,
        bottom: (top + height) as f32,
    })
}

/// Fallback for controls that don't expose UIA text bounding rects: reads the classic
/// Win32 caret position of the foreground thread (works for many native edit controls).
pub(super) fn get_system_caret_rect() -> Option<Rect> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }
        let tid = GetWindowThreadProcessId(hwnd, None);
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(tid, &mut info).ok()?;
        if info.hwndCaret.is_invalid() {
            return None;
        }
        let mut top_left = windows::Win32::Foundation::POINT {
            x: info.rcCaret.left,
            y: info.rcCaret.top,
        };
        let mut bottom_right = windows::Win32::Foundation::POINT {
            x: info.rcCaret.right,
            y: info.rcCaret.bottom,
        };
        if !ClientToScreen(info.hwndCaret, &mut top_left).as_bool()
            || !ClientToScreen(info.hwndCaret, &mut bottom_right).as_bool()
        {
            return None;
        }
        Some(Rect {
            left: top_left.x as f32,
            top: top_left.y as f32,
            right: bottom_right.x as f32,
            bottom: bottom_right.y as f32,
        })
    }
}
