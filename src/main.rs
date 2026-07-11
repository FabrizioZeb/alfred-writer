#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use alfred_writer::{app, automation, config, tray};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};

fn main() -> eframe::Result<()> {
    let config = Arc::new(Mutex::new(config::Config::load()));
    let enabled = config.lock().unwrap().enabled;

    let (ui_tx, ui_rx) = channel();
    let automation_handle = automation::spawn(config.clone(), ui_tx);

    let tray_handle = tray::build(enabled).expect("failed to create system tray icon");
    let quit = Arc::new(AtomicBool::new(false));

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_visible(false)
            .with_decorations(false)
            .with_inner_size([1.0, 1.0]),
        ..Default::default()
    };

    let quit_for_app = quit.clone();
    let app_cmd_tx = automation_handle.cmd_tx.clone();
    let result = eframe::run_native(
        "Alfred Writer",
        native_options,
        Box::new(move |_cc| {
            Ok(Box::new(app::App::new(
                config.clone(),
                ui_rx,
                tray_handle.rx,
                app_cmd_tx,
                quit_for_app,
            )))
        }),
    );

    let _ = automation_handle.cmd_tx.send(automation::AutomationCmd::Shutdown);
    if quit.load(Ordering::SeqCst) {
        std::process::exit(0);
    }
    result
}
