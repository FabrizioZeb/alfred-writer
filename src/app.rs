use crate::automation::{AutomationCmd, Rect, UiEvent};
use crate::claude::Issue;
use crate::config::{Config, MODELS};
use crate::tray::TrayEvent;
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const PURPLE: egui::Color32 = egui::Color32::from_rgb(0x4b, 0x3f, 0xd6);
const RED: egui::Color32 = egui::Color32::from_rgb(0xb4, 0x23, 0x18);
const GREEN: egui::Color32 = egui::Color32::from_rgb(0x06, 0x76, 0x47);
const GREY: egui::Color32 = egui::Color32::from_rgb(0x6b, 0x70, 0x85);

#[derive(Clone)]
enum PopupState {
    Loading { rect: Rect },
    Issues { rect: Rect, issues: Vec<Issue> },
    Error { rect: Rect, message: String },
}

struct SettingsDraft {
    model_idx: usize,
    enabled: bool,
    status: Option<(String, Instant)>,
}

pub struct App {
    config: Arc<Mutex<Config>>,
    ui_rx: Receiver<UiEvent>,
    tray_rx: Receiver<TrayEvent>,
    automation_cmd_tx: Sender<AutomationCmd>,
    quit: Arc<AtomicBool>,
    popup: Option<PopupState>,
    show_settings: bool,
    settings_open_request: bool,
    draft: SettingsDraft,
}

