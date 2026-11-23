//! Canvas input: what each tool does with the pointer, and shortcuts.

use std::sync::Arc;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, Pos2, Rect};
use egaku_core::adjust::Gradient;
use egaku_core::brush::{Paint, PaintMode};
use egaku_core::document::{Content, Kind};
use egaku_core::fill::FillParams;
use egaku_core::selection::{SelectOp, Selection};
use egaku_core::transform::Affine;

use crate::app::{Active, App, Gesture, Modal, Pending, TextEditing, consume_exact};
use crate::canvas::visible_tiles;
use crate::handles::{self, Handle};
use crate::prefs::{GradientKind, SelectShape, Tool};

impl App {
    /// Which tool a press would use, taking held keys into account.
    pub fn active_tool(&self, ctx: &egui::Context) -> Active {
        if ctx.egui_wants_keyboard_input() {
            return Active::Tool(self.prefs.tool);
        }
        let (m, space, r) =
            ctx.input(|i| (i.modifiers, i.key_down(Key::Space), i.key_down(Key::R)));
        let tool = self.prefs.tool;
        if space && m.command {
            Active::Zoom
        } else if (space && m.shift) || r {
            Active::Rotate
        } else if space {
            Active::Tool(Tool::Hand)
        } else if m.alt
            && !m.shift
            && matches!(
                tool,
                Tool::Brush | Tool::Eraser | Tool::Fill | Tool::Gradient | Tool::Text
            )
        {
            Active::Tool(Tool::Picker)
        } else if m.command
            && !m.shift
            && !matches!(tool, Tool::Move | Tool::Hand | Tool::Select | Tool::Crop)
        {
            Active::Tool(Tool::Move)
        } else if self.pen_eraser && tool == Tool::Brush {
            Active::Tool(Tool::Eraser)
        } else {
            Active::Tool(tool)
        }
    }

    /// Whether the active layer shows transform handles under the move tool.
    pub fn shows_handles(&self) -> bool {
        self.free_transform
            || self
                .ed
                .transforming()
                .is_some_and(|t| t.layers().count() > 1)
            || (self.prefs.tool == Tool::Move
                && self.ed.selection_count() == 1
                && matches!(self.ed.doc.active_layer().kind(), Kind::Text | Kind::Image))
    }

    /// The handle frame of the active layer (or the running transform), in
    /// canvas coordinates.
    pub fn handle_quad(&self) -> Option<[(f32, f32); 4]> {
        if let Some(t) = self.ed.transforming() {
            return Some(t.quad());
        }
        let l = self.ed.doc.active_layer();
        let (m, w, h) = match &l.content {
            Content::Image(o) => (o.transform, o.source.width as f32, o.source.height as f32),
            Content::Text(o) => (o.transform, o.bbox.0, o.bbox.1),
            _ => return None,
        };
        Some([
            m.apply(0.0, 0.0),
            m.apply(w, 0.0),
            m.apply(w, h),
            m.apply(0.0, h),
        ])
    }

    // ---- canvas -------------------------------------------------------------

    pub fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let vp = ui.max_rect();
        let resp = ui.interact(
            vp,
            ui.scope_id().with("canvas"),
            egui::Sense::click_and_drag(),
        );
        ui.painter().rect_filled(vp, 0.0, crate::theme::BG_CANVAS);
        let ppp = ctx.pixels_per_point();
        if self.ed.size_gen != self.tex_size_gen {
            self.tex_size_gen = self.ed.size_gen;
            self.textures.clear();
            self.thumbs.clear();
            self.sel_outline = None;
            if std::mem::take(&mut self.keep_view) {
                // Keep zoom and rotation; just make sure the canvas is in reach.
                let (w, h) = (self.ed.doc.width as f32, self.ed.doc.height as f32);
                self.view.center.x = self.view.center.x.clamp(0.0, w);
                self.view.center.y = self.view.center.y.clamp(0.0, h);
            } else {
                self.fit_pending = true;
            }
        }
        if self.fit_pending && vp.width() > 64.0 {
            self.view
                .fit(vp, ppp, self.ed.doc.width, self.ed.doc.height);
            self.fit_pending = false;
        }
        if let Some(t) = &mut self.tablet {
            let s = t.poll();
            if s.pressure.is_some() {
                self.pen_pressure = s.pressure;
            }
            self.pen_eraser = s.eraser;
        }
        self.tick_playback(&ctx);

        // Panels were drawn before the canvas handled input this frame, so
        // repaint once more whenever the document changed under them.
        let before = self.ed.state_id();
        self.canvas_input(&ctx, vp, &resp);
        if self.ed.state_id() != before {
            ctx.request_repaint();
        }

