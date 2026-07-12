//! Turning an (original, suggestion) pair into an actual edit in the focused field.
//!
//! See SKILLS.md invariants 1–3 before touching this file: corrections go through
//! clipboard-paste (never per-character SendInput), ranges are built from character
//! offsets we compute ourselves (never `FindText` alone), and focus is *waited for*, not
//! slept for.

use super::field::{read_text, wait_for_focus};
use std::time::Duration;
use windows::core::{Interface, BSTR};
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationElement, IUIAutomationTextPattern, IUIAutomationTextRange,
    IUIAutomationValuePattern, TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start,
    TextUnit_Character, UIA_TextPatternId, UIA_ValuePatternId,
};

/// Finds `needle` in `haystack` and returns (start, len) as UTF-16 code-unit counts —
/// the unit UIA's TextUnit_Character moves by — rather than Rust byte offsets.
///
/// Returns:
/// `Some((start, len))` in UTF-16 code units for the first occurrence of `needle`, or
/// `None` if `needle` isn't a substring of `haystack`.
pub(super) fn find_utf16_offset(haystack: &str, needle: &str) -> Option<(i32, i32)> {
    let byte_idx = haystack.find(needle)?;
    let start = haystack[..byte_idx].encode_utf16().count() as i32;
    let len = needle.encode_utf16().count() as i32;
    Some((start, len))
}

/// Builds a text range covering [start, start+len) (in UTF-16 code units) by walking a
/// clone of the document range's endpoints, rather than trusting FindText's own
/// (pickier) verbatim search.
pub(super) fn range_for_offset(tp: &IUIAutomationTextPattern, start: i32, len: i32) -> Option<IUIAutomationTextRange> {
    unsafe {
        let range = tp.DocumentRange().ok()?;
        // Collapse End down onto Start (both end up at document position 0).
        range.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, -1_000_000).ok()?;
        // Moving Start past the (now coincident) End drags End along, so both land at `start`.
        range.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, start).ok()?;
        // Extend End forward by `len` from its current position (`start`).
        range.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, len).ok()?;
        Some(range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_ascii_substring() {
        assert_eq!(find_utf16_offset("hello world", "world"), Some((6, 5)));
    }

    #[test]
    fn finds_match_at_start() {
        assert_eq!(find_utf16_offset("hello world", "hello"), Some((0, 5)));
    }

    #[test]
    fn returns_none_when_not_found() {
        assert_eq!(find_utf16_offset("hello world", "goodbye"), None);
    }

    #[test]
    fn returns_first_occurrence_when_needle_repeats() {
        // "the the" -> second "the" starts at byte 4 == UTF-16 offset 4 (all ASCII).
        assert_eq!(find_utf16_offset("the the cat", "the"), Some((0, 3)));
    }

    #[test]
    fn counts_utf16_code_units_not_bytes_for_multibyte_text_before_the_match() {
        // "café " is 5 chars but 6 UTF-8 bytes ('é' is 2 bytes) and 5 UTF-16 code units
        // (BMP character, still 1 code unit) — offset must be in UTF-16 units, not bytes.
        let haystack = "café world";
        assert_eq!(find_utf16_offset(haystack, "world"), Some((5, 5)));
    }

    #[test]
    fn counts_surrogate_pairs_as_two_utf16_units() {
        // An emoji outside the BMP encodes as a UTF-16 surrogate pair (2 code units),
        // which is what UIA's TextUnit_Character walks by.
        let haystack = "hi \u{1F600} bye";
        assert_eq!(find_utf16_offset(haystack, "bye"), Some((6, 3)));
    }

    #[test]
    fn empty_needle_matches_at_start_with_zero_length() {
        assert_eq!(find_utf16_offset("hello", ""), Some((0, 0)));
    }
}

