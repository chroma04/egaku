//! Small custom widgets shared by the panels.

use eframe::egui::{
    self, Align2, Color32, CursorIcon, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2,
    vec2,
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
    egui::RichText::new(icon)
        .font(theme::icon_font(14.0))
        .into()
}

/// A square, frameless icon button.
pub fn icon_button(ui: &mut Ui, icon: &str, size: f32, selected: bool, tip: &str) -> Response {
    icon_button_sized(ui, icon, vec2(size, size), selected, tip)
}

pub fn icon_button_sized(
    ui: &mut Ui,
    icon: &str,
    size: Vec2,
    selected: bool,
    tip: &str,
) -> Response {
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
    if tip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tip)
    }
}

/// A row of mutually exclusive icon buttons in a shared pill.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    value: &mut T,
    opts: &[(T, &str, &str)],
) -> bool {
    let mut changed = false;
    let h = theme::ROW;
    let text_font = egui::FontId::proportional(12.5);
    // Icons get square segments; words get room for themselves.
    let widths: Vec<f32> = opts
        .iter()
        .map(|(_, label, _)| {
            if label.chars().count() == 1 {
                h + 4.0
            } else {
                ui.painter()
                    .layout_no_wrap(label.to_string(), text_font.clone(), theme::TEXT)
                    .size()
                    .x
                    + 18.0
            }
        })
        .collect();
    let total: f32 = widths.iter().sum();
    let (rect, _) = ui.allocate_exact_size(vec2(total, h), Sense::hover());
    ui.painter().rect_filled(rect, 6.0, theme::BG_RAISED);
    let mut x = rect.left();
    for (i, ((v, label, tip), w)) in opts.iter().zip(&widths).enumerate() {
        let r = Rect::from_min_size(pos2(x, rect.top()), vec2(*w, h));
        x += w;
        let resp = ui.interact(
            r,
            ui.scope_id().with(("seg", i, *tip, *label)),
            Sense::click(),
        );
        let sel = *value == *v;
        if sel {
            ui.painter()
                .rect_filled(r.shrink(2.0), 4.0, theme::BG_ACTIVE);
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(r.shrink(2.0), 4.0, theme::BG_HOVER);
        }
        let is_icon = label.chars().count() == 1;
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            label,
            if is_icon {
                theme::icon_font(15.0)
            } else {
                text_font.clone()
            },
            if sel { Color32::WHITE } else { theme::TEXT_DIM },
        );
        let resp = if tip.is_empty() {
            resp
        } else {
            resp.on_hover_text(*tip)
        };
        if resp.clicked() && !sel {
            *value = *v;
            changed = true;
        }
    }
    changed
}

/// A two-state chip with an icon (and optional text).
pub fn toggle(ui: &mut Ui, on: &mut bool, icon: &str, text: &str, tip: &str) -> Response {
    let font = egui::FontId::proportional(12.5);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font.clone(), theme::TEXT);
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
    if tip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tip)
    }
}

/// How a field slider maps its value to the bar.
#[derive(Copy, Clone)]
pub enum Scale {
    Linear,
    Log,
}

/// How a number is shown and typed. Values are stored in their natural
/// units (opacity 0..=1) but always shown, and typed, in display units
/// (`mul` × value, e.g. 0..=100 %), so what you type is what you saw.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Fmt {
    pub mul: f32,
    pub decimals: usize,
    /// One more decimal below 10 (brush sizes: "3.5 px", "40 px").
    pub fine_below_10: bool,
    pub suffix: &'static str,
    /// Show a sign and allow typing negative numbers.
    pub signed: bool,
}

impl Fmt {
    pub const fn num(decimals: usize) -> Fmt {
        Fmt {
            mul: 1.0,
            decimals,
            fine_below_10: false,
            suffix: "",
            signed: false,
        }
    }
    pub const fn int() -> Fmt {
        Fmt::num(0)
    }
    pub const fn px() -> Fmt {
        Fmt::num(0).suffix(" px")
    }
    /// 0..=1 shown as 0..=100 %.
    pub const fn pct() -> Fmt {
        Fmt::num(0).mul(100.0).suffix("%")
    }
    /// -1..=1 shown as −100..=+100.
    pub const fn signed_pct() -> Fmt {
        Fmt::num(0).mul(100.0).signed()
    }
    pub const fn suffix(mut self, s: &'static str) -> Fmt {
        self.suffix = s;
        self
    }
    pub const fn mul(mut self, m: f32) -> Fmt {
        self.mul = m;
        self
    }
    pub const fn signed(mut self) -> Fmt {
        self.signed = true;
        self
    }
    pub const fn fine(mut self) -> Fmt {
        self.fine_below_10 = true;
        self
    }

    fn decimals_for(&self, shown: f32) -> usize {
        if self.fine_below_10 && shown.abs() < 9.95 {
            self.decimals + 1
        } else {
            self.decimals
        }
    }

