use crate::automation::{place_popup, AutomationCmd, Rect, UiEvent};
use crate::config::Config;
use crate::providers::{ExternalCommandConfig, InputMode, Issue, LocalConfig, ProviderConfig};
use crate::theme;
use crate::tray::TrayEvent;
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Title of the suggestion popup/indicator window. Never user-visible (decorations are
/// off and it's excluded from the taskbar) — it exists so [`apply_noactivate`] can find
/// this specific HWND by exact title without ever matching the hidden root window or the
/// Settings window, which both start with "Alfred Writer" too.
const POPUP_TITLE: &str = "Alfred Writer — Suggestions";

/// Stamps `WS_EX_NOACTIVATE` onto the popup window so it never takes keyboard focus:
/// the user keeps typing in their field while hovering, scrolling, or clicking Apply.
/// Mouse input still arrives normally — only *activation* is suppressed.
///
/// Called every frame the popup is shown (cheap: one FindWindowW + a read), not just
/// once, because egui recreates the OS window whenever the viewport builder changes
/// (e.g. the popup resizes between indicator/loading/issues states), which would drop a
/// style applied only at first creation.
fn apply_noactivate(title: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE,
    };
    unsafe {
        let Ok(hwnd) = FindWindowW(None, &HSTRING::from(title)) else {
            return;
        };
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let no_activate = WS_EX_NOACTIVATE.0 as isize;
        if ex & no_activate == 0 {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | no_activate);
        }
    }
}

#[derive(Clone)]
enum PopupState {
    /// `spans[i]` = on-screen rects of `issues[i]`'s flagged text (empty when the span
    /// couldn't be located), used to draw clickable underlines beneath the text itself.
    Issues { rect: Rect, issues: Vec<Issue>, spans: Vec<Vec<Rect>> },
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
    /// One executable name per line (case-insensitive, `.exe` optional) — apps where
    /// checking is disabled entirely.
    blacklist_text: String,
    status: Option<(String, Instant)>,
}

