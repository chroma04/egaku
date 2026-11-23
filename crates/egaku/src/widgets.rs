//! Small custom widgets shared by the panels.

use eframe::egui::{
    self, Align2, Color32, CursorIcon, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2,
    pos2, vec2,
};

use crate::theme;

/// An icon followed by a label, for buttons and menu items.
pub fn il(icon: &str, text: &str) -> egui::WidgetText {
    let mut job = egui::text::LayoutJob::default();
    let fmt = |font| egui::TextFormat {
        font_id: font,
        color: Color32::PLACEHOLDER,
        valign: egui::Align::Center,
        ..Default::default()
    };
    job.append(icon, 0.0, fmt(theme::icon_font(14.0)));
    job.append(text, 6.0, fmt(egui::FontId::proportional(13.0)));
    job.into()
}

/// A label followed by a trailing icon (dropdown buttons).
pub fn li(text: &str, icon: &str) -> egui::WidgetText {
    let mut job = egui::text::LayoutJob::default();
    let fmt = |font| egui::TextFormat {
        font_id: font,
        color: Color32::PLACEHOLDER,
        valign: egui::Align::Center,
        ..Default::default()
    };
    job.append(text, 0.0, fmt(egui::FontId::proportional(13.0)));
    job.append(icon, 8.0, fmt(theme::icon_font(11.0)));
    job.into()
}

/// Just an icon, as widget text.
pub fn ic(icon: &str) -> egui::WidgetText {
    egui::RichText::new(icon).font(theme::icon_font(14.0)).into()
}

/// A square, frameless icon button.
pub fn icon_button(ui: &mut Ui, icon: &str, size: f32, selected: bool, tip: &str) -> Response {
    icon_button_sized(ui, icon, vec2(size, size), selected, tip)
}

pub fn icon_button_sized(ui: &mut Ui, icon: &str, size: Vec2, selected: bool, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    let bg = if selected {
        theme::ACCENT_BG
    } else if resp.is_pointer_button_down_on() {
        theme::BG_ACTIVE
    } else if resp.hovered() && enabled {
        theme::BG_HOVER
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 6.0, bg);
    let fg = if !enabled {
        theme::TEXT_FAINT
    } else if selected {
        theme::ACCENT
    } else if resp.hovered() {
        Color32::WHITE
    } else {
        theme::TEXT_DIM
    };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon,
        theme::icon_font((size.y * 0.58).round()),
        fg,
    );
    if tip.is_empty() { resp } else { resp.on_hover_text(tip) }
}

/// A row of mutually exclusive icon buttons in a shared pill.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, opts: &[(T, &str, &str)]) -> bool {
    let mut changed = false;
    let h = theme::ROW;
    let w = h + 4.0;
    let (rect, _) = ui.allocate_exact_size(vec2(w * opts.len() as f32, h), Sense::hover());
    ui.painter().rect_filled(rect, 6.0, theme::BG_RAISED);
    for (i, (v, icon, tip)) in opts.iter().enumerate() {
        let r = Rect::from_min_size(rect.min + vec2(i as f32 * w, 0.0), vec2(w, h));
        let resp = ui.interact(r, ui.scope_id().with(("seg", i, *tip)), Sense::click());
        let sel = *value == *v;
        if sel {
            ui.painter().rect_filled(r.shrink(2.0), 4.0, theme::BG_ACTIVE);
        } else if resp.hovered() {
            ui.painter().rect_filled(r.shrink(2.0), 4.0, theme::BG_HOVER);
        }
        let label_is_icon = icon.chars().count() == 1;
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            icon,
            if label_is_icon {
                theme::icon_font(15.0)
            } else {
                egui::FontId::proportional(12.0)
            },
            if sel { Color32::WHITE } else { theme::TEXT_DIM },
        );
        if resp.on_hover_text(*tip).clicked() && !sel {
            *value = *v;
            changed = true;
        }
    }
    changed
}

/// A two-state chip with an icon (and optional text).
pub fn toggle(ui: &mut Ui, on: &mut bool, icon: &str, text: &str, tip: &str) -> Response {
    let font = egui::FontId::proportional(12.5);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font.clone(), theme::TEXT);
    let w = if text.is_empty() {
        theme::ROW + 2.0
    } else {
        galley.size().x + 34.0
    };
    let (rect, mut resp) = ui.allocate_exact_size(vec2(w, theme::ROW), Sense::click());
    let bg = if *on {
        theme::ACCENT_BG
    } else if resp.hovered() {
        theme::BG_HOVER
    } else {
        theme::BG_RAISED
    };
    ui.painter().rect_filled(rect, 6.0, bg);
    let fg = if *on { theme::ACCENT } else { theme::TEXT_DIM };
    let ix = if text.is_empty() {
        rect.center().x
    } else {
        rect.left() + 14.0
    };
    ui.painter().text(
        pos2(ix, rect.center().y),
        Align2::CENTER_CENTER,
        icon,
        theme::icon_font(15.0),
        fg,
    );
    if !text.is_empty() {
        ui.painter().text(
            pos2(rect.left() + 26.0, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            font,
            if *on { theme::TEXT } else { theme::TEXT_DIM },
        );
    }
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    if tip.is_empty() { resp } else { resp.on_hover_text(tip) }
}

/// How a field slider maps its value to the bar.
#[derive(Copy, Clone)]
pub enum Scale {
    Linear,
    Log,
}

