use crate::theme;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub enum TrayEvent {
    ToggleEnabled(bool),
    OpenSettings,
    Quit,
}

pub struct TrayHandle {
    pub rx: Receiver<TrayEvent>,
    _tray: TrayIcon,
}

/// Creates the system tray icon and menu (Enabled / Settings… / Quit), and spawns a
/// thread that forwards menu clicks onto a channel for the UI thread to consume.
///
/// Parameters:
/// - `enabled`: initial checked-state for the "Enabled" toggle, from the loaded config.
///
/// Returns:
/// A [`TrayHandle`] whose `rx` receiver yields a [`TrayEvent`] per menu click. Errors if
/// the tray icon or menu can't be created.
pub fn build(enabled: bool) -> anyhow::Result<TrayHandle> {
    let menu = Menu::new();
    let toggle = CheckMenuItem::new("Enabled", true, enabled, None);
    let settings = MenuItem::new("Settings…", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append_items(&[
        &toggle,
        &PredefinedMenuItem::separator(),
        &settings,
        &quit,
    ])?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Alfred Writer (AW)")
        .with_icon(build_icon())
        .build()?;

    let toggle_id = toggle.id().clone();
    let settings_id = settings.id().clone();
    let quit_id = quit.id().clone();
    let enabled_state = Arc::new(AtomicBool::new(enabled));

    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let receiver = MenuEvent::receiver();
        while let Ok(event) = receiver.recv() {
            let ev = if event.id == toggle_id {
                let new_val = !enabled_state.load(Ordering::SeqCst);
                enabled_state.store(new_val, Ordering::SeqCst);
                Some(TrayEvent::ToggleEnabled(new_val))
            } else if event.id == settings_id {
                Some(TrayEvent::OpenSettings)
            } else if event.id == quit_id {
                Some(TrayEvent::Quit)
            } else {
                None
            };
            if let Some(ev) = ev {
                if tx.send(ev).is_err() {
                    break;
                }
            }
        }
    });

    Ok(TrayHandle { rx, _tray: tray })
}

/// The tray's two-tone AW badge — see [`theme::badge_rgba`] for why it's ring+fill only,
/// no monogram text, at this size.
fn build_icon() -> Icon {
    let size = 32;
    Icon::from_rgba(theme::badge_rgba(size), size, size).expect("valid icon buffer")
}
