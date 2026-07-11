use crate::automation::{AutomationCmd, Rect, UiEvent};
use crate::config::Config;
use crate::providers::{ExternalCommandConfig, InputMode, Issue, LocalConfig, ProviderConfig};
use crate::theme;
use crate::tray::TrayEvent;
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone)]
enum PopupState {
    Loading { rect: Rect },
    Issues { rect: Rect, issues: Vec<Issue> },
    Error { rect: Rect, message: String },
}

/// Which provider type is selected in the Settings dropdown. Kept distinct from
/// [`ProviderConfig`] so switching the dropdown mid-edit doesn't lose whatever was
/// already typed into the other providers' fields this session.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProviderKind {
    Local,
    ExternalCommand,
}

impl ProviderKind {
    const ALL: [ProviderKind; 2] = [ProviderKind::Local, ProviderKind::ExternalCommand];

    fn label(self) -> &'static str {
        match self {
            ProviderKind::Local => "Local (Ollama / LM Studio)",
            ProviderKind::ExternalCommand => "External command",
        }
    }

    fn from_config(config: &ProviderConfig) -> Self {
        match config {
            ProviderConfig::Local(_) => ProviderKind::Local,
            ProviderConfig::ExternalCommand(_) => ProviderKind::ExternalCommand,
        }
    }
}

/// Editable form of [`ExternalCommandConfig`]: `args_template` becomes one line of text
/// per argument, `env` becomes one `KEY=VALUE` per line, and `timeout_secs` becomes a
/// plain text field, so egui can edit them.
struct ExternalCommandDraft {
    command: String,
    args_text: String,
    input_mode: InputMode,
    response_path: String,
    error_path: String,
    model: String,
    timeout_secs: String,
    env_text: String,
}

impl From<&ExternalCommandConfig> for ExternalCommandDraft {
    fn from(c: &ExternalCommandConfig) -> Self {
        Self {
            command: c.command.clone(),
            args_text: c.args_template.join("\n"),
            input_mode: c.input_mode,
            response_path: c.response_path.clone().unwrap_or_default(),
            error_path: c.error_path.clone().unwrap_or_default(),
            model: c.model.clone(),
            env_text: c.env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n"),
            timeout_secs: c.timeout_secs.to_string(),
        }
    }
}

impl ExternalCommandDraft {
    fn to_config(&self) -> ExternalCommandConfig {
        ExternalCommandConfig {
            command: self.command.clone(),
            args_template: self.args_text.lines().map(str::to_string).filter(|s| !s.is_empty()).collect(),
            input_mode: self.input_mode,
            response_path: none_if_blank(&self.response_path),
            error_path: none_if_blank(&self.error_path),
            model: self.model.clone(),
            timeout_secs: self.timeout_secs.parse().unwrap_or(45),
            env: self
                .env_text
                .lines()
                .filter_map(|line| line.split_once('='))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect(),
        }
    }
}