    /// The value as displayed, with its unit.
    pub fn show(&self, v: f32) -> String {
        let d = v * self.mul;
        let n = self.decimals_for(d);
        // Avoid "-0".
        let d = if d.abs() < 0.5 * 10f32.powi(-(n as i32)) {
            0.0
        } else {
            d
        };
        if self.signed {
            format!("{d:+.n$}{}", self.suffix)
        } else {
            format!("{d:.n$}{}", self.suffix)
        }
    }

    /// The value as it appears in the box when you start typing.
    fn edit_text(&self, v: f32) -> String {
        let d = v * self.mul;
        let n = self
            .decimals_for(d)
            .max(if self.mul == 1.0 { 0 } else { 1 });
        let s = format!("{d:.n$}");
        let s = if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        };
        if s == "-0" { "0".into() } else { s }
    }

    /// Whether `c` can appear in a typed value.
    fn allows(&self, c: char, negative_ok: bool) -> bool {
        c.is_ascii_digit()
            || (c == '.' && (self.decimals > 0 || self.fine_below_10 || self.mul != 1.0))
            || ((c == '-' || c == '+') && negative_ok)
    }

    fn parse(&self, s: &str) -> Option<f32> {
        let v: f32 = s.trim().parse().ok()?;
        v.is_finite().then_some(v / self.mul)
    }
}

/// Which cell is being typed into. Only one at a time: starting to edit a
/// cell commits whichever other cell was open.
fn active_cell_id() -> egui::Id {
    egui::Id::unique("egaku-active-cell")
}

#[derive(Clone)]
struct CellEdit {
    text: String,
    /// True on the frame editing started (focus is requested then only).
    fresh: bool,
}

/// Starts typing into a cell.
fn begin_cell(ui: &Ui, id: egui::Id, text: String) {
    let te = id.with("te");
    ui.data_mut(|d| {
        d.insert_temp(id.with("edit"), CellEdit { text, fresh: true });
        d.insert_temp(active_cell_id(), id);
    });
    ui.memory_mut(|m| m.request_focus(te));
}

/// Shows the text box of a cell being edited. Returns `None` if the cell
/// isn't being edited, else `Some(Some(v))` when a value was committed and
/// `Some(None)` while editing continues (or was cancelled).
///
/// Enter or clicking away commits; Escape discards. Characters that can't
/// be part of the value are never inserted.
fn cell_editor(
    ui: &mut Ui,
    id: egui::Id,
    rect: Rect,
    fmt: Fmt,
    min: f32,
    max: f32,
) -> Option<Option<f32>> {
    let edit_id = id.with("edit");
    let mut e: CellEdit = ui.data(|d| d.get_temp(edit_id))?;
    let te = id.with("te");
    let commit = |text: &str| fmt.parse(text).map(|v| v.clamp(min, max));
    let finish = |ui: &Ui| {
        ui.data_mut(|d| {
            d.remove::<CellEdit>(edit_id);
            if d.get_temp::<egui::Id>(active_cell_id()) == Some(id) {
                d.remove::<egui::Id>(active_cell_id());
            }
        });
    };
    // Another cell took over: keep what was typed here.
    if ui.data(|d| d.get_temp::<egui::Id>(active_cell_id())) != Some(id) {
        finish(ui);
        return Some(commit(&e.text));
    }
    let focused = ui.memory(|m| m.has_focus(te));
    let negative_ok = min < 0.0;
    if focused || e.fresh {
        ui.input_mut(|i| {
            i.events.retain_mut(|ev| match ev {
                egui::Event::Text(t) | egui::Event::Paste(t) => {
                    t.retain(|c| fmt.allows(c, negative_ok));
                    !t.is_empty()
                }
                _ => true,
            })
        });
    }
    let r = ui.put(
        rect,
        egui::TextEdit::singleline(&mut e.text)
            .id(te)
            .char_limit(16)
            .align(Align2::CENTER_CENTER),
    );
    e.text.retain(|c| fmt.allows(c, negative_ok));
    if e.fresh {
        // The click that opened the box would otherwise count as a click
        // elsewhere (the box didn't exist under the pointer yet) and take
        // the focus straight away again.
        ui.memory_mut(|m| m.request_focus(te));
        // Select everything so typing replaces the old value.
        if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), te) {
            st.cursor
                .set_char_range(Some(egui::text_selection::CCursorRange::select_all_str(
                    &e.text,
                )));
            egui::TextEdit::store_state(ui.ctx(), te, st);
        }
    }
    let esc = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    if esc {
        finish(ui);
        ui.memory_mut(|m| m.surrender_focus(te));
        return Some(None);
    }
    let enter = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    if enter || r.lost_focus() || r.clicked_elsewhere() {
        finish(ui);
        ui.memory_mut(|m| m.surrender_focus(te));
        return Some(commit(&e.text));
    }
    if !focused && !e.fresh {
        // Left open while the widget was hidden (tool changed): drop it.
        finish(ui);
        return Some(None);
    }
    e.fresh = false;
    ui.data_mut(|d| d.insert_temp(edit_id, e));
    Some(None)
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
    fmt: Fmt,
    width: f32,
) -> Response {
    let id = ui.scope_id().with(("field", label));
    let (rect, mut resp) = ui.allocate_exact_size(vec2(width, theme::ROW), Sense::click_and_drag());
    if resp.double_clicked() {
        // The first click of the double-click moved the value; put it back,
        // since the double-click means "let me type it".
        if let Some(v) = ui.data(|d| d.get_temp::<f32>(id.with("before-click")))
            && v != *value
        {
            *value = v;
            resp.mark_changed();
        }
        begin_cell(ui, id, fmt.edit_text(*value));
    }
    let to_t = |v: f32| match scale {
        Scale::Linear => (v - min) / (max - min),
        Scale::Log => (v.max(min).ln() - min.ln()) / (max.ln() - min.ln()),
    };
    let from_t = |t: f32| match scale {
        Scale::Linear => min + t * (max - min),
        Scale::Log => (min.ln() + t * (max.ln() - min.ln())).exp(),
    };
    if let Some(done) = cell_editor(ui, id, rect, fmt, min, max) {
        if let Some(v) = done
            && v != *value
        {
            *value = v;
            resp.mark_changed();
        }
        return resp;
    }
    let undo_id = id.with("before-click");
    if resp.clicked() {
        ui.data_mut(|d| d.insert_temp(undo_id, *value));
    } else if resp.drag_started() {
        ui.data_mut(|d| d.remove::<f32>(undo_id));
    }
    if (resp.dragged() || resp.clicked())
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
    p.rect_filled(
        rect,
        6.0,
        if hot {
            theme::BG_HOVER
        } else {
            theme::BG_RAISED
        },
    );
    let t = to_t(*value).clamp(0.0, 1.0);
    if t > 0.0 {
        let fill = Rect::from_min_max(
            rect.min,
            pos2(rect.left() + rect.width() * t, rect.bottom()),
        );
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
        fmt.show(*value),
        egui::FontId::proportional(12.0),
        theme::TEXT,
    );
    resp
}

