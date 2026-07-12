//! "Where on screen is the thing the user is editing" — used to position the popup near
//! the caret rather than at a corner of the (possibly huge) text control. Three strategies
//! are tried in order by `mod.rs`: UIA caret rect, classic Win32 system caret, then the
//! whole element's bounding rect as a last resort.

use windows::core::Interface;
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Gdi::{ClientToScreen, GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
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
    parse_rects(psa).into_iter().next()
}

/// Decodes a UIA bounding-rectangles SAFEARRAY (flat `[left, top, width, height]*`
/// doubles — one quad per rendered line of the range) into screen rects. Consumes and
/// destroys `psa`.
unsafe fn parse_rects(psa: *mut SAFEARRAY) -> Vec<Rect> {
    if psa.is_null() {
        return Vec::new();
    }
    let Ok(lbound) = SafeArrayGetLBound(psa, 1) else {
        let _ = SafeArrayDestroy(psa);
        return Vec::new();
    };
    let Ok(ubound) = SafeArrayGetUBound(psa, 1) else {
        let _ = SafeArrayDestroy(psa);
        return Vec::new();
    };
    let count = ubound - lbound + 1;
    if count < 4 {
        let _ = SafeArrayDestroy(psa);
        return Vec::new();
    }
    let mut data_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
    if SafeArrayAccessData(psa, &mut data_ptr).is_err() {
        let _ = SafeArrayDestroy(psa);
        return Vec::new();
    }
    let data = std::slice::from_raw_parts(data_ptr as *const f64, count as usize);
    let rects = data
        .chunks_exact(4)
        .map(|q| Rect {
            left: q[0] as f32,
            top: q[1] as f32,
            right: (q[0] + q[2]) as f32,
            bottom: (q[1] + q[3]) as f32,
        })
        .collect();
    let _ = SafeArrayUnaccessData(psa);
    let _ = SafeArrayDestroy(psa);
    rects
}

/// On-screen rects (one per rendered line) of the first occurrence of `needle` within
/// `el`'s text — the anchor for drawing a clickable underline right beneath the flagged
/// span. Empty when the control has no `TextPattern`, the needle isn't in `text` (e.g.
/// the field changed since the check), or the provider reports no rectangles (span
/// scrolled out of view).
pub(super) fn text_span_rects(el: &IUIAutomationElement, text: &str, needle: &str) -> Vec<Rect> {
    unsafe {
        let Ok(pat) = el.GetCurrentPattern(UIA_TextPatternId) else {
            return Vec::new();
        };
        let Ok(tp) = pat.cast::<IUIAutomationTextPattern>() else {
            return Vec::new();
        };
        let Some((start, len)) = super::replace::find_utf16_offset(text, needle) else {
            return Vec::new();
        };
        if len == 0 {
            return Vec::new();
        }
        let Some(range) = super::replace::range_for_offset(&tp, start, len) else {
            return Vec::new();
        };
        let Ok(psa) = range.GetBoundingRectangles() else {
            return Vec::new();
        };
        parse_rects(psa)
    }
}

/// Gap between the anchor (caret/selection rect) and the popup edge, in pixels.
const ANCHOR_GAP: f32 = 6.0;

/// Computes where to place a `width` × `height` popup anchored to the caret rect
/// `anchor`, keeping it fully inside the work area (screen minus taskbar) of whichever
/// monitor the caret is on. Preferred placement is below the caret, left-aligned with
/// it (reading position for LTR text); if that would run off the bottom, the popup
/// flips above the caret instead of covering it; horizontal overflow slides it back
/// along the same edge. The popup therefore stays visually attached to the edited text
/// region rather than floating at a "roughly nearby" point that may be half off-screen.
pub fn place_popup(anchor: &Rect, width: f32, height: f32) -> (f32, f32) {
    // If the monitor can't be resolved (headless session, exotic display change race),
    // fall back to an unbounded work area — same behavior as before this existed.
    let work = monitor_work_area(anchor.left, anchor.bottom).unwrap_or(Rect {
        left: f32::MIN,
        top: f32::MIN,
        right: f32::MAX,
        bottom: f32::MAX,
    });
    place_within(anchor, width, height, &work)
}

/// Pure placement math split from [`place_popup`] so it's testable without a monitor.
fn place_within(anchor: &Rect, width: f32, height: f32, work: &Rect) -> (f32, f32) {
    let mut x = anchor.left;
    let mut y = anchor.bottom + ANCHOR_GAP;
    if x + width > work.right {
        x = work.right - width;
    }
    if x < work.left {
        x = work.left;
    }
    if y + height > work.bottom {
        // Would cover the taskbar / run off-screen: flip above the caret instead of
        // sliding down over the line being edited.
        y = anchor.top - ANCHOR_GAP - height;
    }
    if y < work.top {
        y = work.top;
    }
    (x, y)
}

/// Work area (screen minus taskbar/docked bars) of the monitor nearest to `(x, y)`.
fn monitor_work_area(x: f32, y: f32) -> Option<Rect> {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: x as i32, y: y as i32 }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return None;
        }
        Some(Rect::from(info.rcWork))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Rect = Rect { left: 0.0, top: 0.0, right: 1920.0, bottom: 1040.0 };

    fn caret_at(left: f32, top: f32) -> Rect {
        Rect { left, top, right: left + 2.0, bottom: top + 18.0 }
    }

    #[test]
    fn prefers_below_the_caret_left_aligned() {
        let (x, y) = place_within(&caret_at(500.0, 300.0), 320.0, 200.0, &WORK);
        assert_eq!(x, 500.0);
        assert_eq!(y, 318.0 + ANCHOR_GAP);
    }

    #[test]
    fn slides_left_when_overflowing_the_right_edge() {
        let (x, _) = place_within(&caret_at(1850.0, 300.0), 320.0, 200.0, &WORK);
        assert_eq!(x, 1920.0 - 320.0);
    }

    #[test]
    fn flips_above_the_caret_when_overflowing_the_bottom() {
        let anchor = caret_at(500.0, 1000.0);
        let (_, y) = place_within(&anchor, 320.0, 200.0, &WORK);
        assert_eq!(y, anchor.top - ANCHOR_GAP - 200.0);
        assert!(y + 200.0 <= anchor.top, "flipped popup must not cover the edited line");
    }

    #[test]
    fn never_escapes_the_work_area_even_for_corner_carets() {
        for (cx, cy) in [(0.0, 0.0), (1919.0, 0.0), (0.0, 1039.0), (1919.0, 1039.0)] {
            let (x, y) = place_within(&caret_at(cx, cy), 320.0, 200.0, &WORK);
            assert!(x >= WORK.left && x + 320.0 <= WORK.right, "x={x} for caret ({cx},{cy})");
            assert!(y >= WORK.top, "y={y} for caret ({cx},{cy})");
        }
    }
}
