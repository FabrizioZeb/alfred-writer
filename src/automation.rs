//! Watches the OS-focused text field, decides when to check it with the configured LLM
//! provider, and applies corrections back. This file is the orchestrator only — the
//! actual mechanics live in submodules by concern:
//!
//! - `field`    — reading a UIA element (editable? password? what's its text?).
//! - `geometry` — where on screen to put the popup.
//! - `replace`  — turning an (original, suggestion) pair into an actual edit.
//! - `cache`    — exact-text -> issues memoization.
//!
//! The debounce/cooldown/single-flight/cache combo in `run()` below is load-bearing, not
//! incidental — every check may cost real time and/or money (a cloud API call or a
//! subprocess), so it exists specifically to bound how often that happens.

mod cache;
mod field;
mod geometry;
mod replace;

pub use geometry::Rect;

use crate::config::Config;
use crate::providers::{self, CancellationHandle, CancellationToken, Issue, ProviderResponse};
use crate::targets;
use cache::{cache_get, cache_insert, new_cache};
use field::{is_editable, is_password, read_text};
use geometry::{get_caret_rect, get_system_caret_rect};
use replace::apply_replacement;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation, IUIAutomationElement};

// Each check may hit a cloud API or spawn a subprocess (multi-second, possibly costs real
// money), so we're deliberately much more conservative here than a typical "check as you
// type" debounce: wait for a longer pause, enforce a cooldown between checks on the same
// field, never run more than one check at a time, and cache results so retyping/undoing
// back to a previously-seen exact text is free.
const DEBOUNCE: Duration = Duration::from_millis(2500);
const MIN_CHECK_INTERVAL: Duration = Duration::from_secs(6);
const MIN_LENGTH: usize = 12;
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Messages the automation thread sends to the UI thread to drive the popup.
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// Hide the popup (field lost focus, text no longer needs review, etc).
    Hide,
    /// Show a "checking…" state near `rect` while a provider call is in flight.
    Loading { rect: Rect },
    /// Show flagged issues near `rect`. An empty `issues` vec is treated like `Hide`.
    Issues { rect: Rect, issues: Vec<Issue> },
    /// Show an error message near `rect` (provider failure, timeout, parse failure).
    Error { rect: Rect, message: String },
}

/// Commands the UI thread sends to the automation thread.
pub enum AutomationCmd {
    /// User clicked Apply on a suggestion: replace `original` with `suggestion` in the
    /// currently tracked field.
    Apply { original: String, suggestion: String },
    /// Stop the automation thread's loop.
    Shutdown,
}

/// Handle returned by [`spawn`] for sending commands into the automation thread.
pub struct AutomationHandle {
    pub cmd_tx: Sender<AutomationCmd>,
}

/// Starts the automation thread: initializes UI Automation on its own COM apartment and
/// runs the watch → debounce → check → apply loop for the lifetime of the process.
///
/// Parameters:
/// - `config`: shared settings (provider, enabled) read on every poll.
/// - `ui_tx`: channel the loop sends [`UiEvent`]s on to drive the popup.
///
/// Returns:
/// An [`AutomationHandle`] whose `cmd_tx` is how the UI thread sends [`AutomationCmd`]s
/// (Apply / Shutdown) back into the loop.
pub fn spawn(config: Arc<Mutex<Config>>, ui_tx: Sender<UiEvent>) -> AutomationHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel::<AutomationCmd>();

    std::thread::spawn(move || {
        if let Err(e) = run(config, ui_tx, cmd_rx) {
            eprintln!("automation thread exited: {e:?}");
        }
    });

    AutomationHandle { cmd_tx }
}