        let (w, h) = (self.ed.doc.width, self.ed.doc.height);
        let vis = visible_tiles(&self.view, vp, ppp, w, h);
        let pending = self.ed.update_projection(Some(vis), 192);
        let changes = self.ed.projection.take_changed();
        self.textures.note_changes(&changes);
        let transparent = self.ed.doc.state.paper.is_none();
        self.textures
            .paint(ui, vp, &self.view, &self.ed.projection, transparent);
        self.canvas_overlay(ui, vp, &resp);
        if pending || self.ed.projection.has_pending() {
            ctx.request_repaint();
        }
    }

    fn tick_playback(&mut self, ctx: &egui::Context) {
        let Some((t0, f0)) = self.playing else { return };
        let (frames, fps) = (self.ed.doc.state.frames, self.ed.doc.state.fps);
        if frames < 2 {
            self.playing = None;
            return;
        }
        let now = ctx.input(|i| i.time);
        let f = (f0 + ((now - t0) * fps as f64) as usize) % frames;
        self.ed.set_frame(f);
        let next = 1.0 / fps as f64 - ((now - t0) % (1.0 / fps as f64));
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(next.max(0.001)));
    }

    pub fn toggle_play(&mut self, ctx: &egui::Context) {
        if self.playing.is_some() {
            self.playing = None;
        } else if self.ed.doc.state.frames > 1 {
            self.finish_modes();
            self.playing = Some((ctx.input(|i| i.time), self.ed.doc.state.frame));
        }
    }

    fn canvas_input(&mut self, ctx: &egui::Context, vp: Rect, resp: &egui::Response) {
        let ppp = ctx.pixels_per_point();
        let over = resp.contains_pointer();
        let events = ctx.input(|i| i.events.clone());
        let active = self.active_tool(ctx);
        for ev in events {
            match ev {
                egui::Event::Touch {
                    force: Some(f),
                    phase,
                    ..
                } if self.tablet.is_none() => {
                    self.pen_pressure = match phase {
                        egui::TouchPhase::End | egui::TouchPhase::Cancel => None,
                        _ => Some(f.clamp(0.0, 1.0)),
                    };
                }
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed: true,
                    modifiers,
                } if over && self.modal.is_none() => match button {
                    egui::PointerButton::Primary if self.gesture == Gesture::None => {
                        self.press(ctx, vp, pos, modifiers, active);
                    }
                    egui::PointerButton::Middle => self.gesture = Gesture::Pan { last: pos },
                    _ => {}
                },
                egui::Event::PointerMoved(pos) => {
                    self.hover = Some(pos);
                    self.drag(ctx, vp, pos);
                }
                egui::Event::PointerButton {
                    pos,
                    pressed: false,
                    button,
                    ..
                } => {
                    if button == egui::PointerButton::Primary
                        || matches!(self.gesture, Gesture::Pan { .. })
                    {
                        self.drag(ctx, vp, pos);
                        self.release(ctx);
                    }
                }
                egui::Event::PointerGone => self.hover = None,
                egui::Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                    ..
                } if over => {
                    let anchor = self.hover.unwrap_or(vp.center());
                    match unit {
                        egui::MouseWheelUnit::Point if !modifiers.command => {
                            // Trackpad scrolling pans.
                            let a = self.view.to_canvas(vp, ppp, anchor);
                            let b = self.view.to_canvas(vp, ppp, anchor + delta);
                            self.view.center -= b - a;
                        }
                        egui::MouseWheelUnit::Point => {
                            let z = self.view.zoom * (delta.y * 0.01).exp();
                            self.view.zoom_at(vp, ppp, anchor, z);
                        }
                        _ => {
                            // Mouse wheel notches zoom around the cursor.
                            let z = self.view.zoom * 1.2f32.powf(delta.y.clamp(-4.0, 4.0));
                            self.view.zoom_at(vp, ppp, anchor, z);
                        }
                    }
                }
                egui::Event::Zoom(f) if over => {
                    let anchor = self.hover.unwrap_or(vp.center());
                    self.view.zoom_at(vp, ppp, anchor, self.view.zoom * f);
                }
                egui::Event::Rotate(a) if over => self.view.set_rotation(self.view.rotation + a),
                _ => {}
            }
        }
        if self.gesture != Gesture::None
            && !matches!(self.gesture, Gesture::Pan { .. })
            && !ctx.input(|i| i.pointer.primary_down())
        {
            // Released outside the window.
            self.release(ctx);
        }
    }

    fn pressure(&self) -> f32 {
        self.pen_pressure.unwrap_or(1.0)
    }

    fn paint_for(&self, tool: Tool) -> Paint {
        Paint {
            color: self.prefs.fg,
            mode: if tool == Tool::Eraser {
                PaintMode::Erase
            } else {
                PaintMode::Paint
            },
            alpha_lock: false,
        }
    }

    pub fn select_op(&self, m: Modifiers) -> SelectOp {
        match (m.shift, m.alt) {
            (true, true) => SelectOp::Intersect,
            (true, false) => SelectOp::Add,
            (false, true) => SelectOp::Subtract,
            _ => self.select_mode,
        }
    }

    fn gradient(&self, a: Pos2, b: Pos2) -> Gradient {
        let fg = self.prefs.fg;
        let bg = self.prefs.bg;
        Gradient {
            radial: self.prefs.gradient == GradientKind::Radial,
            from: (a.x, a.y),
            to: (b.x, b.y),
            c0: [fg[0], fg[1], fg[2], 255],
            c1: if self.prefs.gradient_transparent {
                [fg[0], fg[1], fg[2], 0]
            } else {
                [bg[0], bg[1], bg[2], 255]
            },
            opacity: self.prefs.brush.opacity,
        }
    }

    fn press(&mut self, ctx: &egui::Context, vp: Rect, pos: Pos2, m: Modifiers, active: Active) {
        let ppp = ctx.pixels_per_point();
        let c = self.view.to_canvas(vp, ppp, pos);
        self.playing = None;
        match active {
            Active::Zoom => {
                self.gesture = Gesture::Zoom {
                    start: pos,
                    start_zoom: self.view.zoom,
                }
            }
            Active::Rotate => {
                let d = pos - vp.center();
                self.gesture = Gesture::Rotate {
                    start_angle: d.y.atan2(d.x),
                    start_rot: self.view.rotation,
                };
            }
            Active::Tool(tool) => match tool {
                Tool::Brush | Tool::Eraser => {
                    let params = *self.brush_params(tool);
                    let paint = self.paint_for(tool);
                    let r = match (m.shift, self.ed.last_point()) {
                        // Shift-click draws a straight line from the last stroke.
                        (true, Some(from)) => self.ed.line(params, paint, from, (c.x, c.y)),
                        _ => self
                            .ed
                            .begin_stroke(params, paint, c.x, c.y, self.pressure())
                            .map(|_| {
                                self.gesture = Gesture::Stroke;
                            }),
                    };
                    if let Err(r) = r {
                        self.refusal(ctx, r);
                    } else if tool == Tool::Brush && self.gesture != Gesture::Stroke {
                        self.prefs.remember_color(self.prefs.fg);
                    }
                }
                Tool::Fill => {
                    if c.x >= 0.0 && c.y >= 0.0 {
                        let p = FillParams {
                            tolerance: self.prefs.fill_tolerance,
                            grow: self.prefs.fill_grow,
                            opacity: self.prefs.brush.opacity,
                        };
                        let paint = self.paint_for(Tool::Brush);
                        match self.ed.fill(
                            c.x as u32,
                            c.y as u32,
                            &p,
                            self.prefs.fill_sample_all,
                            paint,
                        ) {
                            Ok(()) => self.prefs.remember_color(self.prefs.fg),
                            Err(r) => self.refusal(ctx, r),
                        }
                    }
                }
                Tool::Gradient => match self.ed.begin_filter(Arc::new(self.gradient(c, c))) {
                    Ok(()) => self.gesture = Gesture::Gradient { start: c, end: c },
                    Err(r) => self.refusal(ctx, r),
                },
                Tool::Picker => {
                    self.gesture = Gesture::Pick;
                    self.pick(c);
                }
                Tool::Move => self.press_move(ctx, vp, pos, c, m),
                Tool::Select => {
                    let op = self.select_op(m);
                    self.press_op = op;
                    // Dragging inside the selection (with no modifier) moves
                    // the outline; the pixels stay where they are.
                    let inside = !m.shift
                        && !m.alt
                        && self.ed.selection().is_some_and(|s| {
                            s.value(c.x.floor() as i64, c.y.floor() as i64) >= 128
                        });
                    if inside {
                        self.gesture = Gesture::MoveSelection { start: c, end: c };
                        return;
                    }
                    match self.prefs.select_shape {
                        SelectShape::Rect | SelectShape::Ellipse => {
                            self.gesture = Gesture::SelectBox { start: c, end: c }
                        }
                        SelectShape::Lasso => {
                            self.lasso = vec![c];
                            self.gesture = Gesture::Lasso;
                        }
                        SelectShape::Color => {
                            if c.x >= 0.0 && c.y >= 0.0 {
                                self.ed.select_color(
                                    c.x as u32,
                                    c.y as u32,
                                    self.prefs.color_tolerance,
                                    self.prefs.select_antialias,
                                    self.prefs.wand_sample_all,
                                    op,
                                );
                            }
                        }
                        SelectShape::Wand => {
                            if c.x >= 0.0 && c.y >= 0.0 {
                                self.ed.magic_wand(
                                    c.x as u32,
                                    c.y as u32,
                                    self.prefs.wand_tolerance,
                                    self.prefs.wand_sample_all,
                                    op,
                                );
                            }
                        }
                    }
                }
                Tool::Text => self.press_text(c),
                Tool::Crop => {
                    let (w, h) = (self.ed.doc.width as f32, self.ed.doc.height as f32);
                    let r = self.crop.unwrap_or([0.0, 0.0, w, h]);
                    let s = handles::screen_quad(&self.view, vp, ppp, &handles::rect_quad(r));
                    let handle = match (self.crop, handles::hit(&s, pos, false)) {
                        (Some(_), Some(h)) => h,
                        _ => {
                            self.crop = Some([c.x, c.y, c.x, c.y]);
                            Handle::Edge(1, 1)
                        }
                    };
                    self.gesture = Gesture::Crop {
                        handle,
                        start: c,
                        start_rect: self.crop.unwrap(),
                    };
                }
                Tool::Hand => self.gesture = Gesture::Pan { last: pos },
            },
        }
    }

    fn press_move(&mut self, ctx: &egui::Context, vp: Rect, pos: Pos2, c: Pos2, m: Modifiers) {
        let ppp = ctx.pixels_per_point();
        // Handles first (free transform, or an object's frame).
        if self.shows_handles()
            && let Some(q) = self.handle_quad()
        {
            let s = handles::screen_quad(&self.view, vp, ppp, &q);
            match handles::hit(&s, pos, true) {
                Some(h) => {
                    if !self.free_transform
                        && let Err(r) = self.ed.begin_transform(self.prefs.smooth_transform)
                    {
                        self.refusal(ctx, r);
                        return;
                    }
                    let start_delta = self.ed.transforming().map_or(Affine::IDENTITY, |t| t.delta);
                    self.gesture = Gesture::Handle {
                        handle: h,
                        start: c,
                        start_delta,
                    };
                    return;
                }
                None if self.free_transform => {
                    // Clicking away applies the transform.
                    self.ed.commit_transform();
                    self.free_transform = false;
                }
                None => {}
            }
        }
        if self.prefs.auto_select
            && self.ed.selection().is_none()
            && !m.command
            && let Some(id) = self.layer_at(c.x, c.y)
        {
            // Picking a layer that is already selected keeps the others too.
            let keep = self.ed.is_layer_selected(id) && self.ed.selection_count() > 1;
            if !keep {
                self.ed.select_layer(id);
            }
            // An object picked this way can be dragged by its frame at once.
            if !keep && matches!(self.ed.doc.active_layer().kind(), Kind::Text | Kind::Image) {
                if self.ed.begin_transform(self.prefs.smooth_transform).is_ok() {
                    self.gesture = Gesture::Handle {
                        handle: Handle::Inside,
                        start: c,
                        start_delta: Affine::IDENTITY,
                    };
                }
                return;
            }
        }
        match self.ed.begin_transform(false) {
            Ok(()) => self.gesture = Gesture::Move { start: c },
            Err(r) => self.refusal(ctx, r),
        }
    }

    fn press_text(&mut self, c: Pos2) {
        // Clicking on a text layer edits it; elsewhere starts new text.
        let target = self
            .layer_at(c.x, c.y)
            .filter(|id| {
                self.ed
                    .doc
                    .find(*id)
                    .is_some_and(|l| l.kind() == Kind::Text)
            })
            .or_else(|| {
                // Or anywhere inside the current text's box.
                let l = self.ed.doc.active_layer();
                if let Content::Text(o) = &l.content
                    && let Some(inv) = o.transform.inverse()
                {
                    let (u, v) = inv.apply(c.x, c.y);
                    (u >= 0.0 && v >= 0.0 && u <= o.bbox.0 && v <= o.bbox.1).then_some(l.id)
                } else {
                    None
                }
            });
        if let Some(te) = &self.text_edit {
            if te.layer.is_some() && te.layer == target {
                return;
            }
            // Clicking away from the text being typed finishes it; the next
            // click starts new text.
            self.finish_modes();
            if target.is_none() {
                return;
            }
        }
        self.finish_modes();
        if let Some(id) = target {
            self.ed.select_layer(id);
            if let Content::Text(o) = &self.ed.doc.find(id).unwrap().content {
                let obj = (**o).clone();
                self.prefs.fg = obj.color;
                self.fg_hsv = crate::color_picker::to_hsv(obj.color);
                self.text_edit = Some(TextEditing {
                    layer: Some(id),
                    obj,
                });
            }
        } else {
            let t = &self.prefs.text;
            let size = t.size;
            let obj = egaku_core::object::TextObject {
                text: String::new(),
                font: t.font.clone(),
                size,
                color: self.prefs.fg,
                bold: t.bold,
                italic: t.italic,
                align: self.prefs.text_align(),
                line_height: t.line_height,
                wrap: None,
                transform: Affine::translate(c.x.round(), (c.y - size * 0.75).round()),
                bbox: (size * 0.5, size * t.line_height),
                pixel: t.pixel,
            };
            self.text_edit = Some(TextEditing { layer: None, obj });
        }
    }

    fn pick(&mut self, c: Pos2) {
        if let Some(p) = self.ed.sample(c.x, c.y) {
            // Sample what is seen: composite over the paper (or white).
            let paper = self.ed.doc.state.paper.unwrap_or([255; 3]);
            let a = p[3] as u32;
            let mix = |s: u8, d: u8| ((s as u32 * a + d as u32 * (255 - a) + 127) / 255) as u8;
            self.set_fg([
                mix(p[0], paper[0]),
                mix(p[1], paper[1]),
                mix(p[2], paper[2]),
            ]);
        }
    }

    fn drag(&mut self, ctx: &egui::Context, vp: Rect, pos: Pos2) {
        let ppp = ctx.pixels_per_point();
        let c = self.view.to_canvas(vp, ppp, pos);
        let m = ctx.input(|i| i.modifiers);
        match self.gesture {
            Gesture::None => {}
            Gesture::Stroke => self.ed.stroke_to(c.x, c.y, self.pressure()),
            Gesture::Pan { last } => {
                let a = self.view.to_canvas(vp, ppp, last);
                self.view.center -= c - a;
                self.gesture = Gesture::Pan { last: pos };
            }
            Gesture::Zoom { start, start_zoom } => {
                let z = start_zoom * 2f32.powf((pos.x - start.x) / 120.0);
                self.view.zoom_at(vp, ppp, start, z);
            }
            Gesture::Rotate {
                start_angle,
                start_rot,
            } => {
                let d = pos - vp.center();
                if d.length() > 4.0 {
                    let mut r = start_rot + d.y.atan2(d.x) - start_angle;
                    if m.shift {
                        let step = 15f32.to_radians();
                        r = (r / step).round() * step;
                    }
                    self.view.set_rotation(r);
                }
            }
            Gesture::Move { start } => {
                let mut d = c - start;
                if m.shift {
                    // Constrain to the dominant axis.
                    if d.x.abs() > d.y.abs() {
                        d.y = 0.0;
                    } else {
                        d.x = 0.0;
                    }
                }
                self.ed
                    .set_transform(Affine::translate(d.x.round(), d.y.round()), false);
            }
            Gesture::Handle {
                handle,
                start,
                start_delta,
            } => {
                if let Some((frame, size)) = self.ed.transforming().map(|t| (t.frame, t.size)) {
                    let mut p = c;
                    if handle == Handle::Inside && m.shift {
                        let d = c - start;
                        p = if d.x.abs() > d.y.abs() {
                            Pos2::new(c.x, start.y)
                        } else {
                            Pos2::new(start.x, c.y)
                        };
                    }
                    let d = handles::drag(
                        handle,
                        frame,
                        size,
                        start_delta,
                        start,
                        p,
                        m.shift,
                        m.alt,
                        m.shift,
                    );
                    let smooth = self.prefs.smooth_transform;
                    let raster = self.ed.transforming().is_some_and(|t| t.is_raster());
                    let d = if handle == Handle::Inside && raster {
                        // Whole-pixel moves keep raster pixels crisp.
                        let mut d = d;
                        d.m[4] = d.m[4].round();
                        d.m[5] = d.m[5].round();
                        d
                    } else {
                        d
                    };
                    self.ed.set_transform(d, smooth);
                }
            }
            Gesture::SelectBox { start, .. } => {
                let mut end = c;
                if m.shift && self.press_op != SelectOp::Add {
                    let d = c - start;
                    let s = d.x.abs().max(d.y.abs());
                    end = start + egui::vec2(s * d.x.signum(), s * d.y.signum());
                }
                self.gesture = Gesture::SelectBox { start, end };
            }
            Gesture::Lasso => {
                if let Some(last) = self.lasso.last() {
                    let dist = (self.view.to_screen(vp, ppp, *last) - pos).length();
                    if dist > 1.5 {
                        self.lasso.push(c);
                    }
                }
            }
            Gesture::Gradient { start, .. } => {
                let mut end = c;
                if m.shift {
                    let d = c - start;
                    let a = (d.y.atan2(d.x) / std::f32::consts::FRAC_PI_4).round()
                        * std::f32::consts::FRAC_PI_4;
                    end = start + egui::vec2(a.cos(), a.sin()) * d.length();
                }
                self.gesture = Gesture::Gradient { start, end };
                self.ed.update_filter(Arc::new(self.gradient(start, end)));
            }
            Gesture::Crop {
                handle,
                start,
                start_rect,
            } => {
                let (w, h) = (self.ed.doc.width as f32, self.ed.doc.height as f32);
                self.crop = Some(handles::drag_rect(handle, start_rect, c - start, w, h));
            }
            Gesture::Pick => self.pick(c),
            Gesture::MoveSelection { start, .. } => {
                let mut end = c;
                if m.shift {
                    let d = c - start;
                    if d.x.abs() > d.y.abs() {
                        end.y = start.y;
                    } else {
                        end.x = start.x;
                    }
                }
                self.gesture = Gesture::MoveSelection { start, end };
            }
        }
    }

    fn release(&mut self, _ctx: &egui::Context) {
        let (w, h) = (self.ed.doc.width, self.ed.doc.height);
        match self.gesture {
            Gesture::Stroke => {
                self.ed.end_stroke();
                if self.prefs.tool == Tool::Brush {
                    self.prefs.remember_color(self.prefs.fg);
                }
            }
            Gesture::Move { .. } => self.ed.commit_transform(),
            Gesture::Handle { .. } => {
                if !self.free_transform {
                    self.ed.commit_transform();
                }
            }
            Gesture::SelectBox { start, end } => {
                let op = self.press_op;
                if (end - start).length() < 2.0 {
                    if op == SelectOp::Replace {
                        self.ed.deselect();
                    }
                } else {
                    let s = if self.prefs.select_shape == SelectShape::Ellipse {
                        Selection::ellipse(
                            w,
                            h,
                            start.x,
                            start.y,
                            end.x,
                            end.y,
                            self.prefs.select_antialias,
                        )
                    } else {
                        Selection::rect(w, h, start.x, start.y, end.x, end.y)
                    };
                    self.ed.select_shape(s, op);
                }
            }
            Gesture::Lasso => {
                let pts: Vec<(f32, f32)> = self.lasso.iter().map(|p| (p.x, p.y)).collect();
                if pts.len() >= 3 {
                    let s = Selection::polygon(w, h, &pts, self.prefs.select_antialias);
                    self.ed.select_shape(s, self.press_op);
                } else if self.press_op == SelectOp::Replace {
                    self.ed.deselect();
                }
                self.lasso.clear();
            }
            Gesture::Gradient { start, end } => {
                if (end - start).length() < 1.0 {
                    self.ed.cancel_filter();
                } else {
                    self.ed.commit_filter("Gradient");
                    self.prefs.remember_color(self.prefs.fg);
                }
            }
            Gesture::Crop { .. } => {
                if let Some(r) = self.crop
                    && (r[2] - r[0] < 1.0 || r[3] - r[1] < 1.0)
                {
                    self.crop = None;
                }
            }
            Gesture::Pick => self.prefs.remember_color(self.prefs.fg),
            Gesture::MoveSelection { start, end } => {
                let (dx, dy) = ((end.x - start.x).round(), (end.y - start.y).round());
                if dx != 0.0 || dy != 0.0 {
                    self.ed.translate_selection(dx as i64, dy as i64);
                } else if self.prefs.select_shape == SelectShape::Color {
                    if start.x >= 0.0 && start.y >= 0.0 {
                        self.ed.select_color(
                            start.x as u32,
                            start.y as u32,
                            self.prefs.color_tolerance,
                            self.prefs.select_antialias,
                            self.prefs.wand_sample_all,
                            self.select_mode,
                        );
                    }
                } else if self.prefs.select_shape == SelectShape::Wand {
                    // A click (not a drag) inside still means "select here".
                    if start.x >= 0.0 && start.y >= 0.0 {
                        self.ed.magic_wand(
                            start.x as u32,
                            start.y as u32,
                            self.prefs.wand_tolerance,
                            self.prefs.wand_sample_all,
                            self.select_mode,
                        );
                    }
                } else if self.select_mode == SelectOp::Replace {
                    self.ed.deselect();
                }
            }
            _ => {}
        }
        self.gesture = Gesture::None;
    }

    pub fn apply_crop(&mut self) {
        if let Some(r) = self.crop.take() {
            let (w, h) = (self.ed.doc.width as f32, self.ed.doc.height as f32);
            let r = handles::normalize(r, w, h);
            if r[2] - r[0] >= 1.0 && r[3] - r[1] >= 1.0 && r != [0.0, 0.0, w, h] {
                self.ed
                    .crop(r[0] as u32, r[1] as u32, r[2] as u32, r[3] as u32);
            }
        }
    }

    // ---- view commands --------------------------------------------------------

    pub fn zoom_step(&mut self, ctx: &egui::Context, vp: Rect, dir: f32) {
        // Steps through a familiar ladder of zoom levels.
        const STEPS: [f32; 21] = [
            1.0 / 32.0,
            1.0 / 24.0,
            1.0 / 16.0,
            1.0 / 12.0,
            1.0 / 8.0,
            1.0 / 6.0,
            0.25,
            1.0 / 3.0,
            0.5,
            2.0 / 3.0,
            1.0,
            1.5,
            2.0,
            3.0,
            4.0,
            6.0,
            8.0,
            12.0,
            16.0,
            32.0,
            64.0,
        ];
        let z = self.view.zoom;
        let next = if dir > 0.0 {
            STEPS
                .iter()
                .copied()
                .find(|s| *s > z * 1.001)
                .unwrap_or(64.0)
        } else {
            STEPS
                .iter()
                .rev()
                .copied()
                .find(|s| *s < z * 0.999)
                .unwrap_or(1.0 / 32.0)
        };
        let ppp = ctx.pixels_per_point();
        self.view.zoom_at(vp, ppp, vp.center(), next);
    }

    // ---- shortcuts ----------------------------------------------------------

    pub fn shortcuts(&mut self, ctx: &egui::Context, vp: Rect) {
        let cmd = Modifiers::COMMAND;
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        let ca = Modifiers::COMMAND | Modifiers::ALT;
        // Modifiers must match exactly, so Ctrl+Shift+N is never taken for Ctrl+N.
        let sc = |m: Modifiers, k: Key| ctx.input_mut(|i| consume_exact(i, m, k));

        // Works even while typing.
        if sc(cmd, Key::S) {
            self.save(ctx);
        }
        if sc(cs, Key::S) {
            self.save_as(ctx);
        }
        if self.modal.is_some() {
            return;
        }
        if self.text_edit.is_some() && sc(Modifiers::NONE, Key::Escape) {
            self.finish_modes();
            return;
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        // Clipboard arrives as events (the platform maps the keys).
        let clip = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Copy => Some(0),
                    egui::Event::Cut => Some(1),
                    egui::Event::Paste(_) => Some(2),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        for c in clip {
            match c {
                0 => self.copy(ctx, false, false),
                1 => self.copy(ctx, false, true),
                _ => {
                    if !self.paste_internal() {
                        self.toast(ctx, "The clipboard has no image", true);
                    }
                }
            }
        }
        if sc(cs, Key::C) {
            self.copy(ctx, true, false);
        }
        if sc(cmd, Key::N) {
            self.request(ctx, Pending::New);
        }
        if sc(cmd, Key::O) {
            self.request(ctx, Pending::Open(None));
        }
        if sc(cs, Key::E) {
            self.modal = Some(Modal::Export);
        }
        if sc(cs, Key::Z) || sc(cmd, Key::Y) {
            self.redo();
        }
        if sc(cmd, Key::Z) {
            if self.free_transform || self.crop.is_some() {
                self.cancel_modes();
            } else {
                self.undo();
            }
        }
        let active = self.ed.doc.active_id();
        if sc(cs, Key::N) {
            self.ed.add_layer();
        }
        let ids = self.ed.selected_layers();
        if sc(cmd, Key::J) {
            self.finish_modes();
            if self.ed.selection().is_some() {
                self.ed.layer_via_copy();
            } else {
                self.ed.duplicate_layers(&ids);
            }
        }
        if sc(cmd, Key::E) {
            self.finish_modes();
            self.ed.merge_selected();
        }
        if sc(cmd, Key::G) {
            self.finish_modes();
            self.ed.group_layers(&ids);
        }
        if sc(cs, Key::G) {
            self.ed.ungroup(active);
        }
        if sc(ca, Key::G) {
            self.ed.toggle_clip(active);
        }
        if sc(cmd, Key::A) {
            self.ed.select_all();
        }
        if sc(cmd, Key::D) {
            self.ed.deselect();
        }
        if sc(cs, Key::I) {
            self.ed.invert_selection();
        }
        if sc(cmd, Key::T) {
            self.start_free_transform(ctx);
        }
        if sc(cmd, Key::Quote) {
            self.prefs.pixel_grid = !self.prefs.pixel_grid;
        }
        if sc(cmd, Key::Num0) {
            self.fit_pending = true;
        }
        if sc(cmd, Key::Num1) {
            let ppp = ctx.pixels_per_point();
            self.view.zoom_at(vp, ppp, vp.center(), 1.0);
        }
        let zoom_in =
            |k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(cmd, k)));
        if zoom_in(Key::Plus) || zoom_in(Key::Equals) {
            self.zoom_step(ctx, vp, 1.0);
        }
        if sc(cmd, Key::Minus) {
            self.zoom_step(ctx, vp, -1.0);
        }

        let key = |k: Key| sc(Modifiers::NONE, k);
        let shift_key = |k: Key| sc(Modifiers::SHIFT, k);
        let alt_key = |k: Key| sc(Modifiers::ALT, k);
        if key(Key::Enter) {
            if self.free_transform {
                self.ed.commit_transform();
                self.free_transform = false;
            } else if self.crop.is_some() {
                self.apply_crop();
            }
        }
        if key(Key::Escape) {
            if self.free_transform || self.crop.is_some() || self.gesture != Gesture::None {
                self.cancel_modes();
            } else if self.ed.selection().is_some() {
                self.ed.deselect();
            }
        }
        for (k, t) in [
            (Key::V, Tool::Move),
            (Key::B, Tool::Brush),
            (Key::E, Tool::Eraser),
            (Key::G, Tool::Fill),
            (Key::T, Tool::Text),
            (Key::I, Tool::Picker),
            (Key::C, Tool::Crop),
            (Key::H, Tool::Hand),
        ] {
            if key(k) {
                self.set_tool(t);
            }
        }
        if shift_key(Key::G) {
            self.set_tool(Tool::Gradient);
        }
        if key(Key::M) {
            // M cycles rectangle ↔ ellipse when already selecting.
            if self.prefs.tool == Tool::Select && self.prefs.select_shape == SelectShape::Rect {
                self.prefs.select_shape = SelectShape::Ellipse;
            } else {
                self.prefs.select_shape = SelectShape::Rect;
            }
            self.set_tool(Tool::Select);
        }
        if key(Key::L) {
            self.prefs.select_shape = SelectShape::Lasso;
            self.set_tool(Tool::Select);
        }
        if key(Key::W) {
            self.prefs.select_shape = SelectShape::Wand;
            self.set_tool(Tool::Select);
        }
        if shift_key(Key::W) {
            self.prefs.select_shape = SelectShape::Color;
            self.set_tool(Tool::Select);
        }
        if key(Key::X) {
            self.swap_colors();
        }
        if key(Key::D) {
            self.set_fg([0, 0, 0]);
            self.prefs.bg = [255, 255, 255];
        }
        if shift_key(Key::M) {
            self.view.toggle_mirror();
        }
        if key(Key::P) {
            self.toggle_play(ctx);
        }
        if key(Key::Comma) {
            let f = self.ed.doc.state.frame;
            self.ed.set_frame(f.saturating_sub(1));
        }
        if key(Key::Period) {
            let f = self.ed.doc.state.frame + 1;
            self.ed.set_frame(f.min(self.ed.doc.state.frames - 1));
        }
        if key(Key::Tab) {
            self.ui_hidden = !self.ui_hidden;
        }
        if key(Key::F11) {
            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
        }
        if (key(Key::Delete) || key(Key::Backspace))
            && let Err(r) = {
                self.finish_modes();
                self.ed.delete_key()
            }
        {
            self.refusal(ctx, r);
        }
        if alt_key(Key::Backspace) || alt_key(Key::Delete) {
            let c = self.prefs.fg;
            if let Err(r) = self.ed.fill_selection(c) {
                self.refusal(ctx, r);
            }
        }
        // Arrow keys nudge: the selection outline with the select tool, the
        // layer (or selected pixels) with the move tool. Shift: 10 px.
        for (k, dx, dy) in [
            (Key::ArrowLeft, -1, 0),
            (Key::ArrowRight, 1, 0),
            (Key::ArrowUp, 0, -1),
            (Key::ArrowDown, 0, 1),
        ] {
            let n = if shift_key(k) {
                10
            } else if key(k) {
                1
            } else {
                continue;
            };
            let (dx, dy) = (dx * n, dy * n);
            match self.prefs.tool {
                Tool::Select => self.ed.translate_selection(dx as i64, dy as i64),
                Tool::Move if self.gesture == Gesture::None => {
                    if self.free_transform {
                        if let Some(d) = self.ed.transforming().map(|t| t.delta) {
                            let d = Affine::translate(dx as f32, dy as f32).compose(d);
                            self.ed.set_transform(d, self.prefs.smooth_transform);
                        }
                    } else {
                        match self.ed.begin_transform(false) {
                            Ok(()) => {
                                self.ed
                                    .set_transform(Affine::translate(dx as f32, dy as f32), false);
                                self.ed.commit_transform();
                            }
                            Err(r) => self.refusal(ctx, r),
                        }
                    }
                }
                _ => {}
            }
        }
        if key(Key::Slash) {
            self.ed
                .set_layer(active, "Alpha lock", |l| l.alpha_lock = !l.alpha_lock);
        }
        if key(Key::F5) {
            self.toggle_brush_editor();
        }
        if key(Key::F1) {
            self.modal = Some(Modal::Shortcuts);
        }
        // Brush size and hardness, Photoshop-style.
        if matches!(self.prefs.tool, Tool::Brush | Tool::Eraser) {
            let b = self.current_brush_mut();
            let pixel = b.pixel;
            if key(Key::OpenBracket) {
                b.size = if pixel {
                    (b.size - 1.0).max(1.0)
                } else {
                    (b.size / 1.18).max(1.0)
                };
            }
            if key(Key::CloseBracket) {
                b.size = if pixel {
                    (b.size + 1.0).min(64.0)
                } else {
                    (b.size * 1.18).min(2000.0)
                };
            }
            if shift_key(Key::OpenBracket) {
                b.hardness = (b.hardness - 0.1).max(0.0);
            }
            if shift_key(Key::CloseBracket) {
                b.hardness = (b.hardness + 0.1).min(1.0);
            }
            // Number keys set opacity: 1 = 10% … 0 = 100%.
            let digits = [
                Key::Num1,
                Key::Num2,
                Key::Num3,
                Key::Num4,
                Key::Num5,
                Key::Num6,
                Key::Num7,
                Key::Num8,
                Key::Num9,
                Key::Num0,
            ];
            for (i, k) in digits.into_iter().enumerate() {
                if key(k) {
                    self.current_brush_mut().opacity = (i + 1) as f32 / 10.0;
                }
            }
        }
    }
}

