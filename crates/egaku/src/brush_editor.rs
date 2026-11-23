//! The brush editor: a floating window for shaping a brush while you try it.
//!
//! Brush panels in most painting apps are long lists of sliders whose effect
//! you only find out on the canvas. Here every change shows at once on a
//! scratch pad inside the window, the pressure curve can be drawn for you
//! from a few test strokes instead of fiddled with, and settings are grouped
//! by what you notice (the tip, how it reacts, its texture) rather than by
//! how the engine is built.

use eframe::egui::{
    self, Align2, Color32, ColorImage, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};
use egaku_core::brush::{
    BrushParams, Dab, LINEAR_CURVE, Mask, StrokeEngine, eval_curve, fit_curve,
};
use egui_phosphor::regular as ph;

use crate::app::App;
use crate::prefs::{Tool, UserBrush, presets};
use crate::theme;
use crate::widgets::{self, Fmt, Scale, field, pct, toggle};

const PAD_W: u32 = 360;
const PAD_H: u32 = 132;
/// The largest brush the pad shows at its true size.
const PAD_MAX_SIZE: f32 = 44.0;

/// The scratch pad: strokes made with the brush being edited.
pub struct Pad {
    mask: Mask,
    engine: Option<StrokeEngine>,
    tex: Option<egui::TextureHandle>,
    dirty: bool,
    /// Learning the pen's pressure range from the next strokes.
    calibrating: bool,
    samples: Vec<f32>,
    /// Pressure while the pen is down on the pad, for the curve's marker.
    live: Option<f32>,
}

impl Pad {
    fn new() -> Self {
        Pad {
            mask: Mask::new(PAD_W, PAD_H),
            engine: None,
            tex: None,
            dirty: true,
            calibrating: false,
            samples: vec![],
            live: None,
        }
    }

    fn clear(&mut self) {
        self.mask = Mask::new(PAD_W, PAD_H);
        self.engine = None;
        self.dirty = true;
    }
}

/// The settings the pad paints with: large brushes are scaled down to fit,
/// with their texture and lengths scaled alike so it still looks right.
fn pad_params(p: &BrushParams) -> (BrushParams, bool) {
    let mut q = *p;
    if p.pixel {
        q.size = p.size.clamp(1.0, 12.0);
        return (q, p.size > 12.0);
    }
    let scaled = p.size > PAD_MAX_SIZE;
    if scaled {
        let k = PAD_MAX_SIZE / p.size;
        q.size = PAD_MAX_SIZE;
        q.grain_scale = (p.grain_scale * k).max(1.5);
        q.taper = p.taper * k;
    }
    (q, scaled)
}

impl App {
    fn current_brush(&self) -> BrushParams {
        *self.brush_params(self.prefs.tool)
    }

    pub fn toggle_brush_editor(&mut self) {
        self.prefs.brush_editor = !self.prefs.brush_editor;
        if self.prefs.brush_editor && !matches!(self.prefs.tool, Tool::Brush | Tool::Eraser) {
            self.set_tool(Tool::Brush);
        }
    }

