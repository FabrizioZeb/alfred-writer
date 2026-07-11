//! Alfred Writer's color palette and small shared UI-drawing helpers, used by both the
//! egui windows (`app.rs`) and the raw-pixel tray icon (`tray.rs`) so the two stay in
//! sync instead of each hardcoding its own colors.
//!
//! The four named colors are the brand palette as given; everything else here is a tint
//! or shade derived from them for use as backgrounds, secondary text, and the one accent
//! (danger/error) the given palette didn't include.

use eframe::egui::{Color32, Context, Visuals};
use std::io::Cursor;
use std::sync::OnceLock;

/// Primary accent: headings, primary actions (Save, Apply-adjacent emphasis). Not used
/// for the badge itself — see [`badge_rgba`]/[`draw_badge`], which use a green instead.
pub const MAGENTA: Color32 = Color32::from_rgb(0xCB, 0x05, 0xBB);
/// Dark neutral: body text, borders, anything that needs to read as "ink".
pub const SLATE: Color32 = Color32::from_rgb(0x47, 0x58, 0x5C);
/// Positive/success accent: suggestions, confirmations, the Apply action. Pale by
/// design — meant as a background/highlight tint, not as text (see [`SAGE_TEXT`] for
/// that: `SAGE` itself fails contrast on a white background).
pub const SAGE: Color32 = Color32::from_rgb(0xCB, 0xD5, 0xBB);
/// A shade of `SAGE` dark enough to read as text on a white/light background, for
/// "success"-toned labels (a suggested fix, a saved confirmation).
pub const SAGE_TEXT: Color32 = Color32::from_rgb(0x4B, 0x6B, 0x3A);

/// Warm red for errors/removed text — not in the given palette (which has no error
/// color), chosen to sit between magenta and slate in warmth so it reads as "alert"
/// without clashing with the rest of the UI.
pub const DANGER: Color32 = Color32::from_rgb(0xC1, 0x39, 0x2B);
/// Lightened `SLATE`, for secondary/help text that shouldn't compete with body text.
pub const MUTED: Color32 = Color32::from_rgb(0x86, 0x97, 0x9B);
/// Light tint of `DANGER`, for error-message card backgrounds (mirrors `SURFACE_TINT`
/// and `MAGENTA_TINT` — a background tint needs to be its own constant, not derived by
/// multiplying `DANGER` at render time, since `Color32::linear_multiply` darkens toward
/// black rather than lightening toward white).
pub const DANGER_TINT: Color32 = Color32::from_rgb(0xF7, 0xE1, 0xDE);
/// Near-white, very lightly sage-tinted surface for card/panel backgrounds — keeps
/// grouped content visually distinct without resorting to flat white or grey.
pub const SURFACE_TINT: Color32 = Color32::from_rgb(0xEF, 0xF3, 0xEA);
/// Light magenta tint for header bars / selected-state backgrounds.
pub const MAGENTA_TINT: Color32 = Color32::from_rgb(0xF6, 0xE3, 0xF4);

/// The AW badge (green fill, slate ring, white "AW" monogram), pre-rendered at build
/// time into `assets/icon.ico` by `scripts/generate-icon.ps1` — see that script for why:
/// GDI+'s text rasterizer draws small bold text far better than anything reasonable to
/// hand-roll in Rust, so the badge is designed once there and read back here as pixels,
/// rather than re-implemented as a second, drifting copy.
static ICON_ICO_BYTES: &[u8] = include_bytes!("../assets/icon.ico");

fn icon_dir() -> &'static ico::IconDir {
    static DIR: OnceLock<ico::IconDir> = OnceLock::new();
    DIR.get_or_init(|| {
        ico::IconDir::read(Cursor::new(ICON_ICO_BYTES))
            .expect("assets/icon.ico is malformed — regenerate it with scripts/generate-icon.ps1")
    })
}

/// Returns the AW badge as a raw RGBA pixel buffer at exactly `size` x `size`, for
/// contexts that need actual pixels rather than an egui painter — the system tray icon
/// and OS window icons.
///
/// `size` must be one of the resolutions baked into `assets/icon.ico` (currently 16, 32,
/// 48, 64, 256); this panics rather than silently returning a mismatched buffer if asked
/// for anything else, since every call site here picks its size from that fixed set.
pub fn badge_rgba(size: u32) -> Vec<u8> {
    let dir = icon_dir();
    let entry = dir
        .entries()
        .iter()
        .find(|e| e.width() == size)
        .unwrap_or_else(|| panic!("assets/icon.ico has no {size}x{size} frame — add it in scripts/generate-icon.ps1"));
    let image = entry.decode().expect("embedded AW badge frame should decode cleanly");
    image.rgba_data().to_vec()
}

/// Draws the AW monogram badge (green fill, slate ring, white "AW" mark) used as Alfred
/// Writer's compact identity in every in-app window header. Drawn directly with egui's
/// painter (rather than via [`badge_rgba`]) since this version needs to scale smoothly
/// with the UI and doesn't need to match a fixed pixel grid the way an OS icon does.
pub fn draw_badge(ui: &mut eframe::egui::Ui, diameter: f32) {
    use eframe::egui::{FontId, Sense, Stroke};

    let (rect, _response) = ui.allocate_exact_size(eframe::egui::vec2(diameter, diameter), Sense::hover());
    let painter = ui.painter();
    let center = rect.center();
    let radius = diameter / 2.0;

    painter.circle_filled(center, radius, SAGE_TEXT);
    painter.circle_stroke(center, radius - 0.75, Stroke::new(1.5_f32, SLATE));
    painter.text(
        center,
        eframe::egui::Align2::CENTER_CENTER,
        "AW",
        FontId::proportional(diameter * 0.42),
        Color32::WHITE,
    );
}