fn none_if_blank(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

struct SettingsDraft {
    provider_kind: ProviderKind,
    local: LocalConfig,
    external: ExternalCommandDraft,
    enabled: bool,
    status: Option<(String, Instant)>,
}

impl SettingsDraft {
    fn from_config(config: &Config) -> Self {
        let mut draft = Self {
            provider_kind: ProviderKind::from_config(&config.provider),
            local: LocalConfig::default(),
            external: ExternalCommandDraft::from(&ExternalCommandConfig::default()),
            enabled: config.enabled,
            status: None,
        };
        match &config.provider {
            ProviderConfig::Local(c) => draft.local = c.clone(),
            ProviderConfig::ExternalCommand(c) => draft.external = ExternalCommandDraft::from(c),
        }
        draft
    }

    fn to_provider_config(&self) -> ProviderConfig {
        match self.provider_kind {
            ProviderKind::Local => ProviderConfig::Local(self.local.clone()),
            ProviderKind::ExternalCommand => ProviderConfig::ExternalCommand(self.external.to_config()),
        }
    }
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
        let draft = SettingsDraft::from_config(&config.lock().unwrap());
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
        theme::apply_visuals(ctx);

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
            .with_title("Alfred Writer (AW) — Settings")
            .with_inner_size([460.0, 680.0])
            .with_min_inner_size([380.0, 320.0])
            .with_resizable(true)
            .with_taskbar(false)
            .with_icon(egui::IconData {
                rgba: theme::badge_rgba(64),
                width: 64,
                height: 64,
            });
        if self.settings_open_request {
            builder = builder.with_position(egui::pos2(200.0, 200.0));
            self.settings_open_request = false;
        }

        let draft = &mut self.draft;
        let config = &self.config;

        ctx.show_viewport_immediate(id, builder, |ctx, _class| {
            egui::CentralPanel::default().show(ctx, |ui| {
                // The whole panel scrolls (rather than just the provider-fields block)
                // so nothing gets silently cut off if the window is resized smaller than
                // the current provider's field list needs.
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        theme::draw_badge(ui, 28.0);
                        ui.vertical(|ui| {
                            ui.heading(egui::RichText::new("Alfred Writer").strong().color(theme::SLATE));
                            ui.label(egui::RichText::new("AW").small().color(theme::MUTED));
                        });
                    });
                    ui.add_space(4.0);
                    ui.label("Grammar and style checking, system-wide, powered by a local model or an external command you control.");
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(8.0);

                    ui.label(egui::RichText::new("Provider").strong().color(theme::SLATE));
                    egui::ComboBox::from_id_source("provider_combo")
                        .selected_text(draft.provider_kind.label())
                        .show_ui(ui, |ui| {
                            for kind in ProviderKind::ALL {
                                ui.selectable_value(&mut draft.provider_kind, kind, kind.label());
                            }
                        });
                    ui.add_space(8.0);

                    egui::Frame::none()
                        .fill(theme::SURFACE_TINT)
                        .rounding(6.0)
                        .inner_margin(egui::Margin::same(10.0))
                        .show(ui, |ui| {
                            show_provider_fields(ui, draft);
                        });

                    ui.add_space(10.0);
                    ui.checkbox(&mut draft.enabled, egui::RichText::new("Enabled").strong());

                    ui.add_space(12.0);
                    let save_button = egui::Button::new(egui::RichText::new("Save").strong().color(egui::Color32::WHITE))
                        .fill(theme::MAGENTA);
                    if ui.add(save_button).clicked() {
                        let mut c = config.lock().unwrap();
                        c.provider = draft.to_provider_config();
                        c.enabled = draft.enabled;
                        let _ = c.save();
                        draft.status = Some(("Saved.".to_string(), Instant::now()));
                    }

                    if let Some((msg, at)) = &draft.status {
                        if at.elapsed().as_secs_f32() < 1.8 {
                            ui.horizontal(|ui| {
                                theme::draw_check_icon(ui, 13.0, theme::SAGE_TEXT);
                                ui.label(egui::RichText::new(msg).strong().color(theme::SAGE_TEXT));
                            });
                        } else {
                            draft.status = None;
                        }
                    }
                });
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
            PopupState::Loading { .. } => 44.0,
            PopupState::Error { .. } => 64.0,
            PopupState::Issues { issues, .. } => (issues.len() as f32 * 104.0).clamp(64.0, 320.0),
        };
        let window_height = 76.0 + body_height;

        let id = egui::ViewportId::from_hash_of("alfred-writer-popup");
        let builder = egui::ViewportBuilder::default()
            .with_title("Alfred Writer")
            .with_inner_size([320.0, window_height])
            .with_position(egui::pos2(rect.left, rect.bottom + 6.0))
            .with_decorations(false)
            .with_always_on_top()
            .with_resizable(false)
            .with_transparent(true)
            .with_taskbar(false);

        let mut close_clicked = false;
        let mut apply_click: Option<(usize, String, String)> = None;
        let mut dismiss_click: Option<usize> = None;

        ctx.show_viewport_immediate(id, builder, |ctx, _class| {
            egui::CentralPanel::default()
                .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::WHITE))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        theme::draw_badge(ui, 20.0);
                        ui.label(egui::RichText::new("Alfred Writer").strong().color(theme::SLATE));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::close_button(ui, 18.0).clicked() {
                                close_clicked = true;
                            }
                        });
                    });
                    ui.separator();

                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| match popup {
                        PopupState::Loading { .. } => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(egui::RichText::new("Checking your writing…").strong().color(theme::SLATE));
                            });
                        }
                        PopupState::Error { message, .. } => {
                            egui::Frame::none()
                                .fill(theme::DANGER_TINT)
                                .rounding(6.0)
                                .inner_margin(egui::Margin::same(8.0))
                                .show(ui, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(egui::RichText::new("Error").strong().color(theme::DANGER));
                                        ui.colored_label(theme::DANGER, message);
                                    });
                                });
                        }
                        PopupState::Issues { issues, .. } => {
                            for (i, issue) in issues.iter().enumerate() {
                                // Unique ID salt per row — without this, every issue's
                                // widgets (Apply/Dismiss buttons) share the same
                                // auto-generated egui ID (same call site each loop turn),
                                // which causes ID clashes and scrambles click routing.
                                ui.push_id(i, |ui| {
                                    egui::Frame::none()
                                        .fill(theme::SURFACE_TINT)
                                        .rounding(6.0)
                                        .inner_margin(egui::Margin::same(8.0))
                                        .show(ui, |ui| {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.colored_label(theme::DANGER, egui::RichText::new(&issue.original).strikethrough());
                                                theme::draw_arrow_icon(ui, 14.0, theme::MUTED);
                                                ui.colored_label(theme::SAGE_TEXT, egui::RichText::new(&issue.suggestion).strong());
                                            });

                                            if !issue.explanation.is_empty() {
                                                ui.colored_label(theme::MUTED, &issue.explanation);
                                            }
                                            ui.add_space(4.0);
                                            ui.horizontal(|ui| {
                                                theme::draw_check_icon(ui, 13.0, theme::SAGE_TEXT);
                                                let apply_button = egui::Button::new(
                                                    egui::RichText::new("Apply").strong().color(egui::Color32::WHITE),
                                                )
                                                .fill(theme::SAGE_TEXT);
                                                if ui.add(apply_button).clicked() {
                                                    apply_click = Some((i, issue.original.clone(), issue.suggestion.clone()));
                                                }
                                                ui.add_space(4.0);
                                                theme::draw_cross_icon(ui, 13.0, theme::MUTED);
                                                if ui.button(egui::RichText::new("Dismiss").color(theme::MUTED)).clicked() {
                                                    dismiss_click = Some(i);
                                                }
                                            });
                                        });
                                    ui.add_space(6.0);
                                });
                            }
                        }
                    });

                    ui.separator();
                    ui.small(egui::RichText::new("Powered by your configured provider · double-check important text").color(theme::MUTED));
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