fn run(config: Arc<Mutex<Config>>, ui_tx: Sender<UiEvent>, cmd_rx: Receiver<AutomationCmd>) -> anyhow::Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    let uia: IUIAutomation = unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };

    let mut current_element: Option<IUIAutomationElement> = None;
    let mut last_text = String::new();
    let mut last_change = Instant::now();
    let mut dirty = false;
    let mut popup_visible = false;
    let mut last_check_at: Option<Instant> = None;
    let current_gen = Arc::new(AtomicU64::new(0));
    let check_in_flight = Arc::new(AtomicBool::new(false));
    let issue_cache = new_cache();
    let own_pid = std::process::id() as i32;
    // Holds the cancellation handle for whichever check is currently in flight, so a
    // field/focus change that makes that check moot can cancel it early instead of just
    // discarding its result once it eventually finishes.
    let current_cancel: Arc<Mutex<Option<CancellationHandle>>> = Arc::new(Mutex::new(None));

    loop {
        match cmd_rx.recv_timeout(POLL_INTERVAL) {
            Ok(AutomationCmd::Apply { original, suggestion }) => {
                if let Some(el) = current_element.as_ref() {
                    let ok = apply_replacement(&uia, el, &original, &suggestion);
                    if !ok {
                        eprintln!("failed to apply replacement");
                    }
                    // Re-sync our tracked text after an edit so we don't immediately re-check it.
                    if let Some(text) = read_text(el) {
                        last_text = text;
                        dirty = false;
                    }
                }
                continue;
            }
            Ok(AutomationCmd::Shutdown) => {
                if let Some(handle) = current_cancel.lock().unwrap().take() {
                    handle.cancel();
                }
                break;
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }

        let enabled = config.lock().unwrap().enabled;

        let focused = unsafe { uia.GetFocusedElement() }.ok();

        // Clicking into our own popup/settings windows moves OS focus there. Don't
        // treat that as "the field lost focus" — just ignore the poll and keep
        // tracking the field we already had, so the popup doesn't vanish out from
        // under the user's click.
        if let Some(f) = &focused {
            let pid = unsafe { f.CurrentProcessId() }.unwrap_or(0);
            if pid == own_pid {
                continue;
            }
        }

        let same_as_before = match (&focused, &current_element) {
            (Some(a), Some(b)) => unsafe { uia.CompareElements(a, b) }.map(|b| b.as_bool()).unwrap_or(false),
            (None, None) => true,
            _ => false,
        };

        if !same_as_before {
            // The field we were about to hear back on no longer matters — cancel rather
            // than let it run to completion only to have its result discarded.
            if let Some(handle) = current_cancel.lock().unwrap().take() {
                handle.cancel();
            }

            current_element = focused.clone();
            last_check_at = None;
            if popup_visible {
                popup_visible = false;
                let _ = ui_tx.send(UiEvent::Hide);
            }

            // Re-focusing a field used to always clear last_text to "", which made the
            // very next poll see text != last_text and kick off a fresh debounce/check
            // cycle even when nothing had actually changed. Instead: read what's there
            // now and, if we've already checked this exact text before (cache hit),
            // treat it as clean — no debounce, no check, no provider call. Only text
            // we've genuinely never checked starts a debounce cycle.
            match current_element.as_ref().and_then(read_text) {
                Some(text) => {
                    if cache_get(&issue_cache, &text).is_some() {
                        last_text = text;
                        dirty = false;
                    } else {
                        last_text = text;
                        last_change = Instant::now();
                        dirty = true;
                    }
                }
                None => {
                    last_text.clear();
                    dirty = false;
                }
            }
        }

        // Terminals (and similar) expose their scrollback as UIA text, but most of it is
        // immutable history — only the live prompt line is really editable, and it isn't
        // reliably addressable as a substring within that buffer. Trying anyway is what
        // caused corrections to land at the end of the buffer instead of in place. See
        // src/targets.rs.
        if let Some(el) = current_element.as_ref() {
            let pid = unsafe { el.CurrentProcessId() }.unwrap_or(0);
            if targets::classify(pid) == targets::Policy::Skip {
                continue;
            }
        }

        if !enabled {
            continue;
        }

        let Some(el) = current_element.as_ref() else {
            continue;
        };

        if !is_editable(el) || is_password(el) {
            continue;
        }

        let Some(text) = read_text(el) else {
            continue;
        };

        if text != last_text {
            last_text = text.clone();
            last_change = Instant::now();
            dirty = true;
            if text.trim().chars().count() < MIN_LENGTH && popup_visible {
                popup_visible = false;
                let _ = ui_tx.send(UiEvent::Hide);
            }
        }

        // Gate on: long-enough pause, a cooldown since the last check on this field, and
        // no check already running (we never let two provider calls overlap). If any of
        // these hold us back, `dirty` stays true so we simply retry on a later poll
        // instead of dropping the check.
        let cooldown_elapsed = last_check_at.map_or(true, |t| t.elapsed() >= MIN_CHECK_INTERVAL);
        if dirty
            && last_change.elapsed() >= DEBOUNCE
            && last_text.trim().chars().count() >= MIN_LENGTH
            && cooldown_elapsed
            && !check_in_flight.load(Ordering::SeqCst)
        {
            dirty = false;
            last_check_at = Some(Instant::now());

            let rect: Rect = if let Some(r) = get_caret_rect(el) {
                r
            } else if let Some(r) = get_system_caret_rect() {
                r
            } else if let Some(r) = unsafe { el.CurrentBoundingRectangle() }.map(Rect::from).ok() {
                r
            } else {
                Rect { left: 100.0, top: 100.0, right: 400.0, bottom: 130.0 }
            };

            let text_for_check = last_text.clone();

            // Exact-text cache hit: identical text was already checked (retyped,
            // undone, or refocused) — skip calling the provider entirely.
            if let Some(cached) = cache_get(&issue_cache, &text_for_check) {
                let valid: Vec<Issue> = cached.into_iter().filter(|i| text_for_check.contains(&i.original)).collect();
                popup_visible = !valid.is_empty();
                let _ = ui_tx.send(if valid.is_empty() {
                    UiEvent::Hide
                } else {
                    UiEvent::Issues { rect, issues: valid }
                });
                continue;
            }

            popup_visible = true;
            let _ = ui_tx.send(UiEvent::Loading { rect });

            let gen = current_gen.fetch_add(1, Ordering::SeqCst) + 1;
            let gen_check = current_gen.clone();
            let provider_config = config.lock().unwrap().provider.clone();
            let ui_tx2 = ui_tx.clone();
            let in_flight = check_in_flight.clone();
            let cache_for_thread = issue_cache.clone();
            in_flight.store(true, Ordering::SeqCst);

            let (cancel_token, cancel_handle) = CancellationToken::new();
            *current_cancel.lock().unwrap() = Some(cancel_handle);
            let cancel_for_thread = cancel_token.clone();

            std::thread::spawn(move || {
                let provider = providers::build(&provider_config);
                let request = providers::PromptRequest {
                    model: provider_config.model().to_string(),
                    system_prompt: providers::default_system_prompt().to_string(),
                    schema: providers::issue_schema(),
                    text: text_for_check.clone(),
                };
                let result = provider.execute(&request, &cancel_for_thread);
                in_flight.store(false, Ordering::SeqCst);
                if gen_check.load(Ordering::SeqCst) != gen {
                    return; // stale, field changed again before this returned
                }
                match result {
                    ProviderResponse::Issues(issues) => {
                        cache_insert(&cache_for_thread, text_for_check.clone(), issues.clone());
                        let valid: Vec<Issue> = issues
                            .into_iter()
                            .filter(|i| text_for_check.contains(&i.original))
                            .collect();
                        let _ = ui_tx2.send(UiEvent::Issues { rect, issues: valid });
                    }
                    ProviderResponse::Error(message) => {
                        let _ = ui_tx2.send(UiEvent::Error { rect, message });
                    }
                    ProviderResponse::Cancelled => {}
                }
            });
        }
    }

    Ok(())
}
