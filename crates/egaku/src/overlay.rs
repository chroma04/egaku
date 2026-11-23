//! Everything drawn over the canvas: cursors, selection outlines, handles,
//! grids, the text box and the zoom HUD.

use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, CursorIcon, Mesh, Pos2, Rect, Shape, Stroke, Vec2, pos2, vec2,
};
use egaku_core::document::Kind;
use egui_phosphor::regular as ph;

use crate::app::{Active, App, Gesture};
use crate::handles::{self, Handle};
use crate::prefs::{SelectShape, Tool};
use crate::theme;

const ANT: f32 = 4.0;

/// Draws marching ants along screen-space segments.
fn ants(painter: &egui::Painter, segs: &[(Pos2, Pos2)], phase: f32) {
    let mut mesh = Mesh::default();
    let mut quad = |a: Pos2, b: Pos2, c: Color32| {
        let d = b - a;
        let len = d.length();
        if len < 0.01 {
            return;
        }
        let n = vec2(-d.y, d.x) / len * 0.6;
        let i = mesh.vertices.len() as u32;
        for p in [a + n, b + n, b - n, a - n] {
            mesh.colored_vertex(p, c);
        }
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
    };
    for &(a, b) in segs {
        quad(a, b, Color32::BLACK);
        let d = b - a;
        let len = d.length();
        if len < 0.5 {
            continue;
        }
        let dir = d / len;
        // Dashes are phased by screen position so they line up between
        // neighbouring segments and crawl over time.
        let start = (a.x + a.y + phase).rem_euclid(ANT * 2.0);
        let mut t = -start;
        while t < len {
            let s = t.max(0.0);
            let e = (t + ANT).min(len);
            if e > s {
                quad(a + dir * s, a + dir * e, Color32::WHITE);
            }
            t += ANT * 2.0;
        }
    }
    painter.add(Shape::mesh(mesh));
}

fn handle_box(painter: &egui::Painter, p: Pos2) {
    let r = Rect::from_center_size(p, vec2(8.0, 8.0));
    painter.rect_filled(r, 2.0, Color32::WHITE);
    painter.rect_stroke(
        r,
        2.0,
        Stroke::new(1.0, Color32::from_black_alpha(180)),
        egui::StrokeKind::Outside,
    );
}

fn frame_lines(painter: &egui::Painter, s: &[Pos2; 4], col: Color32) {
    for i in 0..4 {
        let (a, b) = (s[i], s[(i + 1) % 4]);
        painter.line_segment([a, b], Stroke::new(3.0, Color32::from_black_alpha(90)));
        painter.line_segment([a, b], Stroke::new(1.2, col));
    }
}

impl App {
    fn selection_segments(&mut self) -> Option<Arc<Vec<[f32; 4]>>> {
        let sel = self.ed.doc.state.selection.clone()?;
        let key = Arc::as_ptr(&sel) as usize;
        match &self.sel_outline {
            Some((k, segs)) if *k == key => Some(segs.clone()),
            _ => {
                let segs = Arc::new(sel.outline());
                self.sel_outline = Some((key, segs.clone()));
                Some(segs)
            }
        }
    }