/// A compact slider: label on the left and value on the right, drawn inside
/// a bar whose fill shows the value. Drag anywhere to set it; double-click
/// to type a number.
#[allow(clippy::too_many_arguments)]
pub fn field(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    min: f32,
    max: f32,
    scale: Scale,
    fmt: &dyn Fn(f32) -> String,
    width: f32,
) -> Response {
    let id = ui.scope_id().with(("field", label));
    let (rect, mut resp) = ui.allocate_exact_size(vec2(width, theme::ROW), Sense::click_and_drag());
    let editing_id = id.with("edit");
    let mut editing: Option<String> = ui.data(|d| d.get_temp(editing_id));
    if resp.double_clicked() {
        editing = Some(format!("{}", (*value * 100.0).round() / 100.0));
    }
    let to_t = |v: f32| match scale {
        Scale::Linear => (v - min) / (max - min),
        Scale::Log => (v.max(min).ln() - min.ln()) / (max.ln() - min.ln()),
    };
    let from_t = |t: f32| match scale {
        Scale::Linear => min + t * (max - min),
        Scale::Log => (min.ln() + t * (max.ln() - min.ln())).exp(),
    };
    if let Some(mut text) = editing {
        let r = ui.put(
            rect,
            egui::TextEdit::singleline(&mut text)
                .align(Align2::CENTER_CENTER),
        );
        r.request_focus();
        let done = r.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter));
        if done {
            let t = text.trim().trim_end_matches(['%', 'p', 'x', ' ', '°']);
            if let Ok(v) = t.parse::<f32>() {
                let pct = fmt(1.0).ends_with('%') && max <= 1.0;
                let v = if pct { v / 100.0 } else { v };
                *value = v.clamp(min, max);
                resp.mark_changed();
            }
            ui.data_mut(|d| d.remove::<String>(editing_id));
        } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            ui.data_mut(|d| d.remove::<String>(editing_id));
        } else {
            ui.data_mut(|d| d.insert_temp(editing_id, text));
        }
        return resp;
    }
    if (resp.dragged() || resp.clicked())
        && !resp.double_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        // Shift drags finely, relative to where the drag started.
        let fine = ui.input(|i| i.modifiers.shift);
        let t = if fine && resp.dragged() {
            let d = resp.drag_delta().x / rect.width() * 0.1;
            (to_t(*value) + d).clamp(0.0, 1.0)
        } else {
            ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0)
        };
        let v = from_t(t);
        if (v - *value).abs() > f32::EPSILON {
            *value = v;
            resp.mark_changed();
        }
    }
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
    let p = ui.painter();
    let hot = resp.hovered() || resp.dragged();
    p.rect_filled(rect, 6.0, if hot { theme::BG_HOVER } else { theme::BG_RAISED });
    let t = to_t(*value).clamp(0.0, 1.0);
    if t > 0.0 {
        let fill = Rect::from_min_max(rect.min, pos2(rect.left() + rect.width() * t, rect.bottom()));
        p.with_clip_rect(fill).rect_filled(
            rect,
            6.0,
            if hot {
                Color32::from_rgb(78, 78, 86)
            } else {
                Color32::from_rgb(64, 64, 71)
            },
        );
    }
    p.text(
        rect.left_center() + vec2(8.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.0),
        theme::TEXT_DIM,
    );
    p.text(
        rect.right_center() - vec2(8.0, 0.0),
        Align2::RIGHT_CENTER,
        fmt(*value),
        egui::FontId::proportional(12.0),
        theme::TEXT,
    );
    resp
}

/// A field for a 0..=1 value shown as a percentage.
pub fn pct(ui: &mut Ui, label: &str, v: &mut f32, width: f32) -> Response {
    field(ui, label, v, 0.0, 1.0, Scale::Linear, &|x| format!("{:.0}%", x * 100.0), width)
}

/// A panel header with a disclosure triangle; returns whether it's open.
pub fn panel_header(ui: &mut Ui, title: &str, open: &mut bool, add_actions: impl FnOnce(&mut Ui)) {
    let w = ui.available_width();
    ui.horizontal(|ui| {
        ui.set_height(26.0);
        let (rect, resp) = ui.allocate_exact_size(vec2(w * 0.5, 26.0), Sense::click());
        let caret = if *open {
            egui_phosphor::regular::CARET_DOWN
        } else {
            egui_phosphor::regular::CARET_RIGHT
        };
        let col = if resp.hovered() { theme::TEXT } else { theme::TEXT_DIM };
        ui.painter().text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            caret,
            theme::icon_font(11.0),
            col,
        );
        ui.painter().text(
            rect.left_center() + vec2(16.0, 0.0),
            Align2::LEFT_CENTER,
            title,
            theme::semibold(12.5),
            theme::TEXT,
        );
        if resp.clicked() {
            *open = !*open;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            add_actions(ui);
        });
    });
}

/// A thin horizontal divider spanning the panel.
pub fn divider(ui: &mut Ui) {
    let r = ui.available_rect_before_wrap();
    let y = ui.cursor().top() + 3.0;
    ui.painter().hline(r.x_range(), y, Stroke::new(1.0, theme::LINE));
    ui.add_space(7.0);
}

/// A colour swatch; returns its response.
pub fn swatch(ui: &mut Ui, color: [u8; 3], size: Vec2, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let c = Color32::from_rgb(color[0], color[1], color[2]);
    ui.painter().rect_filled(rect, 4.0, c);
    let stroke = if selected {
        Stroke::new(2.0, Color32::WHITE)
    } else if resp.hovered() {
        Stroke::new(1.5, Color32::from_gray(210))
    } else {
        Stroke::new(1.0, Color32::from_white_alpha(18))
    };
    ui.painter().rect_stroke(rect, 4.0, stroke, StrokeKind::Inside);
    resp
}

/// A small dim caption.
pub fn caption(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(11.5).color(theme::TEXT_FAINT));
}