/// Replaces the first occurrence of `original` with `suggestion` inside `el`'s text.
///
/// Tries, in order: an offset-based `TextPattern` selection (primary path — see module
/// docs), a `TextPattern::FindText` fallback, then a whole-value `ValuePattern::SetValue`
/// fallback for controls with no usable `TextPattern`.
///
/// Parameters:
/// - `uia`: the UI Automation COM instance (used to poll for focus).
/// - `el`: the target field, which must currently be (or become) OS-focused.
/// - `original`: exact substring to replace; if it's not found in `el`'s current text via
///   any of the three strategies, this is a no-op.
/// - `suggestion`: replacement text.
///
/// Returns:
/// `true` if a replacement strategy reported success, `false` if all of them failed or
/// `original` couldn't be located.
pub(super) fn apply_replacement(uia: &IUIAutomation, el: &IUIAutomationElement, original: &str, suggestion: &str) -> bool {
    unsafe {
        // Clicking Apply in the popup viewport moves OS keyboard focus to our own
        // window, which breaks both text-range selection (many UIA providers refuse
        // to select in an unfocused control) and paste (goes to whichever window
        // currently has focus). Reclaim focus for the target field, and actually wait
        // for it to land instead of hoping a fixed sleep was long enough.
        let focus_result = el.SetFocus();
        eprintln!("[alfred-writer] apply: SetFocus -> {:?}", focus_result);
        wait_for_focus(uia, el);

        // Prefer TextPattern: locate the exact range and select+paste over it so
        // rich/contenteditable-like controls (not just simple edit boxes) work too.
        //
        // We build the range from character offsets computed by searching the text
        // *we* just read (read_text uses the same DocumentRange().GetText() this
        // targets), rather than relying on TextPattern::FindText's own verbatim
        // matching. FindText is pickier about whitespace/line-wrap representation,
        // which made multi-word phrase matches fail more often than single-word ones
        // even though both were verbatim substrings of what we displayed.
        if let Ok(pat) = el.GetCurrentPattern(UIA_TextPatternId) {
            if let Ok(tp) = pat.cast::<IUIAutomationTextPattern>() {
                if let Some(text) = read_text(el) {
                    if let Some((start, len)) = find_utf16_offset(&text, original) {
                        if let Some(range) = range_for_offset(&tp, start, len) {
                            let select_result = range.Select();
                            eprintln!("[alfred-writer] apply: Select (offset {start}+{len}) -> {:?}", select_result);
                            if select_result.is_ok() {
                                // Let the selection actually take effect before pasting over it.
                                std::thread::sleep(Duration::from_millis(40));
                                let pasted = crate::input::paste_text(suggestion);
                                eprintln!("[alfred-writer] apply: paste_text -> {pasted}");
                                return pasted;
                            }
                        }
                    }
                }

                // Fallback: TextPattern's own verbatim search, in case the offset
                // route failed for some provider-specific reason.
                if let Ok(doc) = tp.DocumentRange() {
                    let needle = BSTR::from(original);
                    if let Ok(found) = doc.FindText(&needle, false, false) {
                        if !Interface::as_raw(&found).is_null() && found.Select().is_ok() {
                            std::thread::sleep(Duration::from_millis(40));
                            let pasted = crate::input::paste_text(suggestion);
                            eprintln!("[alfred-writer] apply: FindText fallback paste_text -> {pasted}");
                            return pasted;
                        }
                    }
                }
            }
        }

        // Fallback: ValuePattern, replace whole value.
        if let Ok(pat) = el.GetCurrentPattern(UIA_ValuePatternId) {
            if let Ok(vp) = pat.cast::<IUIAutomationValuePattern>() {
                if let Ok(current) = vp.CurrentValue() {
                    let current = current.to_string();
                    if let Some(idx) = current.find(original) {
                        let new_val = format!("{}{}{}", &current[..idx], suggestion, &current[idx + original.len()..]);
                        let set_result = vp.SetValue(&BSTR::from(new_val));
                        eprintln!("[alfred-writer] apply: ValuePattern SetValue -> {:?}", set_result);
                        return set_result.is_ok();
                    }
                }
            }
        }
    }
    false
}