/// Forces a consistent light theme across every widget (buttons, text edits, combo
/// boxes, checkboxes — not just the labels we color explicitly), built on our palette.
///
/// Without this, widgets we don't touch directly (a multiline `TextEdit`, a
/// `ComboBox`'s popup) fall back to egui's default dark-mode widget colors while panels
/// we've explicitly filled stay light, producing a jarring, inconsistent mix of light
/// and dark surfaces in the same window. Call once per frame before drawing.
pub fn apply_visuals(ctx: &Context) {
    let mut visuals = Visuals::light();
    visuals.override_text_color = Some(SLATE);
    visuals.panel_fill = Color32::WHITE;
    visuals.extreme_bg_color = Color32::WHITE; // TextEdit / ScrollArea backgrounds
    visuals.faint_bg_color = SURFACE_TINT;
    visuals.selection.bg_fill = MAGENTA_TINT;
    visuals.selection.stroke.color = MAGENTA;
    visuals.hyperlink_color = MAGENTA;

    visuals.widgets.inactive.bg_fill = Color32::from_rgb(0xF1, 0xF1, 0xEE);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(0xF1, 0xF1, 0xEE);
    visuals.widgets.hovered.bg_fill = MAGENTA_TINT;
    visuals.widgets.hovered.weak_bg_fill = MAGENTA_TINT;
    visuals.widgets.active.bg_fill = MAGENTA;
    visuals.widgets.active.weak_bg_fill = MAGENTA;

    ctx.set_visuals(visuals);
}

// The functional icons below (check, cross, arrow) are hand-drawn line strokes rather
// than Unicode glyphs (✓ ✕ →). egui's bundled default font doesn't include those symbols
// — they rendered as tofu boxes when tried — so drawing them ourselves is what actually
// guarantees they show up, regardless of font coverage.

fn paint_check(painter: &eframe::egui::Painter, rect: eframe::egui::Rect, color: Color32) {
    use eframe::egui::{pos2, Stroke};
    let size = rect.width();
    let stroke = Stroke::new((size * 0.16).max(1.5), color);
    let a = pos2(rect.left() + size * 0.15, rect.top() + size * 0.55);
    let b = pos2(rect.left() + size * 0.42, rect.bottom() - size * 0.18);
    let c = pos2(rect.right() - size * 0.12, rect.top() + size * 0.2);
    painter.line_segment([a, b], stroke);
    painter.line_segment([b, c], stroke);
}

fn paint_cross(painter: &eframe::egui::Painter, rect: eframe::egui::Rect, color: Color32) {
    use eframe::egui::{pos2, Stroke};
    let size = rect.width();
    let stroke = Stroke::new((size * 0.14).max(1.5), color);
    let pad = size * 0.22;
    painter.line_segment([pos2(rect.left() + pad, rect.top() + pad), pos2(rect.right() - pad, rect.bottom() - pad)], stroke);
    painter.line_segment([pos2(rect.right() - pad, rect.top() + pad), pos2(rect.left() + pad, rect.bottom() - pad)], stroke);
}

fn paint_arrow(painter: &eframe::egui::Painter, rect: eframe::egui::Rect, color: Color32) {
    use eframe::egui::{pos2, Stroke};
    let size = rect.width();
    let stroke = Stroke::new((size * 0.14).max(1.5), color);
    let mid_y = rect.center().y;
    painter.line_segment([pos2(rect.left(), mid_y), pos2(rect.right() - size * 0.18, mid_y)], stroke);
    painter.line_segment([pos2(rect.right() - size * 0.18, mid_y), pos2(rect.right() - size * 0.45, mid_y - size * 0.3)], stroke);
    painter.line_segment([pos2(rect.right() - size * 0.18, mid_y), pos2(rect.right() - size * 0.45, mid_y + size * 0.3)], stroke);
}

/// Decorative (non-interactive) checkmark icon, e.g. before "Apply" or a saved-status line.
pub fn draw_check_icon(ui: &mut eframe::egui::Ui, size: f32, color: Color32) {
    let (rect, _response) = ui.allocate_exact_size(eframe::egui::vec2(size, size), eframe::egui::Sense::hover());
    paint_check(ui.painter(), rect, color);
}

/// Decorative (non-interactive) X icon, e.g. before "Dismiss".
pub fn draw_cross_icon(ui: &mut eframe::egui::Ui, size: f32, color: Color32) {
    let (rect, _response) = ui.allocate_exact_size(eframe::egui::vec2(size, size), eframe::egui::Sense::hover());
    paint_cross(ui.painter(), rect, color);
}

/// Decorative (non-interactive) right-pointing arrow, e.g. between an original and
/// corrected phrase.
pub fn draw_arrow_icon(ui: &mut eframe::egui::Ui, size: f32, color: Color32) {
    let (rect, _response) = ui.allocate_exact_size(eframe::egui::vec2(size, size), eframe::egui::Sense::hover());
    paint_arrow(ui.painter(), rect, color);
}

/// A small clickable X button (window-close affordance), drawn rather than relying on a
/// Unicode ✕ glyph. Returns the click response so the caller decides what closing means.
pub fn close_button(ui: &mut eframe::egui::Ui, size: f32) -> eframe::egui::Response {
    let (rect, response) = ui.allocate_exact_size(eframe::egui::vec2(size, size), eframe::egui::Sense::click());
    let color = if response.hovered() { DANGER } else { MUTED };
    if response.hovered() {
        ui.painter().rect_filled(rect, 3.0, SURFACE_TINT);
    }
    paint_cross(ui.painter(), rect.shrink(size * 0.2), color);
    response
}