/// Renders the fields specific to whichever provider is currently selected in the
/// Settings dropdown. Each provider keeps its own draft struct so switching the dropdown
/// mid-edit doesn't lose anything already typed into the others.
fn show_provider_fields(ui: &mut egui::Ui, draft: &mut SettingsDraft) {
    match draft.provider_kind {
        ProviderKind::Local => {
            ui.label("Base URL");
            ui.text_edit_singleline(&mut draft.local.base_url);
            ui.small("An OpenAI-compatible endpoint, e.g. Ollama's http://localhost:11434/v1 or LM Studio's http://localhost:1234/v1.");
            ui.label("Model");
            ui.text_edit_singleline(&mut draft.local.model);
        }
        ProviderKind::ExternalCommand => {
            ui.label("Command");
            ui.text_edit_singleline(&mut draft.external.command);
            ui.label("Arguments (one per line; supports {model} {system_prompt} {schema} {prompt})");
            egui::ScrollArea::vertical().id_source("args_scroll").max_height(110.0).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut draft.external.args_text).desired_rows(6).desired_width(f32::INFINITY));
            });

            ui.horizontal(|ui| {
                ui.label("Input mode:");
                ui.selectable_value(&mut draft.external.input_mode, InputMode::Args, "Args");
                ui.selectable_value(&mut draft.external.input_mode, InputMode::Stdin, "Stdin");
            });

            ui.label("Response path (optional; dot-path to the issue list in stdout JSON)");
            ui.text_edit_singleline(&mut draft.external.response_path);
            ui.label("Error path (optional; dot-path to a boolean error flag in stdout JSON)");
            ui.text_edit_singleline(&mut draft.external.error_path);

            ui.horizontal(|ui| {
                ui.label("Model:");
                ui.text_edit_singleline(&mut draft.external.model);
            });
            ui.horizontal(|ui| {
                ui.label("Timeout (seconds):");
                ui.text_edit_singleline(&mut draft.external.timeout_secs);
            });

            ui.label("Environment variables (one KEY=VALUE per line, optional)");
            ui.add(egui::TextEdit::multiline(&mut draft.external.env_text).desired_rows(2).desired_width(f32::INFINITY));

            ui.add_space(6.0);
            ui.small("Authentication is entirely up to the command itself. The default runs the Claude Code CLI, reusing your existing `claude` login — no API key needed here.");
        }
    }
}