    pub fn canvas_overlay(&mut self, ui: &mut egui::Ui, vp: Rect, resp: &egui::Response) {
        let ctx = ui.ctx().clone();
        let painter = ui.painter_at(vp);
        let ppp = ctx.pixels_per_point();
        let active = self.active_tool(&ctx);
        let over = resp.contains_pointer() || self.gesture != Gesture::None;
        let now = ctx.input(|i| i.time);
        let view = self.view;
        let to_s = |x: f32, y: f32| view.to_screen(vp, ppp, pos2(x, y));
        let scale = view.scale(ppp);

        // Pixel grid at high zoom, and an optional coarse grid.
        let vis = view.visible_canvas(vp, ppp).intersect(Rect::from_min_size(
            Pos2::ZERO,
            vec2(self.ed.doc.width as f32, self.ed.doc.height as f32),
        ));
        let grid = |step: f32, col: Color32| {
            if vis.width() <= 0.0 || step * scale < 6.0 {
                return;
            }
            let (x0, x1) = ((vis.left() / step).floor(), (vis.right() / step).ceil());
            let (y0, y1) = ((vis.top() / step).floor(), (vis.bottom() / step).ceil());
            if x1 - x0 > 600.0 || y1 - y0 > 600.0 {
                return;
            }
            let st = Stroke::new(1.0, col);
            let mut x = x0;
            while x <= x1 {
                painter.line_segment(
                    [to_s(x * step, vis.top()), to_s(x * step, vis.bottom())],
                    st,
                );
                x += 1.0;
            }
            let mut y = y0;
            while y <= y1 {
                painter.line_segment(
                    [to_s(vis.left(), y * step), to_s(vis.right(), y * step)],
                    st,
                );
                y += 1.0;
            }
        };
        if self.prefs.pixel_grid && view.zoom >= 8.0 {
            grid(1.0, Color32::from_black_alpha(64));
        }
        if self.prefs.grid_size > 0 {
            grid(
                self.prefs.grid_size as f32,
                Color32::from_rgba_unmultiplied(80, 160, 255, 110),
            );
        }

        // Selection outline.
        if let Some(segs) = self.selection_segments() {
            let screen: Vec<(Pos2, Pos2)> = segs
                .iter()
                .map(|s| (to_s(s[0], s[1]), to_s(s[2], s[3])))
                .filter(|(a, b)| vp.expand(4.0).intersects(Rect::from_two_pos(*a, *b)))
                .collect();
            if screen.len() < 60_000 {
                ants(&painter, &screen, (now * 18.0) as f32);
                ctx.request_repaint_after(std::time::Duration::from_millis(80));
            } else {
                for (a, b) in screen {
                    painter.line_segment([a, b], Stroke::new(1.0, Color32::WHITE));
                }
            }
        }
        // Selection being drawn.
        match self.gesture {
            Gesture::SelectBox { start, end } => {
                let (a, b) = (start, end);
                let pts: Vec<Pos2> = if self.prefs.select_shape == SelectShape::Ellipse {
                    let (cx, cy) = ((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
                    let (rx, ry) = ((b.x - a.x).abs() * 0.5, (b.y - a.y).abs() * 0.5);
                    (0..96)
                        .map(|i| {
                            let t = i as f32 / 96.0 * std::f32::consts::TAU;
                            to_s(cx + t.cos() * rx, cy + t.sin() * ry)
                        })
                        .collect()
                } else {
                    vec![
                        to_s(a.x, a.y),
                        to_s(b.x, a.y),
                        to_s(b.x, b.y),
                        to_s(a.x, b.y),
                    ]
                };
                let segs: Vec<(Pos2, Pos2)> = (0..pts.len())
                    .map(|i| (pts[i], pts[(i + 1) % pts.len()]))
                    .collect();
                ants(&painter, &segs, (now * 18.0) as f32);
                let (w, h) = ((b.x - a.x).abs().round(), (b.y - a.y).abs().round());
                if let Some(p) = self.hover {
                    tag(&painter, p + vec2(16.0, 18.0), &format!("{w:.0} × {h:.0}"));
                }
                ctx.request_repaint();
            }
            Gesture::Lasso if self.lasso.len() > 1 => {
                let pts: Vec<Pos2> = self.lasso.iter().map(|p| to_s(p.x, p.y)).collect();
                let segs: Vec<(Pos2, Pos2)> = pts.windows(2).map(|w| (w[0], w[1])).collect();
                ants(&painter, &segs, (now * 18.0) as f32);
                painter.line_segment(
                    [*pts.last().unwrap(), pts[0]],
                    Stroke::new(1.0, Color32::from_white_alpha(90)),
                );
                ctx.request_repaint();
            }
            Gesture::Gradient { start, end } => {
                let (a, b) = (to_s(start.x, start.y), to_s(end.x, end.y));
                painter.line_segment([a, b], Stroke::new(3.0, Color32::from_black_alpha(120)));
                painter.line_segment([a, b], Stroke::new(1.5, Color32::WHITE));
                for (p, c) in [(a, self.prefs.fg), (b, self.prefs.bg)] {
                    painter.circle_filled(p, 6.0, Color32::from_rgb(c[0], c[1], c[2]));
                    painter.circle_stroke(p, 6.0, Stroke::new(1.5, Color32::WHITE));
                }
            }
            _ => {}
        }

        // Crop.
        if self.prefs.tool == Tool::Crop {
            let (w, h) = (self.ed.doc.width as f32, self.ed.doc.height as f32);
            let r = self.crop.unwrap_or([0.0, 0.0, w, h]);
            if self.crop.is_some() {
                // Dim what will be cut away.
                let dim = Color32::from_black_alpha(140);
                let canvas = [to_s(0.0, 0.0), to_s(w, 0.0), to_s(w, h), to_s(0.0, h)];
                let inner = [
                    to_s(r[0], r[1]),
                    to_s(r[2], r[1]),
                    to_s(r[2], r[3]),
                    to_s(r[0], r[3]),
                ];
                // One mesh without edge feathering, so the pieces meet
                // without seams.
                let mut mesh = Mesh::default();
                for p in canvas.iter().chain(inner.iter()) {
                    mesh.colored_vertex(*p, dim);
                }
                for i in 0..4u32 {
                    let j = (i + 1) % 4;
                    mesh.add_triangle(i, j, 4 + j);
                    mesh.add_triangle(i, 4 + j, 4 + i);
                }
                painter.add(Shape::mesh(mesh));
                // Rule of thirds.
                for k in [1.0 / 3.0, 2.0 / 3.0] {
                    let x = r[0] + (r[2] - r[0]) * k;
                    let y = r[1] + (r[3] - r[1]) * k;
                    let st = Stroke::new(1.0, Color32::from_white_alpha(70));
                    painter.line_segment([to_s(x, r[1]), to_s(x, r[3])], st);
                    painter.line_segment([to_s(r[0], y), to_s(r[2], y)], st);
                }
            }
            let s = handles::screen_quad(&view, vp, ppp, &handles::rect_quad(r));
            frame_lines(&painter, &s, Color32::WHITE);
            for (p, _) in handles::handle_points(&s) {
                handle_box(&painter, p);
            }
            if self.crop.is_some() {
                tag(
                    &painter,
                    s[2] + vec2(10.0, 10.0),
                    &format!("{:.0} × {:.0}", r[2] - r[0], r[3] - r[1]),
                );
            }
            if over
                && self.gesture == Gesture::None
                && let Some(p) = self.hover
                && self.crop.is_some()
                && let Some(hh) = handles::hit(&s, p, false)
            {
                ctx.set_cursor_icon(handles::cursor_for(hh, &s));
            }
        }

        // Transform handles.
        let mut handle_cursor = None;
        if self.shows_handles()
            && let Some(q) = self.handle_quad()
        {
            let s = handles::screen_quad(&view, vp, ppp, &q);
            frame_lines(&painter, &s, theme::ACCENT);
            for (p, _) in handles::handle_points(&s) {
                handle_box(&painter, p);
            }
            if let Gesture::Handle { handle, .. } = self.gesture {
                handle_cursor = Some(handles::cursor_for(handle, &s));
                if let Some(t) = self.ed.transforming() {
                    let (sx, sy) = t.delta.compose(t.frame).scales();
                    let (fx, _) = t.frame.scales();
                    let label = match handle {
                        Handle::Rotate => {
                            format!("{:.1}°", t.delta.compose(t.frame).angle().to_degrees())
                        }
                        Handle::Inside => format!("{:+.0}, {:+.0}", t.delta.m[4], t.delta.m[5]),
                        _ => format!(
                            "{:.0} × {:.0}  ({:.0}%)",
                            t.size.0 * sx,
                            t.size.1 * sy,
                            sx / fx.max(1e-6) * 100.0
                        ),
                    };
                    if let Some(p) = self.hover {
                        tag(&painter, p + vec2(16.0, 18.0), &label);
                    }
                }
            } else if over
                && matches!(active, Active::Tool(Tool::Move))
                && let Some(p) = self.hover
                && let Some(h) = handles::hit(&s, p, true)
            {
                handle_cursor = Some(handles::cursor_for(h, &s));
            }
        }

        // Text being edited: its box, and a field to type in.
        self.text_overlay(&ctx, &painter, vp);

        // Cursor.
        if over {
            let icon = match (active, self.gesture) {
                (_, Gesture::Pan { .. }) => CursorIcon::Grabbing,
                _ if handle_cursor.is_some() => handle_cursor.unwrap(),
                (Active::Zoom, _) => CursorIcon::ZoomIn,
                (Active::Rotate, _) => CursorIcon::AllScroll,
                (Active::Tool(Tool::Hand), _) => CursorIcon::Grab,
                (Active::Tool(Tool::Move), _) => CursorIcon::Move,
                (Active::Tool(Tool::Text), _) => CursorIcon::Text,
                (Active::Tool(Tool::Brush | Tool::Eraser), _) => CursorIcon::None,
                (Active::Tool(Tool::Crop), _) if self.crop.is_some() => {
                    ctx.output(|o| o.cursor_icon)
                }
                _ => CursorIcon::Crosshair,
            };
            ctx.set_cursor_icon(icon);
        }

        if let (Some(p), true) = (self.hover, over) {
            if let Active::Tool(t @ (Tool::Brush | Tool::Eraser)) = active {
                let b = self.brush_params(t);
                let c = view.to_canvas(vp, ppp, p);
                if b.pixel {
                    // Outline the exact pixels the dab will cover.
                    let side = b.size.round().max(1.0);
                    let x0 = (c.x - side * 0.5 + 0.5).floor();
                    let y0 = (c.y - side * 0.5 + 0.5).floor();
                    let q = [
                        to_s(x0, y0),
                        to_s(x0 + side, y0),
                        to_s(x0 + side, y0 + side),
                        to_s(x0, y0 + side),
                    ];
                    if side * scale >= 4.0 {
                        frame_lines(&painter, &q, Color32::WHITE);
                    }
                } else {
                    let r = b.size * 0.5 * scale;
                    if r > 3.0 {
                        painter.circle_stroke(
                            p,
                            r + 0.5,
                            Stroke::new(1.0, Color32::from_black_alpha(140)),
                        );
                        painter.circle_stroke(
                            p,
                            r - 0.5,
                            Stroke::new(1.0, Color32::from_white_alpha(200)),
                        );
                    }
                }
                // A tiny crosshair marks the exact hotspot at any size.
                let k = 3.0;
                for (a, b) in [(vec2(-k, 0.0), vec2(k, 0.0)), (vec2(0.0, -k), vec2(0.0, k))] {
                    painter.line_segment(
                        [p + a, p + b],
                        Stroke::new(2.5, Color32::from_black_alpha(120)),
                    );
                    painter.line_segment([p + a, p + b], Stroke::new(1.0, Color32::WHITE));
                }
            }
            if let Active::Tool(Tool::Picker) = active {
                let c = self.prefs.fg;
                let col = Color32::from_rgb(c[0], c[1], c[2]);
                painter.circle_filled(p + vec2(20.0, -20.0), 11.0, col);
                painter.circle_stroke(
                    p + vec2(20.0, -20.0),
                    11.0,
                    Stroke::new(2.0, Color32::WHITE),
                );
                painter.circle_stroke(
                    p + vec2(20.0, -20.0),
                    12.5,
                    Stroke::new(1.0, Color32::from_black_alpha(120)),
                );
            }
        }

        // Rotation indicator while rotating.
        if matches!(self.gesture, Gesture::Rotate { .. }) {
            let c = vp.center();
            let d = vec2(view.rotation.cos(), view.rotation.sin()) * 40.0;
            painter.circle_stroke(c, 40.0, Stroke::new(1.0, Color32::from_white_alpha(90)));
            painter.line_segment([c, c + vec2(d.y, -d.x)], Stroke::new(2.0, theme::ACCENT));
            tag(
                &painter,
                c + vec2(-16.0, 52.0),
                &format!("{:.0}°", view.rotation.to_degrees()),
            );
        }
        if let Gesture::Move { start } = self.gesture
            && let Some(p) = self.hover
        {
            let c = view.to_canvas(vp, ppp, p);
            let d = c - start;
            tag(
                &painter,
                p + vec2(16.0, 18.0),
                &format!("{:+.0}, {:+.0}", d.x, d.y),
            );
        }

        self.hud(ui, vp);
        self.draw_toast(&ctx, &painter, vp);
    }

    fn text_overlay(&mut self, ctx: &egui::Context, painter: &egui::Painter, vp: Rect) {
        let Some(te) = &self.text_edit else { return };
        let ppp = ctx.pixels_per_point();
        // The box comes from the rendered layer once it exists.
        let obj = te
            .layer
            .and_then(|id| self.ed.doc.find(id))
            .and_then(|l| match &l.content {
                egaku_core::document::Content::Text(o) => Some((**o).clone()),
                _ => None,
            })
            .unwrap_or_else(|| te.obj.clone());
        let (w, h) = (obj.bbox.0.max(obj.size * 0.3), obj.bbox.1.max(obj.size));
        let m = obj.transform;
        let q = [
            m.apply(0.0, 0.0),
            m.apply(w, 0.0),
            m.apply(w, h),
            m.apply(0.0, h),
        ];
        let s = handles::screen_quad(&self.view, vp, ppp, &q);
        let segs: Vec<(Pos2, Pos2)> = (0..4).map(|i| (s[i], s[(i + 1) % 4])).collect();
        ants(painter, &segs, 0.0);
        // A blinking caret at the end of the last line hints where typing goes.
        let blink = (ctx.input(|i| i.time) * 2.0) as i64 % 2 == 0;
        if blink && obj.text.is_empty() {
            painter.line_segment(
                [s[0] + vec2(2.0, 3.0), s[3] + vec2(2.0, -3.0)],
                Stroke::new(1.5, theme::ACCENT),
            );
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));

        // Typing field just below the box.
        let below = s.iter().fold(s[0], |a, p| if p.y > a.y { *p } else { a });
        let left = s.iter().map(|p| p.x).fold(f32::MAX, f32::min);
        let at = pos2(
            left.clamp(vp.left() + 8.0, vp.right() - 300.0),
            (below.y + 10.0).min(vp.bottom() - 90.0),
        );
        let mut text = self.text_edit.as_ref().unwrap().obj.text.clone();
        let mut done = false;
        egui::Area::new(egui::Id::unique("text-entry"))
            .fixed_pos(at)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(280.0);
                    let r = ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .desired_rows(2)
                            .desired_width(268.0)
                            .hint_text("Type your text…"),
                    );
                    if !r.has_focus() && !r.lost_focus() {
                        r.request_focus();
                    }
                    ui.horizontal(|ui| {
                        crate::widgets::caption(ui, "Esc or click away to finish");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button(crate::widgets::il(ph::CHECK, "Done")).clicked() {
                                done = true;
                            }
                        });
                    });
                });
            });
        let te = self.text_edit.as_mut().unwrap();
        if text != te.obj.text {
            te.obj.text = text;
            match te.layer {
                Some(id) => {
                    let o = te.obj.clone();
                    self.ed.set_text(id, o);
                }
                None if !te.obj.text.trim().is_empty() => {
                    let o = te.obj.clone();
                    let id = self.ed.add_text(o);
                    self.text_edit.as_mut().unwrap().layer = Some(id);
                }
                None => {}
            }
        }
        if done {
            self.finish_modes();
        }
    }

    fn hud(&mut self, ui: &mut egui::Ui, vp: Rect) {
        let ctx = ui.ctx().clone();
        let id = egui::Id::unique("zoom-hud");
        let pos = vp.right_bottom() - vec2(12.0, 12.0);
        egui::Area::new(id)
            .fixed_pos(pos)
            .pivot(Align2::RIGHT_BOTTOM)
            .order(egui::Order::Middle)
            .show(&ctx, |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(28, 28, 31, 235))
                    .corner_radius(8)
                    .stroke(Stroke::new(1.0, theme::LINE))
                    .inner_margin(egui::Margin::symmetric(4, 3))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            let deg = self.view.rotation.to_degrees();
                            if deg.abs() > 0.05
                                && ui
                                    .add(
                                        egui::Button::new(crate::widgets::il(
                                            ph::ARROW_CLOCKWISE,
                                            &format!("{deg:.0}°"),
                                        ))
                                        .frame(false),
                                    )
                                    .on_hover_text("Reset rotation")
                                    .clicked()
                            {
                                self.view.set_rotation(0.0);
                            }
                            if self.view.mirror
                                && ui
                                    .add(
                                        egui::Button::new(crate::widgets::ic(ph::FLIP_HORIZONTAL))
                                            .frame(false),
                                    )
                                    .on_hover_text("Mirrored — click to unmirror (Shift+M)")
                                    .clicked()
                            {
                                self.view.toggle_mirror();
                            }
                            let ppp = ctx.pixels_per_point();
                            if ui
                                .add(egui::Button::new(crate::widgets::ic(ph::MINUS)).frame(false))
                                .on_hover_text("Zoom out (Ctrl+−)")
                                .clicked()
                            {
                                self.zoom_step(&ctx, vp, -1.0);
                            }
                            let z = format!("{:.0}%", self.view.zoom * 100.0);
                            let mb = |b: usize| b as f64 / 1048576.0;
                            let tip = format!(
                                "{} × {} px\npixels {:.0} MB · undo {:.0} MB · {} GPU tiles",
                                self.ed.doc.width,
                                self.ed.doc.height,
                                mb(self.ed.doc.pixel_bytes()),
                                mb(self.ed.history.memory()),
                                self.textures.texture_count()
                            );
                            let r = ui.add(
                                egui::Button::new(egui::RichText::new(z).monospace())
                                    .frame(false)
                                    .min_size(vec2(52.0, 20.0)),
                            );
                            egui::Popup::menu(&r).show(|ui| {
                                if ui.button("Fit to window").clicked() {
                                    self.fit_pending = true;
                                }
                                for z in [0.25, 0.5, 1.0, 2.0, 4.0, 8.0] {
                                    if ui.button(format!("{:.0}%", z * 100.0)).clicked() {
                                        self.view.zoom_at(vp, ppp, vp.center(), z);
                                    }
                                }
                            });
                            r.on_hover_text(tip);
                            if ui
                                .add(egui::Button::new(crate::widgets::ic(ph::PLUS)).frame(false))
                                .on_hover_text("Zoom in (Ctrl++)")
                                .clicked()
                            {
                                self.zoom_step(&ctx, vp, 1.0);
                            }
                        });
                    });
            });
        let _ = Vec2::ZERO;
        let _ = Kind::Raster;
    }

    fn draw_toast(&mut self, ctx: &egui::Context, painter: &egui::Painter, vp: Rect) {
        let now = ctx.input(|i| i.time);
        if let Some(t) = &self.toast {
            if now > t.until {
                self.toast = None;
            } else {
                let alpha = ((t.until - now) * 3.0).min(1.0) as f32;
                let icon = if t.warn {
                    ph::WARNING_CIRCLE
                } else {
                    ph::CHECK_CIRCLE
                };
                let galley = painter.layout_no_wrap(
                    t.text.clone(),
                    egui::FontId::proportional(13.5),
                    theme::TEXT.gamma_multiply(alpha),
                );
                let size = galley.size() + vec2(48.0, 18.0);
                let r = Rect::from_center_size(pos2(vp.center().x, vp.bottom() - 48.0), size);
                painter.rect_filled(
                    r.translate(vec2(0.0, 3.0)),
                    10.0,
                    Color32::from_black_alpha((60.0 * alpha) as u8),
                );
                painter.rect_filled(
                    r,
                    10.0,
                    Color32::from_rgba_unmultiplied(40, 40, 44, (240.0 * alpha) as u8),
                );
                painter.text(
                    r.left_center() + vec2(18.0, 0.0),
                    Align2::CENTER_CENTER,
                    icon,
                    theme::icon_font(16.0),
                    (if t.warn { theme::WARN } else { theme::ACCENT }).gamma_multiply(alpha),
                );
                painter.galley(r.min + vec2(34.0, 9.0), galley, Color32::WHITE);
                ctx.request_repaint();
            }
        }
    }
}

/// A small dark label.
fn tag(painter: &egui::Painter, p: Pos2, text: &str) {
    let g = painter.layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(12.0),
        Color32::WHITE,
    );
    let r = Rect::from_min_size(p, g.size() + vec2(12.0, 6.0));
    painter.rect_filled(r, 5.0, Color32::from_black_alpha(190));
    painter.galley(r.min + vec2(6.0, 3.0), g, Color32::WHITE);
}