pub const SHORTCUTS: &[(&str, &str)] = &[
    (
        "V  M  L  W  C",
        "Move, Select (M again: ellipse), Lasso, Magic wand, Crop · Shift+W: select by colour",
    ),
    ("B  E  G  ⇧G  T", "Brush, Eraser, Fill, Gradient, Text"),
    ("I  H", "Eyedropper, Hand"),
    ("Space + drag", "Pan (or middle mouse / trackpad scroll)"),
    ("Ctrl+Space + drag", "Zoom (or mouse wheel / pinch)"),
    ("R + drag", "Rotate view (Shift snaps to 15°)"),
    ("Shift+M", "Mirror view"),
    ("Alt", "Eyedropper while held"),
    ("Ctrl", "Move while held"),
    ("Shift + click", "Straight line from the last stroke"),
    ("[  ]  ·  Shift + [  ]", "Brush size · hardness"),
    ("1 … 0", "Brush opacity 10% … 100%"),
    ("F5", "Brush editor"),
    ("X / D", "Swap / reset colours"),
    ("Ctrl+T", "Free transform (Enter applies, Esc cancels)"),
    (
        "Ctrl+A / Ctrl+D / Ctrl+Shift+I",
        "Select all / deselect / invert",
    ),
    (
        "Shift / Alt while selecting",
        "Add to / subtract from selection",
    ),
    (
        "Del  ·  Alt+Del",
        "Clear selection (deletes text, images and groups) · fill with colour",
    ),
    (
        "Ctrl+C / X / V",
        "Copy / cut / paste (Ctrl+Shift+C copies merged)",
    ),
    (
        "Arrows  ·  Shift+Arrows",
        "Nudge layer (move tool) or selection outline (select tool) by 1 · 10 px",
    ),
    ("Ctrl+J", "Duplicate layer (or copy selection to a layer)"),
    (
        "Ctrl / Shift + click layer",
        "Select several layers · a range of layers",
    ),
    ("Ctrl+G / Ctrl+Shift+G", "Group / ungroup"),
    ("Ctrl+Alt+G", "Clip to layer below"),
    ("Ctrl+E", "Merge down"),
    ("/", "Lock layer alpha"),
    ("Ctrl+Z / Ctrl+Shift+Z", "Undo / redo"),
    ("Ctrl+Shift+N", "New layer"),
    ("P  ·  ,  .", "Play animation · previous / next frame"),
    ("Ctrl+0 / Ctrl+1", "Fit / 100%"),
    ("Ctrl+'", "Pixel grid"),
    ("Ctrl+S / Ctrl+Shift+E", "Save / export"),
    ("Tab / F11", "Hide panels / fullscreen"),
];