    pub fn brush_editor(&mut self, ctx: &egui::Context) {
        if !self.prefs.brush_editor {
            return;
        }
        let mut open = true;
        let editing = if self.prefs.tool == Tool::Eraser {
            "Eraser"
        } else {
            "Brush"
        };
        egui::Window::new(
            egui::RichText::new(format!("{editing} editor")).font(theme::semibold(13.0)),
        )
        .id(egui::Id::unique("brush-editor"))
        .open(&mut open)
        .default_pos(pos2(64.0, 84.0))
        .default_size(vec2(640.0, 760.0))
        .min_width(600.0)
        .resizable(true)
        .collapsible(true)
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(180.0);
                    self.brush_library(ui, ctx);
                });
                ui.separator();
                ui.vertical(|ui| {
                    self.brush_pad(ui, ctx);
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical()
                        .id_salt("brush-settings")
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.brush_settings(ui));
                });
            });
        });
        if !open {
            self.prefs.brush_editor = false;
        }
        if ctx.input(|i| i.pointer.any_released()) {
            self.ed.end_gesture();
        }
    }

    // ---- library --------------------------------------------------------------

    fn brush_library(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let current = self.current_brush();
        widgets::caption(ui, "Library");
        let mut load = None;
        let mut remove = None;
        let mut overwrite = None;
        egui::ScrollArea::vertical()
            .id_salt("brush-library")
            .max_height(ui.available_height() - 64.0)
            .show(ui, |ui| {
                for p in presets() {
                    if self
                        .library_row(ui, ctx, p.name, &p.params, p.params == current, false)
                        .clicked()
                    {
                        load = Some(p.params);
                    }
                }
                if !self.prefs.user_brushes.is_empty() {
                    ui.add_space(6.0);
                    widgets::caption(ui, "Yours");
                }
                let users = self.prefs.user_brushes.clone();
                for (i, b) in users.iter().enumerate() {
                    let r =
                        self.library_row(ui, ctx, &b.name, &b.params, b.params == current, true);
                    if r.clicked() {
                        load = Some(b.params);
                    }
                    egui::Popup::context_menu(&r).show(|ui| {
                        if ui.button("Update with current settings").clicked() {
                            overwrite = Some(i);
                        }
                        if ui.button("Delete").clicked() {
                            remove = Some(i);
                        }
                    });
                }
            });
        if let Some(p) = load {
            *self.current_brush_mut() = p;
            if let Some(pad) = &mut self.pad {
                pad.clear();
            }
        }
        if let Some(i) = overwrite {
            self.prefs.user_brushes[i].params = current;
        }
        if let Some(i) = remove {
            self.prefs.user_brushes.remove(i);
        }
        ui.add_space(6.0);
        let known = presets().iter().any(|p| p.params == current)
            || self.prefs.user_brushes.iter().any(|b| b.params == current);
        let name_id = egui::Id::unique("new-brush-name");
        let mut name: String = ui.data(|d| d.get_temp(name_id)).unwrap_or_default();
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut name)
                    .hint_text("Name")
                    .desired_width(110.0),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui
                .add_enabled(!known, egui::Button::new(widgets::il(ph::PLUS, "Save")))
                .on_hover_text(if known {
                    "These settings are already in the library"
                } else {
                    "Keep these settings as a new brush"
                })
                .clicked()
                || (enter && !known)
            {
                let n = name.trim();
                let n = if n.is_empty() {
                    format!("Brush {}", self.prefs.user_brushes.len() + 1)
                } else {
                    n.to_string()
                };
                self.prefs.user_brushes.push(UserBrush {
                    name: n,
                    params: current,
                });
                name.clear();
            }
        });
        ui.data_mut(|d| d.insert_temp(name_id, name));
        if !known {
            widgets::caption(ui, "Modified — save to keep it.");
        }
    }

    fn library_row(
        &mut self,
        ui: &mut Ui,
        ctx: &egui::Context,
        name: &str,
        params: &BrushParams,
        selected: bool,
        user: bool,
    ) -> egui::Response {
        let key = if user {
            format!("user:{name}")
        } else {
            name.to_string()
        };
        let tex = self.preset_preview(ctx, &key, params);
        let (rect, resp) = ui.allocate_exact_size(vec2(176.0, 44.0), Sense::click());
        if selected || resp.hovered() {
            ui.painter().rect_filled(
                rect,
                6.0,
                if selected {
                    theme::BG_ACTIVE
                } else {
                    theme::BG_HOVER
                },
            );
        }
        ui.painter().text(
            rect.left_top() + vec2(8.0, 4.0),
            Align2::LEFT_TOP,
            name,
            egui::FontId::proportional(12.0),
            theme::TEXT,
        );
        let img = Rect::from_min_size(rect.min + vec2(8.0, 18.0), vec2(160.0, 24.0));
        ui.painter().image(
            tex,
            img,
            Rect::from_min_max(pos2(0.0, 0.1), pos2(1.0, 0.9)),
            Color32::WHITE,
        );
        resp
    }

    // ---- scratch pad ------------------------------------------------------------

    fn brush_pad(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let p = self.current_brush();
        let pressure = self.pen_pressure;
        let fg = self.prefs.fg;
        let pad = self.pad.get_or_insert_with(Pad::new);
        let (pp, scaled) = pad_params(&p);
        ui.horizontal(|ui| {
            widgets::caption(ui, "Try it here");
            if scaled {
                widgets::caption(ui, &format!("· shown at {:.0} px", pp.size));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Clear").clicked() {
                    pad.clear();
                }
            });
        });
        let (rect, resp) =
            ui.allocate_exact_size(vec2(PAD_W as f32, PAD_H as f32), Sense::click_and_drag());
        let local = |p: Pos2| (p.x - rect.left(), p.y - rect.top());
        let pr = pressure.unwrap_or(1.0);
        let mut dabs: Vec<Dab> = vec![];
        if resp.drag_started() || (resp.clicked() && pad.engine.is_none()) {
            pad.engine = Some(StrokeEngine::new(pp));
        }
        if let Some(e) = &mut pad.engine
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let (x, y) = local(pos);
            e.add_point(x, y, pr, &mut dabs);
            pad.live = pressure;
            if pad.calibrating
                && let Some(p) = pressure
            {
                pad.samples.push(p);
            }
        }
        if (resp.drag_stopped() || resp.clicked())
            && let Some(mut e) = pad.engine.take()
        {
            e.finish(&mut dabs);
            pad.live = None;
        }
        if !dabs.is_empty() {
            let mut dirty = vec![];
            for d in &dabs {
                pad.mask.stamp(d, &mut dirty);
            }
            pad.dirty = true;
        }
        if pad.dirty {
            let px: Vec<Color32> = (0..PAD_H)
                .flat_map(|y| (0..PAD_W).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let a = (pad.mask.value(x, y) >> 8) as u8;
                    Color32::from_rgba_unmultiplied(fg[0], fg[1], fg[2], a)
                })
                .collect();
            let img = ColorImage::new([PAD_W as usize, PAD_H as usize], px);
            match &mut pad.tex {
                Some(t) => t.set(img, egui::TextureOptions::NEAREST),
                None => {
                    pad.tex =
                        Some(ctx.load_texture("brush-pad", img, egui::TextureOptions::NEAREST))
                }
            }
            pad.dirty = false;
        }
        // Paper that contrasts with the main colour.
        let lum = fg[0] as u32 * 3 + fg[1] as u32 * 6 + fg[2] as u32;
        let paper = if lum > 1700 {
            Color32::from_gray(52)
        } else {
            Color32::from_rgb(244, 243, 238)
        };
        ui.painter().rect_filled(rect, 6.0, paper);
        if let Some(t) = &pad.tex {
            ui.painter().image(
                t.id(),
                rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        if pad.calibrating {
            ui.painter().rect_stroke(
                rect,
                6.0,
                Stroke::new(2.0, theme::ACCENT),
                StrokeKind::Inside,
            );
            ui.painter().text(
                rect.center_top() + vec2(0.0, 12.0),
                Align2::CENTER_CENTER,
                "Draw a few strokes the way you normally press",
                egui::FontId::proportional(12.0),
                theme::ACCENT,
            );
        }
        if resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
    }

    // ---- settings -----------------------------------------------------------------

    fn brush_settings(&mut self, ui: &mut Ui) {
        let before = self.current_brush();
        let mut p = before;
        let w = ui.available_width().min(420.0);
        let half = (w - 6.0) / 2.0;

        section(ui, "Tip", true, |ui| {
            ui.horizontal_top(|ui| {
                tip_preview(ui, &p);
                ui.vertical(|ui| {
                    let fw = w - 70.0;
                    if p.pixel {
                        field(
                            ui,
                            "Size",
                            &mut p.size,
                            1.0,
                            64.0,
                            Scale::Linear,
                            Fmt::px(),
                            fw,
                        );
                        p.size = p.size.round().max(1.0);
                    } else {
                        field(
                            ui,
                            "Size",
                            &mut p.size,
                            1.0,
                            1000.0,
                            Scale::Log,
                            Fmt::px().fine(),
                            fw,
                        );
                        pct(ui, "Hardness", &mut p.hardness, fw);
                    }
                    if !p.pixel {
                        field(
                            ui,
                            "Spacing",
                            &mut p.spacing,
                            0.01,
                            1.5,
                            Scale::Log,
                            Fmt::pct(),
                            fw,
                        )
                        .on_hover_text("Distance between dabs, as a share of the size");
                    }
                });
            });
            if !p.pixel {
                ui.horizontal(|ui| {
                    pct(ui, "Roundness", &mut p.roundness, half);
                    p.roundness = p.roundness.max(0.02);
                    field(
                        ui,
                        "Angle",
                        &mut p.angle,
                        -180.0,
                        180.0,
                        Scale::Linear,
                        Fmt::int().suffix("°"),
                        half,
                    );
                });
                toggle(
                    ui,
                    &mut p.follow_direction,
                    ph::NAVIGATION_ARROW,
                    "Turn with the stroke",
                    "The tip rotates to follow the direction you draw in",
                );
            } else {
                ui.checkbox(&mut p.pixel_perfect, "Pixel-perfect lines")
                    .on_hover_text("Removes doubled pixels at the corners of 1px lines");
            }
        });

        section(ui, "Pressure", true, |ui| {
            ui.horizontal_top(|ui| {
                let live = self.pad.as_ref().and_then(|p| p.live);
                curve_editor(ui, &mut p.pressure_curve, live);
                ui.vertical(|ui| {
                    widgets::caption(ui, "Pen pressure controls");
                    ui.checkbox(&mut p.pressure_size, "Size");
                    if p.pressure_size {
                        pct(ui, "Thinnest", &mut p.min_size, 170.0)
                            .on_hover_text("Size at the lightest touch");
                    }
                    ui.checkbox(&mut p.pressure_opacity, "Opacity");
                    ui.checkbox(&mut p.pressure_flow, "Flow (build-up)");
                    ui.add_space(6.0);
                    let calibrating = self.pad.as_ref().is_some_and(|p| p.calibrating);
                    if calibrating {
                        let n = self.pad.as_ref().map_or(0, |p| p.samples.len());
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    n >= 8,
                                    egui::Button::new(widgets::il(ph::CHECK, "Use my range")),
                                )
                                .clicked()
                                && let Some(pad) = &mut self.pad
                            {
                                if let Some(c) = fit_curve(&pad.samples) {
                                    p.pressure_curve = c;
                                }
                                pad.calibrating = false;
                                pad.samples.clear();
                            }
                            if ui.button("Cancel").clicked()
                                && let Some(pad) = &mut self.pad
                            {
                                pad.calibrating = false;
                                pad.samples.clear();
                            }
                        });
                        if self.pen_pressure.is_none() {
                            widgets::caption(ui, "No pen pressure yet — use a tablet pen.");
                        }
                    } else {
                        if ui
                            .button(widgets::il(ph::TARGET, "Fit to my hand…"))
                            .on_hover_text(
                                "Draw on the pad and the curve is set from how hard you \
                                 actually press: your light touch becomes thin, your \
                                 usual pressure the middle, your firmest full.",
                            )
                            .clicked()
                        {
                            let pad = self.pad.get_or_insert_with(Pad::new);
                            pad.clear();
                            pad.calibrating = true;
                            pad.samples.clear();
                        }
                        if p.pressure_curve != LINEAR_CURVE
                            && ui.small_button("Straight curve").clicked()
                        {
                            p.pressure_curve = LINEAR_CURVE;
                        }
                    }
                });
            });
        });

        if !p.pixel {
            section(ui, "Stroke", true, |ui| {
                ui.horizontal(|ui| {
                    pct(ui, "Opacity", &mut p.opacity, half);
                    pct(ui, "Flow", &mut p.flow, half);
                });
                ui.horizontal(|ui| {
                    pct(ui, "Stabilizer", &mut p.smoothing, half)
                        .on_hover_text("Smooths the stroke behind the pen");
                    field(
                        ui,
                        "Taper in",
                        &mut p.taper,
                        0.0,
                        400.0,
                        Scale::Linear,
                        Fmt::px(),
                        half,
                    )
                    .on_hover_text("Strokes start thin and swell over this distance");
                });
                pct(ui, "Thin when fast", &mut p.speed_size, half).on_hover_text(
                    "Quick movements make a thinner line, like ink from a nib. \
                     Gives a mouse some of the life of a pen.",
                );
            });

            section(ui, "Scatter", false, |ui| {
                ui.horizontal(|ui| {
                    pct(ui, "Size jitter", &mut p.size_jitter, half);
                    pct(ui, "Angle jitter", &mut p.angle_jitter, half);
                });
                ui.horizontal(|ui| {
                    pct(ui, "Opacity jitter", &mut p.opacity_jitter, half);
                    field(
                        ui,
                        "Scatter",
                        &mut p.scatter,
                        0.0,
                        3.0,
                        Scale::Linear,
                        Fmt::pct(),
                        half,
                    )
                    .on_hover_text("Throws each dab off the path, by up to this share of the size");
                });
            });

            section(ui, "Texture", false, |ui| {
                ui.horizontal(|ui| {
                    pct(ui, "Grain", &mut p.grain, half)
                        .on_hover_text("Paper texture showing through, for chalk and pencil");
                    field(
                        ui,
                        "Grain size",
                        &mut p.grain_scale,
                        1.0,
                        64.0,
                        Scale::Log,
                        Fmt::px().fine(),
                        half,
                    );
                });
                widgets::caption(
                    ui,
                    "The grain is fixed to the canvas, so strokes share one paper.",
                );
            });
        }

        section(ui, "Mode", false, |ui| {
            toggle(
                ui,
                &mut p.pixel,
                ph::GRID_FOUR,
                "Pixel",
                "Pixel-art mode: hard, aliased pixels with no gaps",
            );
            if p.pixel != before.pixel {
                p.size = if p.pixel {
                    p.size.clamp(1.0, 64.0).round()
                } else {
                    p.size
                };
            }
        });

        if p != before {
            *self.current_brush_mut() = p;
        }
    }
}