impl SettingsDraft {
    fn from_config(config: &Config) -> Self {
        let mut draft = Self {
            provider_kind: ProviderKind::from_config(&config.provider),
            local: LocalConfig::default(),
            external: ExternalCommandDraft::from(&ExternalCommandConfig::default()),
            enabled: config.enabled,
            blacklist_text: config.blacklist.join("\n"),
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
    /// Local-provider mode only: issues arrived but the popup is showing as a small
    /// count badge near the caret instead of the full suggestion list. Hovering or
    /// clicking the badge expands it (Grammarly-style: analysis is decoupled from
    /// presentation, so frequent cheap local checks don't shove a full popup at the
    /// user every couple of seconds).
    indicator_collapsed: bool,
    /// When set, the expanded popup shows only this issue (index into the current
    /// `PopupState::Issues` list) — the state after clicking that issue's underline.
    selected_issue: Option<usize>,
    /// One-shot: (re)pin the popup window's position on the next frame it's shown.
    /// Cleared after applying so the user can drag the popup wherever they like without
    /// it snapping back; set again whenever the anchor meaningfully changes (new result,
    /// underline click, badge expand, error).
    reposition_popup: bool,
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
            indicator_collapsed: false,
            selected_issue: None,
            reposition_popup: false,
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
                UiEvent::Hide => {
                    self.popup = None;
                    self.selected_issue = None;
                }
                // Analysis is invisible by design (Grammarly-style strict separation of
                // analysis from presentation): no "checking…" popup while the user
                // types. A result only ever surfaces as underlines (or the badge).
                UiEvent::Loading { .. } => {}
                UiEvent::Issues { rect, issues, spans } => {
                    if issues.is_empty() {
                        self.popup = None;
                        self.selected_issue = None;
                    } else {
                        self.indicator_collapsed = true;
                        self.selected_issue = None;
                        self.reposition_popup = true;
                        self.popup = Some(PopupState::Issues { rect, issues, spans });
                    }
                }
                UiEvent::Error { rect, message } => {
                    self.reposition_popup = true;
                    self.popup = Some(PopupState::Error { rect, message });
                }
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

        // Underline strips stay visible for as long as issues exist (collapsed or
        // expanded); clicking one selects that issue and opens the popup at the click.
        if let Some(PopupState::Issues { spans, .. }) = self.popup.clone() {
            if let Some((idx, anchor)) = self.show_underline_strips(ctx, &spans) {
                self.indicator_collapsed = false;
                self.selected_issue = Some(idx);
                self.reposition_popup = true;
                if let Some(PopupState::Issues { rect, .. }) = &mut self.popup {
                    *rect = anchor;
                }
            }
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

                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("Disabled applications").strong().color(theme::SLATE));
                    ui.small("One program per line, as shown in Task Manager — e.g. keepass, 1Password.exe. Checking is fully off inside these apps. Takes effect on Save, no restart needed.");
                    ui.add(egui::TextEdit::multiline(&mut draft.blacklist_text).desired_rows(3).desired_width(f32::INFINITY));

                    ui.add_space(12.0);
                    let save_button = egui::Button::new(egui::RichText::new("Save").strong().color(egui::Color32::WHITE))
                        .fill(theme::MAGENTA);
                    if ui.add(save_button).clicked() {
                        let mut c = config.lock().unwrap();
                        c.provider = draft.to_provider_config();
                        c.enabled = draft.enabled;
                        c.blacklist = draft
                            .blacklist_text
                            .lines()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect();
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

    /// Draws one thin, always-on-top, non-activating strip right beneath each flagged
    /// span — the inline "underline" that marks where an issue is without covering any
    /// text or taking focus. Only the few pixels of the strip itself are clickable; the
    /// text above it still belongs entirely to the edited app.
    ///
    /// Returns `Some((issue_index, span_rect))` when a strip was clicked this frame.
    fn show_underline_strips(&mut self, ctx: &egui::Context, spans: &[Vec<Rect>]) -> Option<(usize, Rect)> {
        // Bound the number of OS windows a pathological result can spawn.
        const MAX_STRIPS: usize = 12;
        const STRIP_HEIGHT: f32 = 4.0;

        let mut clicked = None;
        let mut shown = 0usize;
        'outer: for (issue_idx, rects) in spans.iter().enumerate() {
            for (line_idx, r) in rects.iter().enumerate() {
                if shown == MAX_STRIPS {
                    break 'outer;
                }
                shown += 1;
                let width = (r.right - r.left).max(10.0);
                // Unique title per strip so apply_noactivate can find each HWND.
                let title = format!("AW underline {issue_idx}.{line_idx}");
                let id = egui::ViewportId::from_hash_of(("aw-underline", issue_idx, line_idx));
                let builder = egui::ViewportBuilder::default()
                    .with_title(&title)
                    .with_inner_size([width, STRIP_HEIGHT])
                    .with_position(egui::pos2(r.left, r.bottom))
                    .with_decorations(false)
                    .with_always_on_top()
                    .with_resizable(false)
                    .with_transparent(true)
                    .with_active(false)
                    .with_taskbar(false);

                let mut hit = false;
                ctx.show_viewport_immediate(id, builder, |ctx, _class| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::none().fill(theme::DANGER))
                        .show(ctx, |ui| {
                            let response = ui.interact(ui.max_rect(), ui.id().with("strip"), egui::Sense::click());
                            if response.clicked() {
                                hit = true;
                            }
                        });
                });
                apply_noactivate(&title);
                if hit {
                    clicked = Some((issue_idx, *r));
                }
            }
        }
        clicked
    }

