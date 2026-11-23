//! Visual style.
//!
//! Neutral, dark chrome so the only colour on screen is the artwork; a
//! single vermilion accent marks what is active. Interface greys are
//! deliberately desaturated so they don't bias colour judgement, the same
//! reason painters use grey palettes and studios grey walls.

use eframe::egui::{self, Color32, CornerRadius, Stroke, Visuals};

pub const ACCENT: Color32 = Color32::from_rgb(232, 98, 64);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(96, 52, 42);
pub const BG_CANVAS: Color32 = Color32::from_rgb(40, 40, 40);
pub const BG_PANEL: Color32 = Color32::from_rgb(30, 30, 30);
pub const BG_WIDGET: Color32 = Color32::from_rgb(46, 46, 46);
pub const TEXT: Color32 = Color32::from_rgb(214, 214, 214);
pub const TEXT_DIM: Color32 = Color32::from_rgb(140, 140, 140);
pub const WARN: Color32 = Color32::from_rgb(240, 180, 90);

pub fn apply(ctx: &egui::Context) {
    let mut v = Visuals::dark();
    v.panel_fill = BG_PANEL;
    v.window_fill = Color32::from_rgb(36, 36, 36);
    v.extreme_bg_color = Color32::from_rgb(22, 22, 22);
    v.faint_bg_color = Color32::from_rgb(34, 34, 34);
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.window_stroke = Stroke::new(1.0, Color32::from_gray(60));
    v.slider_trailing_fill = true;
    let r = CornerRadius::same(4);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_gray(44));
    v.widgets.inactive.weak_bg_fill = BG_WIDGET;
    v.widgets.inactive.bg_fill = BG_WIDGET;
    v.widgets.hovered.weak_bg_fill = Color32::from_gray(58);
    v.widgets.hovered.bg_fill = Color32::from_gray(58);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_gray(80));
    v.widgets.active.weak_bg_fill = Color32::from_gray(66);
    v.widgets.active.bg_fill = Color32::from_gray(66);
    ctx.set_visuals(v);

    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(6.0, 5.0);
        s.spacing.button_padding = egui::vec2(7.0, 3.0);
        s.spacing.slider_width = 150.0;
        s.spacing.interact_size.y = 20.0;
    });
}