impl App {
    /// Builds the (initially invisible) egui application.
    ///
    /// Parameters:
    /// - `config`: shared settings, read to seed the settings-window draft state.
    /// - `ui_rx`: receives [`UiEvent`]s from the automation thread to drive the popup.
    /// - `tray_rx`: receives clicks from the system tray menu.
    /// - `automation_cmd_tx`: sends [`AutomationCmd`]s back to the automation thread
    ///   (currently just `Apply`, on an Apply-button click).
    /// - `quit`: shared flag set when the tray's Quit item is clicked, checked by `main`
    ///   after the eframe event loop exits.
    pub fn new(
        config: Arc<Mutex<Config>>,
        ui_rx: Receiver<UiEvent>,
        tray_rx: Receiver<TrayEvent>,
        automation_cmd_tx: Sender<AutomationCmd>,
        quit: Arc<AtomicBool>,
    ) -> Self {
        let c = config.lock().unwrap();
        let model_idx = MODELS.iter().position(|m| *m == c.model).unwrap_or(0);
        let draft = SettingsDraft {
            model_idx,
            enabled: c.enabled,
            status: None,
        };
        drop(c);
        Self {
            config,
            ui_rx,
            tray_rx,
            automation_cmd_tx,
            quit,
            popup: None,
            show_settings: false,
            settings_open_request: false,
            draft,
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(ev) = self.ui_rx.try_recv() {
            match ev {
                UiEvent::Hide => self.popup = None,
                UiEvent::Loading { rect } => self.popup = Some(PopupState::Loading { rect }),
                UiEvent::Issues { rect, issues } => {
                    if issues.is_empty() {
                        self.popup = None;
                    } else {
                        self.popup = Some(PopupState::Issues { rect, issues });
                    }
                }
                UiEvent::Error { rect, message } => self.popup = Some(PopupState::Error { rect, message }),
            }
        }

        while let Ok(ev) = self.tray_rx.try_recv() {
            match ev {
                TrayEvent::ToggleEnabled(enabled) => {
                    let mut c = self.config.lock().unwrap();
                    c.enabled = enabled;
                    let _ = c.save();
                    self.draft.enabled = enabled;
                }
                TrayEvent::OpenSettings => {
                    self.settings_open_request = true;
                    self.show_settings = true;
                }
                TrayEvent::Quit => {
                    self.quit.store(true, Ordering::SeqCst);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        if self.show_settings {
            self.show_settings_window(ctx);
        }

        if let Some(popup) = self.popup.clone() {
            self.show_popup_window(ctx, &popup);
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(120));
    }
}

impl App {
    fn show_settings_window(&mut self, ctx: &egui::Context) {
        let id = egui::ViewportId::from_hash_of("alfred-writer-settings");
        let mut open = true;
        let mut builder = egui::ViewportBuilder::default()
            .with_title("Alfred Writer — Settings")
            .with_inner_size([360.0, 260.0])
            .with_resizable(false);
        if self.settings_open_request {
            builder = builder.with_position(egui::pos2(200.0, 200.0));
            self.settings_open_request = false;
        }

        let draft = &mut self.draft;
        let config = &self.config;

        ctx.show_viewport_immediate(id, builder, |ctx, _class| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add_space(6.0);
                ui.heading("Alfred Writer");
                ui.label("Grammar and style checking, system-wide, powered by Claude Code.");
                ui.add_space(10.0);

                ui.label("Model");
                egui::ComboBox::from_id_source("model_combo")
                    .selected_text(MODELS[draft.model_idx])
                    .show_ui(ui, |ui| {
                        for (i, m) in MODELS.iter().enumerate() {
                            ui.selectable_value(&mut draft.model_idx, i, *m);
                        }
                    });

                ui.add_space(8.0);
                ui.checkbox(&mut draft.enabled, "Enabled");

                ui.add_space(12.0);
                if ui.button("Save").clicked() {
                    let mut c = config.lock().unwrap();
                    c.model = MODELS[draft.model_idx].to_string();
                    c.enabled = draft.enabled;
                    let _ = c.save();
                    draft.status = Some(("Saved.".to_string(), Instant::now()));
                }

                if let Some((msg, at)) = &draft.status {
                    if at.elapsed().as_secs_f32() < 1.8 {
                        ui.label(egui::RichText::new(msg).color(GREEN));
                    } else {
                        draft.status = None;
                    }
                }

                ui.add_space(10.0);
                ui.small("Uses your existing `claude` CLI login — no separate API key needed.");
            });

            if ctx.input(|i| i.viewport().close_requested()) {
                open = false;
            }
        });

        if !open {
            self.show_settings = false;
        }
    }

    fn show_popup_window(&mut self, ctx: &egui::Context, popup: &PopupState) {
        let rect = match popup {
            PopupState::Loading { rect } => *rect,
            PopupState::Issues { rect, .. } => *rect,
            PopupState::Error { rect, .. } => *rect,
        };

        let body_height: f32 = match popup {
            PopupState::Loading { .. } => 40.0,
            PopupState::Error { .. } => 60.0,
            PopupState::Issues { issues, .. } => (issues.len() as f32 * 92.0).clamp(60.0, 300.0),
        };
        let window_height = 70.0 + body_height;

        let id = egui::ViewportId::from_hash_of("alfred-writer-popup");
        let builder = egui::ViewportBuilder::default()
            .with_title("Alfred Writer")
            .with_inner_size([320.0, window_height])
            .with_position(egui::pos2(rect.left, rect.bottom + 6.0))
            .with_decorations(false)
            .with_always_on_top()
            .with_resizable(false)
            .with_transparent(true);

        let mut close_clicked = false;
        let mut apply_click: Option<(usize, String, String)> = None;
        let mut dismiss_click: Option<usize> = None;

        ctx.show_viewport_immediate(id, builder, |ctx, _class| {
            egui::CentralPanel::default()
                .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::WHITE))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Alfred Writer").strong().color(PURPLE));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("✕").clicked() {
                                close_clicked = true;
                            }
                        });
                    });
                    ui.separator();

                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| match popup {
                        PopupState::Loading { .. } => {
                            ui.label("Checking your writing…");
                        }
                        PopupState::Error { message, .. } => {
                            ui.colored_label(RED, message);
                        }
                        PopupState::Issues { issues, .. } => {
                            for (i, issue) in issues.iter().enumerate() {
                                // Unique ID salt per row — without this, every issue's
                                // widgets (Apply/Dismiss buttons) share the same
                                // auto-generated egui ID (same call site each loop turn),
                                // which causes ID clashes and scrambles click routing.
                                ui.push_id(i, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.colored_label(RED, egui::RichText::new(&issue.original).strikethrough());
                                        ui.colored_label(GREY, "→");
                                        ui.colored_label(GREEN, egui::RichText::new(&issue.suggestion).strong());
                                    });

                                    if !issue.explanation.is_empty() {
                                        ui.colored_label(GREY, &issue.explanation);
                                    }
                                    ui.horizontal(|ui| {
                                        if ui.button("Apply").clicked() {
                                            apply_click = Some((i, issue.original.clone(), issue.suggestion.clone()));
                                        }
                                        if ui.button("Dismiss").clicked() {
                                            dismiss_click = Some(i);
                                        }
                                    });
                                    ui.separator();
                                });
                            }
                        }
                    });

                    ui.small("Powered by Claude · double-check important text");
                });

            if ctx.input(|i| i.viewport().close_requested()) {
                close_clicked = true;
            }
        });

        if let Some((idx, original, suggestion)) = apply_click {
            let _ = self.automation_cmd_tx.send(AutomationCmd::Apply { original, suggestion });
            if let Some(PopupState::Issues { issues, rect }) = self.popup.take() {
                let mut issues = issues;
                issues.remove(idx);
                self.popup = if issues.is_empty() {
                    None
                } else {
                    Some(PopupState::Issues { rect, issues })
                };
            }
        } else if let Some(idx) = dismiss_click {
            if let Some(PopupState::Issues { issues, rect }) = self.popup.take() {
                let mut issues = issues;
                issues.remove(idx);
                self.popup = if issues.is_empty() {
                    None
                } else {
                    Some(PopupState::Issues { rect, issues })
                };
            }
        } else if close_clicked {
            self.popup = None;
        }
    }
}