/// A collapsible group of settings.
fn section(ui: &mut Ui, title: &str, open: bool, body: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(egui::RichText::new(title).font(theme::semibold(12.5)))
        .default_open(open)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 5.0;
            body(ui);
        });
}

/// The tip's shape at a glance.
fn tip_preview(ui: &mut Ui, p: &BrushParams) {
    let (rect, _) = ui.allocate_exact_size(vec2(60.0, 60.0), Sense::hover());
    ui.painter().rect_filled(rect, 6.0, theme::BG_RAISED);
    let n = 48u32;
    let mut mask = Mask::new(n, n);
    let mut dirty = vec![];
    let r = if p.pixel { 8.0 } else { 20.0 };
    mask.stamp(
        &Dab {
            x: n as f32 / 2.0,
            y: n as f32 / 2.0,
            radius: r,
            hardness: p.hardness,
            flow: 1.0,
            cap: 1.0,
            pixel: p.pixel,
            roundness: if p.pixel { 1.0 } else { p.roundness },
            angle: p.angle.to_radians(),
            grain: p.grain,
            grain_scale: (p.grain_scale * r * 2.0 / p.size.max(1.0)).max(1.5),
        },
        &mut dirty,
    );
    let cell = rect.width() / n as f32;
    let mut mesh = egui::Mesh::default();
    for y in 0..n {
        for x in 0..n {
            let a = (mask.value(x, y) >> 8) as u8;
            if a == 0 {
                continue;
            }
            let c = Color32::from_white_alpha(a);
            let r = Rect::from_min_size(
                rect.min + vec2(x as f32 * cell, y as f32 * cell),
                vec2(cell, cell),
            );
            mesh.add_colored_rect(r, c);
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// A pressure curve with four draggable points. Input runs left to right,
/// output bottom to top; a line follows the pen while it is on the pad.
fn curve_editor(ui: &mut Ui, curve: &mut [[f32; 2]; 4], live: Option<f32>) {
    let size = 150.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
    let p = ui.painter();
    p.rect_filled(rect, 6.0, theme::BG_RAISED);
    let inner = rect.shrink(8.0);
    let to_s = |x: f32, y: f32| {
        pos2(
            inner.left() + x * inner.width(),
            inner.bottom() - y * inner.height(),
        )
    };
    let from_s = |s: Pos2| {
        (
            ((s.x - inner.left()) / inner.width()).clamp(0.0, 1.0),
            ((inner.bottom() - s.y) / inner.height()).clamp(0.0, 1.0),
        )
    };
    for k in 1..4 {
        let t = k as f32 / 4.0;
        let st = Stroke::new(1.0, theme::LINE);
        p.line_segment([to_s(t, 0.0), to_s(t, 1.0)], st);
        p.line_segment([to_s(0.0, t), to_s(1.0, t)], st);
    }
    p.line_segment(
        [to_s(0.0, 0.0), to_s(1.0, 1.0)],
        Stroke::new(1.0, Color32::from_white_alpha(18)),
    );
    let pts: Vec<Pos2> = (0..=64)
        .map(|i| {
            let x = i as f32 / 64.0;
            to_s(x, eval_curve(curve, x))
        })
        .collect();
    p.add(egui::Shape::line(pts, Stroke::new(2.0, theme::ACCENT)));
    if let Some(l) = live {
        let y = eval_curve(curve, l);
        p.line_segment(
            [to_s(l, 0.0), to_s(l, y)],
            Stroke::new(1.0, Color32::from_white_alpha(120)),
        );
        p.circle_filled(to_s(l, y), 3.0, Color32::WHITE);
        ui.ctx().request_repaint();
    }
    // Dragging: grab the nearest point.
    let drag_id = ui.scope_id().with("curve-drag");
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let near = (0..4)
            .map(|i| (i, (to_s(curve[i][0], curve[i][1]) - pos).length()))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .filter(|(_, d)| *d < 16.0)
            .map(|(i, _)| i);
        ui.data_mut(|d| d.insert_temp(drag_id, near));
    }
    let grabbed: Option<usize> = ui.data(|d| d.get_temp(drag_id)).flatten();
    if resp.dragged()
        && let (Some(i), Some(pos)) = (grabbed, resp.interact_pointer_pos())
    {
        let (x, y) = from_s(pos);
        let lo = if i == 0 { 0.0 } else { curve[i - 1][0] + 0.02 };
        let hi = if i == 3 { 1.0 } else { curve[i + 1][0] - 0.02 };
        curve[i] = [x.clamp(lo, hi.max(lo)), y];
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<Option<usize>>(drag_id));
    }
    if resp.double_clicked() {
        *curve = LINEAR_CURVE;
    }
    let hover = resp.hover_pos();
    for (i, pt) in curve.iter().enumerate() {
        let s = to_s(pt[0], pt[1]);
        let hot = grabbed == Some(i) || hover.is_some_and(|h| (h - s).length() < 16.0);
        p.circle_filled(s, if hot { 5.5 } else { 4.5 }, Color32::WHITE);
        p.circle_stroke(s, 5.5, Stroke::new(1.0, Color32::from_black_alpha(160)));
    }
    p.text(
        rect.left_bottom() + vec2(22.0, -2.0),
        Align2::LEFT_BOTTOM,
        "light",
        egui::FontId::proportional(10.0),
        theme::TEXT_FAINT,
    );
    p.text(
        rect.right_bottom() + vec2(-10.0, -2.0),
        Align2::RIGHT_BOTTOM,
        "firm",
        egui::FontId::proportional(10.0),
        theme::TEXT_FAINT,
    );
    resp.on_hover_text("Drag the points to shape how pressure responds. Double-click to reset.");
}
