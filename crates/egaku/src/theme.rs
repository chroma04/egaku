//! Visual style.
//!
//! Neutral, dark chrome so the only colour on screen is the artwork; one
//! vermilion accent (朱) marks what is active. Interface greys are
//! deliberately desaturated so they don't bias colour judgement, the same
//! reason painters use grey palettes and studios grey walls. Text is set in
//! Inter; icons are Phosphor.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow,
    Stroke, TextStyle, Visuals,
};

pub const ACCENT: Color32 = Color32::from_rgb(236, 104, 70);
/// The accent at low strength over a panel, for selected backgrounds.
pub const ACCENT_BG: Color32 = Color32::from_rgb(72, 44, 38);
pub const BG_APP: Color32 = Color32::from_rgb(24, 24, 26);
pub const BG_PANEL: Color32 = Color32::from_rgb(30, 30, 33);
pub const BG_RAISED: Color32 = Color32::from_rgb(40, 40, 44);
pub const BG_HOVER: Color32 = Color32::from_rgb(50, 50, 55);
pub const BG_ACTIVE: Color32 = Color32::from_rgb(60, 60, 66);
pub const BG_CANVAS: Color32 = Color32::from_rgb(38, 38, 41);
pub const LINE: Color32 = Color32::from_rgb(44, 44, 48);
pub const TEXT: Color32 = Color32::from_rgb(228, 228, 232);
pub const TEXT_DIM: Color32 = Color32::from_rgb(150, 150, 158);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(100, 100, 108);
pub const WARN: Color32 = Color32::from_rgb(244, 184, 96);
/// Height of a standard control.
pub const ROW: f32 = 24.0;

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("icons".into()))
}

fn fonts() -> FontDefinitions {
    let mut f = FontDefinitions::default();
    f.font_data.insert(
        "inter".into(),
        FontData::from_static(include_bytes!("../assets/fonts/Inter-Regular.otf")).into(),
    );
    f.font_data.insert(
        "inter-semibold".into(),
        FontData::from_static(include_bytes!("../assets/fonts/Inter-SemiBold.otf")).into(),
    );
    let prop = f.families.entry(FontFamily::Proportional).or_default();
    prop.insert(0, "inter".into());
    // Icons get a family of their own: Inter has glyphs in the private-use
    // area too, which would otherwise shadow Phosphor's.
    f.font_data.insert(
        "phosphor".into(),
        FontData::from_static(egui_phosphor::Variant::Regular.font_bytes()).into(),
    );
    f.families
        .insert(FontFamily::Name("icons".into()), vec!["phosphor".into()]);
    // SemiBold falls back to the same chain (icons, emoji) as body text.
    let mut chain = f.families[&FontFamily::Proportional].clone();
    chain[0] = "inter-semibold".into();
    f.families
        .insert(FontFamily::Name("semibold".into()), chain);
    f
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());

    let mut v = Visuals::dark();
    v.panel_fill = BG_PANEL;
    v.window_fill = Color32::from_rgb(34, 34, 38);
    v.extreme_bg_color = Color32::from_rgb(22, 22, 24);
    v.faint_bg_color = Color32::from_rgb(36, 36, 40);
    v.code_bg_color = BG_RAISED;
    v.override_text_color = Some(TEXT);
    v.weak_text_color = Some(TEXT_DIM);
    v.selection.bg_fill = ACCENT_BG;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(56, 56, 62));
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 32,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(90),
    };
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.striped = false;
    let r = CornerRadius::same(6);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = r;
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    v.widgets.inactive.weak_bg_fill = BG_RAISED;
    v.widgets.inactive.bg_fill = BG_RAISED;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.hovered.weak_bg_fill = BG_HOVER;
    v.widgets.hovered.bg_fill = BG_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    v.widgets.active.weak_bg_fill = BG_ACTIVE;
    v.widgets.active.bg_fill = BG_ACTIVE;
    v.widgets.active.bg_stroke = Stroke::NONE;
    v.widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    v.widgets.open.weak_bg_fill = BG_HOVER;
    v.widgets.open.bg_fill = BG_HOVER;
    ctx.set_visuals(v);

    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(6.0, 6.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        s.spacing.slider_width = 120.0;
        s.spacing.slider_rail_height = 4.0;
        s.spacing.interact_size = egui::vec2(28.0, ROW);
        s.spacing.menu_margin = Margin::same(6);
        s.spacing.window_margin = Margin::same(18);
        s.spacing.combo_width = 120.0;
        s.spacing.icon_width = 14.0;
        s.spacing.icon_width_inner = 8.0;
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(11.0)),
            (TextStyle::Body, FontId::proportional(13.0)),
            (TextStyle::Button, FontId::proportional(13.0)),
            (TextStyle::Heading, semibold(17.0)),
            (TextStyle::Monospace, FontId::monospace(12.0)),
        ]
        .into();
        s.interaction.tooltip_delay = 0.35;
        s.interaction.selectable_labels = false;
    });
}