    /// Collapsed form of the issues popup: a small always-on-top count badge near the
    /// caret. Hovering or clicking it expands into the full popup. Reuses the popup's
    /// viewport id so expanding is a resize of the same OS window, not a new one.
    fn show_indicator(&mut self, ctx: &egui::Context, rect: Rect, count: usize) {
        let id = egui::ViewportId::from_hash_of("alfred-writer-popup");
        let (px, py) = place_popup(&rect, 52.0, 32.0);
        let builder = egui::ViewportBuilder::default()
            .with_title(POPUP_TITLE)
            .with_inner_size([52.0, 32.0])
            .with_position(egui::pos2(px, py))
            .with_decorations(false)
            .with_always_on_top()
            .with_resizable(false)
            .with_transparent(true)
            .with_active(false)
            .with_taskbar(false);

        let mut expand = false;
        ctx.show_viewport_immediate(id, builder, |ctx, _class| {
            egui::CentralPanel::default()
                .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::WHITE))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        theme::draw_badge(ui, 16.0);
                        ui.label(egui::RichText::new(count.to_string()).strong().color(theme::SLATE));
                    });
                    let response = ui.interact(ui.max_rect(), ui.id().with("indicator"), egui::Sense::click());
                    if response.hovered() || response.clicked() {
                        expand = true;
                    }
                });
        });
        apply_noactivate(POPUP_TITLE);
        if expand {
            self.indicator_collapsed = false;
            self.reposition_popup = true;
        }
    }

    fn show_popup_window(&mut self, ctx: &egui::Context, popup: &PopupState) {
        if self.indicator_collapsed {
            if let PopupState::Issues { rect, issues, spans } = popup {
                // Underlines are the indicator whenever at least one span resolved to a
                // screen position; the count badge is only the fallback for controls
                // where no span could be located.
                if spans.iter().all(|s| s.is_empty()) {
                    self.show_indicator(ctx, *rect, issues.len());
                }
                return;
            }
        }

        let rect = match popup {
            PopupState::Issues { rect, .. } => *rect,
            PopupState::Error { rect, .. } => *rect,
        };

        // Which issues this popup shows: just the one whose underline was clicked, or
        // all of them (badge expand / "show all").
        let display: Vec<(usize, &Issue)> = match (popup, self.selected_issue) {
            (PopupState::Issues { issues, .. }, Some(sel)) if sel < issues.len() => {
                vec![(sel, &issues[sel])]
            }
            (PopupState::Issues { issues, .. }, _) => issues.iter().enumerate().collect(),
            _ => Vec::new(),
        };

        let body_height: f32 = match popup {
            PopupState::Error { .. } => 64.0,
            PopupState::Issues { .. } => (display.len() as f32 * 104.0).clamp(64.0, 320.0),
        };
        let window_height = 76.0 + body_height;

        let id = egui::ViewportId::from_hash_of("alfred-writer-popup");
        let mut builder = egui::ViewportBuilder::default()
            .with_title(POPUP_TITLE)
            .with_inner_size([320.0, window_height])
            .with_decorations(false)
            .with_always_on_top()
            .with_resizable(false)
            .with_transparent(true)
            .with_active(false)
            .with_taskbar(false);
        // Pin the position only when the anchor changed; otherwise leave the window
        // wherever it is so a user drag isn't snapped back next frame.
        if self.reposition_popup {
            let (px, py) = place_popup(&rect, 320.0, window_height);
            builder = builder.with_position(egui::pos2(px, py));
            self.reposition_popup = false;
        }

        let mut close_clicked = false;
        let mut apply_click: Option<(usize, String, String)> = None;
        let mut dismiss_click: Option<usize> = None;
        let mut show_all_clicked = false;

        ctx.show_viewport_immediate(id, builder, |ctx, _class| {
            egui::CentralPanel::default()
                .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::WHITE))
                .show(ctx, |ui| {
                    // The header doubles as the drag handle (the window has no OS title
                    // bar) — grab it to move the popup anywhere; it stays put because
                    // position is only re-pinned when the anchor changes.
                    let header_response = ui
                        .horizontal(|ui| {
                            theme::draw_badge(ui, 20.0);
                            ui.label(egui::RichText::new("Alfred Writer").strong().color(theme::SLATE));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if theme::close_button(ui, 18.0).clicked() {
                                    close_clicked = true;
                                }
                            });
                        })
                        .response
                        .interact(egui::Sense::drag());
                    if header_response.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    ui.separator();

                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| match popup {
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
                            for &(i, issue) in &display {
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
                            // Single-issue view (opened from an underline): offer the
                            // rest without making the user hunt for other underlines.
                            if display.len() == 1 && issues.len() > 1 {
                                let label = format!("Show all {} suggestions", issues.len());
                                if ui.button(egui::RichText::new(label).color(theme::MUTED)).clicked() {
                                    show_all_clicked = true;
                                }
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
        apply_noactivate(POPUP_TITLE);

        if let Some((idx, original, suggestion)) = apply_click {
            let _ = self.automation_cmd_tx.send(AutomationCmd::Apply { original, suggestion });
            self.remove_issue(idx);
        } else if let Some(idx) = dismiss_click {
            self.remove_issue(idx);
        } else if show_all_clicked {
            self.selected_issue = None;
        } else if close_clicked {
            self.popup = None;
            self.selected_issue = None;
        }
    }

    /// Drops issue `idx` (and its underline spans) after Apply/Dismiss, closing the
    /// popup entirely when it was the last one.
    fn remove_issue(&mut self, idx: usize) {
        self.selected_issue = None;
        if let Some(PopupState::Issues { mut issues, mut spans, rect }) = self.popup.take() {
            if idx < issues.len() {
                issues.remove(idx);
            }
            if idx < spans.len() {
                spans.remove(idx);
            }
            self.popup = if issues.is_empty() {
                None
            } else {
                Some(PopupState::Issues { rect, issues, spans })
            };
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