/// A field for a 0..=1 value shown as a percentage.
pub fn pct(ui: &mut Ui, label: &str, v: &mut f32, width: f32) -> Response {
    field(ui, label, v, 0.0, 1.0, Scale::Linear, Fmt::pct(), width)
}

/// A number box without a bar, for values with a wide range (pixel
/// dimensions): drag sideways to scrub, click to type.
pub fn number_box(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    value: &mut u32,
    min: u32,
    max: u32,
    suffix: &'static str,
    width: f32,
) -> Response {
    let id = ui.scope_id().with(("numbox", id_salt));
    let fmt = Fmt::int().suffix(suffix);
    let (rect, mut resp) = ui.allocate_exact_size(vec2(width, theme::ROW), Sense::click_and_drag());
    if resp.clicked() {
        begin_cell(ui, id, fmt.edit_text(*value as f32));
    }
    if let Some(done) = cell_editor(ui, id, rect, fmt, min as f32, max as f32) {
        if let Some(v) = done {
            let v = v.round() as u32;
            if v != *value {
                *value = v;
                resp.mark_changed();
            }
        }
        return resp;
    }
    let acc_id = id.with("acc");
    if resp.dragged() {
        // Faster the further you go; Shift for single steps.
        let fine = ui.input(|i| i.modifiers.shift);
        let mut acc: f32 = ui.data(|d| d.get_temp(acc_id)).unwrap_or(0.0);
        let dx = resp.drag_delta().x;
        acc += if fine {
            dx * 0.25
        } else {
            dx * (1.0 + *value as f32 / 200.0)
        };
        let step = acc.trunc();
        if step != 0.0 {
            let v = (*value as f32 + step).clamp(min as f32, max as f32) as u32;
            if v != *value {
                *value = v;
                resp.mark_changed();
            }
            acc -= step;
        }
        ui.data_mut(|d| d.insert_temp(acc_id, acc));
    } else {
        ui.data_mut(|d| d.remove::<f32>(acc_id));
    }
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
    let hot = resp.hovered() || resp.dragged();
    let p = ui.painter();
    p.rect_filled(
        rect,
        6.0,
        if hot {
            theme::BG_HOVER
        } else {
            theme::BG_RAISED
        },
    );
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        fmt.show(*value as f32),
        egui::FontId::proportional(12.5),
        theme::TEXT,
    );
    resp
}

/// Keeps only characters `ok` accepts in text typed into the focused
/// widget `id`, before the widget sees them.
pub fn filter_typing(ui: &Ui, id: egui::Id, ok: impl Fn(char) -> bool) {
    if !ui.memory(|m| m.has_focus(id)) {
        return;
    }
    ui.input_mut(|i| {
        i.events.retain_mut(|ev| match ev {
            egui::Event::Text(t) | egui::Event::Paste(t) => {
                t.retain(&ok);
                !t.is_empty()
            }
            _ => true,
        })
    });
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
        let col = if resp.hovered() {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        };
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
    ui.painter()
        .hline(r.x_range(), y, Stroke::new(1.0, theme::LINE));
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
    ui.painter()
        .rect_stroke(rect, 4.0, stroke, StrokeKind::Inside);
    resp
}

/// A small dim caption.
pub fn caption(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(11.5)
            .color(theme::TEXT_FAINT),
    );
}
