//! Application state, input handling and file operations.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, Pos2, Rect, pos2, vec2};
use egaku_core::brush::{BrushParams, Paint, PaintMode};
use egaku_core::color::Hsv;
use egaku_core::document::{Document, LayerId};
use egaku_core::editor::Refusal;
use egaku_core::fill::FillParams;
use egaku_core::tile::TileGrid;
use egaku_core::{Editor, io};

use crate::canvas::{CanvasTextures, View, visible_tiles};
use crate::prefs::{Prefs, Tool};
use crate::theme;

/// What the pointer is currently doing on the canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    None,
    Stroke,
    Pan { last: Pos2 },
    Zoom { start: Pos2, start_zoom: f32 },
    Rotate { start_angle: f32, start_rot: f32 },
    Move { start: Pos2 },
    Pick,
}

/// A tool, possibly temporarily overridden by a held key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Active {
    Tool(Tool),
    Zoom,
    Rotate,
}

pub enum Pending {
    New,
    Open(Option<PathBuf>),
    Quit,
}

pub enum Modal {
    NewDoc,
    Unsaved(Pending),
    Error(String),
    Shortcuts,
}

enum IoDone {
    Saved {
        path: PathBuf,
        state_id: u64,
        doc_gen: u64,
        result: Result<(), String>,
    },
    Exported {
        path: PathBuf,
        result: Result<(), String>,
    },
    Opened {
        path: PathBuf,
        result: Result<Document, String>,
    },
    Imported {
        name: String,
        result: Result<TileGrid, String>,
    },
}

pub struct Toast {
    pub text: String,
    pub warn: bool,
    pub until: f64,
}

pub struct App {
    pub ed: Editor,
    pub path: Option<PathBuf>,
    pub name: String,
    pub view: View,
    pub fit_pending: bool,
    pub textures: CanvasTextures,
    pub prefs: Prefs,
    /// Foreground colour in HSV, kept separately so hue survives greys.
    pub fg_hsv: Hsv,
    pub hex_edit: Option<String>,
    pub gesture: Gesture,
    pub pen_pressure: Option<f32>,
    pub hover: Option<Pos2>,
    pub ui_hidden: bool,
    pub toast: Option<Toast>,
    pub modal: Option<Modal>,
    pub thumbs: HashMap<LayerId, (usize, egui::TextureHandle)>,
    pub renaming: Option<(LayerId, String)>,
    pub layer_drag: Option<usize>,
    io_tx: Sender<IoDone>,
    io_rx: Receiver<IoDone>,
    pub io_busy: Option<String>,
    quit_after_save: bool,
    allow_close: bool,
    doc_gen: u64,
    last_title: String,
}

const PREFS_KEY: &str = "egaku-prefs";

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        let prefs: Prefs = cc
            .storage
            .and_then(|s| eframe::get_value(s, PREFS_KEY))
            .unwrap_or_default();
        let doc = Document::new(prefs.new_width, prefs.new_height, prefs.new_paper);
        let (io_tx, io_rx) = channel();
        let mut app = App {
            ed: Editor::new(doc),
            path: None,
            name: "Untitled".into(),
            view: View::default(),
            fit_pending: true,
            textures: CanvasTextures::default(),
            fg_hsv: crate::color_picker::to_hsv(prefs.fg),
            prefs,
            hex_edit: None,
            gesture: Gesture::None,
            pen_pressure: None,
            hover: None,
            ui_hidden: false,
            toast: None,
            modal: None,
            thumbs: HashMap::new(),
            renaming: None,
            layer_drag: None,
            io_tx,
            io_rx,
            io_busy: None,
            quit_after_save: false,
            allow_close: false,
            doc_gen: 0,
            last_title: String::new(),
        };
        if let Some(p) = open {
            app.open_path(&cc.egui_ctx, p);
        }
        app
    }

    // ---- helpers ------------------------------------------------------------

    pub fn toast(&mut self, ctx: &egui::Context, text: impl Into<String>, warn: bool) {
        let now = ctx.input(|i| i.time);
        self.toast = Some(Toast {
            text: text.into(),
            warn,
            until: now + if warn { 3.0 } else { 2.0 },
        });
        ctx.request_repaint();
    }

    pub fn brush_params(&self, tool: Tool) -> &BrushParams {
        if tool == Tool::Eraser {
            &self.prefs.eraser
        } else {
            &self.prefs.brush
        }
    }

    pub fn current_brush_mut(&mut self) -> &mut BrushParams {
        if self.prefs.tool == Tool::Eraser {
            &mut self.prefs.eraser
        } else {
            &mut self.prefs.brush
        }
    }

    pub fn set_fg(&mut self, c: [u8; 3]) {
        self.prefs.fg = c;
        self.fg_hsv = crate::color_picker::to_hsv(c);
        self.hex_edit = None;
    }

    pub fn swap_colors(&mut self) {
        let (f, b) = (self.prefs.fg, self.prefs.bg);
        self.prefs.bg = f;
        self.set_fg(b);
    }

    fn refusal(&mut self, ctx: &egui::Context, r: Refusal) {
        let msg = match r {
            Refusal::LayerHidden => "This layer is hidden — show it to paint on it",
            Refusal::LayerEmpty => "Nothing to move on this layer",
            Refusal::Busy => return,
        };
        self.toast(ctx, msg, true);
    }

    /// Which tool a press would use, taking held keys into account.
    pub fn active_tool(&self, ctx: &egui::Context) -> Active {
        if ctx.egui_wants_keyboard_input() {
            return Active::Tool(self.prefs.tool);
        }
        let (m, space, r) =
            ctx.input(|i| (i.modifiers, i.key_down(Key::Space), i.key_down(Key::R)));
        if space && m.command {
            Active::Zoom
        } else if (space && m.shift) || r {
            Active::Rotate
        } else if space {
            Active::Tool(Tool::Hand)
        } else if m.alt && matches!(self.prefs.tool, Tool::Brush | Tool::Eraser | Tool::Fill) {
            Active::Tool(Tool::Picker)
        } else {
            Active::Tool(self.prefs.tool)
        }
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
        ui.painter().rect_filled(vp, 0.0, theme::BG_CANVAS);
        let ppp = ctx.pixels_per_point();
        if self.fit_pending && vp.width() > 64.0 {
            self.view
                .fit(vp, ppp, self.ed.doc.width, self.ed.doc.height);
            self.fit_pending = false;
        }

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
                } => {
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
                } if over => match button {
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
        if self.gesture == Gesture::Stroke && !ctx.input(|i| i.pointer.primary_down()) {
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

    fn press(&mut self, ctx: &egui::Context, vp: Rect, pos: Pos2, m: Modifiers, active: Active) {
        let ppp = ctx.pixels_per_point();
        let c = self.view.to_canvas(vp, ppp, pos);
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
                Tool::Picker => {
                    self.gesture = Gesture::Pick;
                    self.pick(c);
                }
                Tool::Move => match self.ed.begin_move() {
                    Ok(()) => self.gesture = Gesture::Move { start: c },
                    Err(r) => self.refusal(ctx, r),
                },
                Tool::Hand => self.gesture = Gesture::Pan { last: pos },
            },
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
                    if ctx.input(|i| i.modifiers.shift) {
                        let step = 15f32.to_radians();
                        r = (r / step).round() * step;
                    }
                    self.view.set_rotation(r);
                }
            }
            Gesture::Move { start } => {
                let d = c - start;
                self.ed.move_to(d.x.round() as i64, d.y.round() as i64);
            }
            Gesture::Pick => self.pick(c),
        }
    }

    fn release(&mut self, _ctx: &egui::Context) {
        match self.gesture {
            Gesture::Stroke => {
                self.ed.end_stroke();
                if self.prefs.tool == Tool::Brush {
                    self.prefs.remember_color(self.prefs.fg);
                }
            }
            Gesture::Move { .. } => self.ed.end_move(),
            Gesture::Pick => self.prefs.remember_color(self.prefs.fg),
            _ => {}
        }
        self.gesture = Gesture::None;
    }

    fn canvas_overlay(&mut self, ui: &egui::Ui, vp: Rect, resp: &egui::Response) {
        let ctx = ui.ctx();
        let painter = ui.painter_at(vp);
        let ppp = ctx.pixels_per_point();
        let active = self.active_tool(ctx);
        let over = resp.contains_pointer() || self.gesture != Gesture::None;

        if over {
            use egui::CursorIcon as C;
            let icon = match (active, self.gesture) {
                (_, Gesture::Pan { .. }) => C::Grabbing,
                (Active::Zoom, _) => C::ZoomIn,
                (Active::Rotate, _) => C::AllScroll,
                (Active::Tool(Tool::Hand), _) => C::Grab,
                (Active::Tool(Tool::Move), _) => C::Move,
                (Active::Tool(Tool::Brush | Tool::Eraser), _) => C::None,
                _ => C::Crosshair,
            };
            ctx.set_cursor_icon(icon);
        }

        if let (Some(p), true) = (self.hover, over) {
            if let Active::Tool(t @ (Tool::Brush | Tool::Eraser)) = active {
                let r = self.brush_params(t).size * 0.5 * self.view.scale(ppp);
                if r > 3.0 {
                    painter.circle_stroke(
                        p,
                        r + 0.5,
                        egui::Stroke::new(1.0, egui::Color32::from_black_alpha(140)),
                    );
                    painter.circle_stroke(
                        p,
                        r - 0.5,
                        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(200)),
                    );
                }
                // A tiny crosshair marks the exact hotspot at any size.
                let k = 3.0;
                for (a, b) in [(vec2(-k, 0.0), vec2(k, 0.0)), (vec2(0.0, -k), vec2(0.0, k))] {
                    painter.line_segment(
                        [p + a, p + b],
                        egui::Stroke::new(2.5, egui::Color32::from_black_alpha(120)),
                    );
                    painter
                        .line_segment([p + a, p + b], egui::Stroke::new(1.0, egui::Color32::WHITE));
                }
            }
            if let Active::Tool(Tool::Picker) = active {
                let c = self.prefs.fg;
                let col = egui::Color32::from_rgb(c[0], c[1], c[2]);
                painter.circle_filled(p + vec2(18.0, -18.0), 9.0, col);
                painter.circle_stroke(
                    p + vec2(18.0, -18.0),
                    9.0,
                    egui::Stroke::new(1.5, egui::Color32::WHITE),
                );
            }
        }

        // Rotation indicator while rotating or when rotated.
        if matches!(self.gesture, Gesture::Rotate { .. }) {
            let c = vp.center();
            let d = vec2(self.view.rotation.cos(), self.view.rotation.sin()) * 40.0;
            painter.circle_stroke(
                c,
                40.0,
                egui::Stroke::new(1.0, egui::Color32::from_white_alpha(90)),
            );
            painter.line_segment(
                [c, c + vec2(d.y, -d.x)],
                egui::Stroke::new(2.0, theme::ACCENT),
            );
            painter.text(
                c + vec2(0.0, 56.0),
                egui::Align2::CENTER_CENTER,
                format!("{:.0}°", self.view.rotation.to_degrees()),
                egui::FontId::proportional(13.0),
                egui::Color32::WHITE,
            );
        }

        if let Some((dx, dy)) = self.ed.move_offset()
            && let Some(p) = self.hover
        {
            painter.text(
                p + vec2(16.0, 16.0),
                egui::Align2::LEFT_TOP,
                format!("{dx:+}, {dy:+}"),
                egui::FontId::monospace(12.0),
                egui::Color32::WHITE,
            );
        }

        // Toast.
        let now = ctx.input(|i| i.time);
        if let Some(t) = &self.toast {
            if now > t.until {
                self.toast = None;
            } else {
                let alpha = ((t.until - now) * 3.0).min(1.0) as f32;
                let galley = painter.layout_no_wrap(
                    t.text.clone(),
                    egui::FontId::proportional(14.0),
                    if t.warn { theme::WARN } else { theme::TEXT },
                );
                let size = galley.size() + vec2(24.0, 14.0);
                let r = Rect::from_center_size(pos2(vp.center().x, vp.bottom() - 40.0), size);
                painter.rect_filled(
                    r,
                    8.0,
                    egui::Color32::from_black_alpha((200.0 * alpha) as u8),
                );
                painter.galley(r.min + vec2(12.0, 7.0), galley, egui::Color32::WHITE);
                ctx.request_repaint();
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
        // Modifiers must match exactly, so Ctrl+Shift+N is never taken for Ctrl+N.
        let sc = |m: Modifiers, k: Key| ctx.input_mut(|i| consume_exact(i, m, k));

        // Works even while typing.
        if sc(cmd, Key::S) {
            self.save(ctx);
        }
        if sc(cs, Key::S) {
            self.save_as(ctx);
        }
        if ctx.egui_wants_keyboard_input() || self.modal.is_some() {
            return;
        }
        if sc(cmd, Key::N) {
            self.request(ctx, Pending::New);
        }
        if sc(cmd, Key::O) {
            self.request(ctx, Pending::Open(None));
        }
        if sc(cs, Key::E) {
            self.export_png(ctx);
        }
        if sc(cs, Key::Z) || sc(cmd, Key::Y) {
            self.ed.redo();
        }
        if sc(cmd, Key::Z) {
            self.ed.undo();
        }
        if sc(cs, Key::N) {
            self.ed.add_layer();
        }
        if sc(cmd, Key::J) {
            self.ed.duplicate_layer();
        }
        if sc(cmd, Key::E) {
            self.ed.merge_down();
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
        for t in Tool::ALL {
            let k = Key::from_name(t.key()).unwrap();
            if key(k) {
                self.prefs.tool = t;
            }
        }
        if key(Key::X) {
            self.swap_colors();
        }
        if key(Key::D) {
            self.set_fg([0, 0, 0]);
            self.prefs.bg = [255, 255, 255];
        }
        if key(Key::M) {
            self.view.toggle_mirror();
        }
        if key(Key::Tab) {
            self.ui_hidden = !self.ui_hidden;
        }
        if key(Key::F11) {
            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
        }
        if key(Key::Escape) {
            self.ed.cancel_stroke();
            self.ed.cancel_move();
            self.gesture = Gesture::None;
        }
        if key(Key::Delete) || key(Key::Backspace) {
            self.ed.clear_layer();
        }
        if key(Key::Slash) {
            let v = !self.ed.doc.active_layer().alpha_lock;
            self.ed
                .commit("Alpha lock", |d| d.active_layer_mut().alpha_lock = v);
        }
        if key(Key::F1) {
            self.modal = Some(Modal::Shortcuts);
        }
        // Brush size and hardness, Photoshop-style.
        if matches!(self.prefs.tool, Tool::Brush | Tool::Eraser) {
            let b = self.current_brush_mut();
            if key(Key::OpenBracket) {
                b.size = (b.size / 1.18).max(1.0);
            }
            if key(Key::CloseBracket) {
                b.size = (b.size * 1.18).min(2000.0);
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

    // ---- files --------------------------------------------------------------

    fn spawn(
        &mut self,
        ctx: &egui::Context,
        label: &str,
        job: impl FnOnce() -> IoDone + Send + 'static,
    ) {
        self.io_busy = Some(label.to_string());
        let tx = self.io_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        });
    }

    pub fn poll_io(&mut self, ctx: &egui::Context) {
        while let Ok(done) = self.io_rx.try_recv() {
            self.io_busy = None;
            match done {
                IoDone::Saved {
                    path,
                    state_id,
                    doc_gen,
                    result,
                } => match result {
                    Ok(()) => {
                        if doc_gen == self.doc_gen {
                            self.ed.mark_saved_at(state_id);
                            self.name = file_name(&path);
                            self.path = Some(path.clone());
                        }
                        self.toast(ctx, format!("Saved {}", file_name(&path)), false);
                        if self.quit_after_save {
                            self.allow_close = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                    Err(e) => {
                        self.quit_after_save = false;
                        self.modal = Some(Modal::Error(format!(
                            "Could not save {}:\n{e}",
                            path.display()
                        )));
                    }
                },
                IoDone::Exported { path, result } => match result {
                    Ok(()) => self.toast(ctx, format!("Exported {}", file_name(&path)), false),
                    Err(e) => {
                        self.modal = Some(Modal::Error(format!(
                            "Could not export {}:\n{e}",
                            path.display()
                        )))
                    }
                },
                IoDone::Opened { path, result } => match result {
                    Ok(doc) => {
                        let native = is_ora(&path);
                        self.set_document(doc, native.then(|| path.clone()), file_name(&path));
                    }
                    Err(e) => {
                        self.modal = Some(Modal::Error(format!(
                            "Could not open {}:\n{e}",
                            path.display()
                        )))
                    }
                },
                IoDone::Imported { name, result } => match result {
                    Ok(grid) => {
                        let mut l = self.ed.doc.new_layer(name);
                        l.tiles = Arc::new(grid);
                        self.ed.insert_layer("Import layer", l);
                    }
                    Err(e) => {
                        self.modal = Some(Modal::Error(format!("Could not import image:\n{e}")))
                    }
                },
            }
        }
    }

    pub fn set_document(&mut self, doc: Document, path: Option<PathBuf>, name: String) {
        self.ed = Editor::new(doc);
        self.doc_gen += 1;
        self.path = path;
        self.name = name;
        self.textures.clear();
        self.thumbs.clear();
        self.renaming = None;
        self.gesture = Gesture::None;
        self.fit_pending = true;
    }

    /// Runs an action that would discard the document, asking first if it
    /// has unsaved changes.
    pub fn request(&mut self, ctx: &egui::Context, p: Pending) {
        if self.ed.is_modified() {
            self.modal = Some(Modal::Unsaved(p));
        } else {
            self.perform(ctx, p);
        }
    }

    pub fn perform(&mut self, ctx: &egui::Context, p: Pending) {
        match p {
            Pending::New => self.modal = Some(Modal::NewDoc),
            Pending::Open(path) => {
                let path = path.or_else(|| {
                    self.dialog()
                        .add_filter("Images", io::OPEN_EXTENSIONS)
                        .add_filter("OpenRaster", &["ora"])
                        .pick_file()
                });
                if let Some(p) = path {
                    self.open_path(ctx, p);
                }
            }
            Pending::Quit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn dialog(&self) -> rfd::FileDialog {
        let mut d = rfd::FileDialog::new();
        if let Some(dir) = self
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .or(self.prefs.last_dir.as_deref())
        {
            d = d.set_directory(dir);
        }
        d
    }

    fn remember_dir(&mut self, p: &Path) {
        self.prefs.last_dir = p.parent().map(Path::to_path_buf);
    }

    pub fn open_path(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.remember_dir(&path);
        let p = path.clone();
        self.spawn(ctx, "Opening…", move || IoDone::Opened {
            result: io::open(&p).map_err(|e| e.to_string()),
            path: p,
        });
    }

    pub fn import_layer(&mut self, ctx: &egui::Context, path: Option<PathBuf>) {
        let Some(path) = path.or_else(|| {
            self.dialog()
                .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
                .pick_file()
        }) else {
            return;
        };
        self.remember_dir(&path);
        let (w, h) = (self.ed.doc.width, self.ed.doc.height);
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Image")
            .to_string();
        self.spawn(ctx, "Importing…", move || IoDone::Imported {
            name,
            result: io::decode_to_grid(&path, w, h).map_err(|e| e.to_string()),
        });
    }

    /// Pastes a clipboard image as a new layer.
    pub fn paste_image(&mut self, img: &egui::ColorImage) {
        let [w, h] = img.size;
        let (cw, ch) = (self.ed.doc.width, self.ed.doc.height);
        // Clipboard images are premultiplied; the importer wants straight alpha.
        let rgba: Vec<u8> = img
            .pixels
            .iter()
            .flat_map(|c| egaku_core::color::unpremultiply(c.to_array()))
            .collect();
        let x = (cw as i64 - w as i64).max(0) as u32 / 2;
        let y = (ch as i64 - h as i64).max(0) as u32 / 2;
        let (w, h) = (w.min(cw as usize) as u32, h.min(ch as usize) as u32);
        // Crop to the canvas if the image is larger.
        let src_w = img.size[0];
        let cropped: Vec<u8> = (0..h as usize)
            .flat_map(|r| rgba[r * src_w * 4..(r * src_w + w as usize) * 4].to_vec())
            .collect();
        let grid = io::grid_from_rgba(cw, ch, &cropped, x, y, w, h);
        let mut l = self.ed.doc.new_layer("Pasted");
        l.tiles = Arc::new(grid);
        self.ed.insert_layer("Paste", l);
    }

    pub fn save(&mut self, ctx: &egui::Context) {
        match self.path.clone() {
            Some(p) if is_ora(&p) => self.save_to(ctx, p),
            _ => self.save_as(ctx),
        }
    }

    pub fn save_as(&mut self, ctx: &egui::Context) {
        let stem = self
            .name
            .rsplit_once('.')
            .map_or(self.name.as_str(), |(s, _)| s)
            .to_string();
        let Some(mut p) = self
            .dialog()
            .add_filter("OpenRaster", &["ora"])
            .set_file_name(format!("{stem}.ora"))
            .save_file()
        else {
            self.quit_after_save = false;
            return;
        };
        if !is_ora(&p) {
            p.set_extension("ora");
        }
        self.save_to(ctx, p);
    }

    fn save_to(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.io_busy.is_some() {
            return;
        }
        self.remember_dir(&path);
        // Snapshots are copy-on-write, so the save runs on a thread against
        // this exact state while painting continues.
        let doc = self.ed.doc.clone();
        let state_id = self.ed.state_id();
        let doc_gen = self.doc_gen;
        self.spawn(ctx, "Saving…", move || IoDone::Saved {
            result: io::ora::save(&doc, &path).map_err(|e| e.to_string()),
            path,
            state_id,
            doc_gen,
        });
    }

    pub fn export_png(&mut self, ctx: &egui::Context) {
        let stem = self
            .name
            .rsplit_once('.')
            .map_or(self.name.as_str(), |(s, _)| s)
            .to_string();
        let Some(mut p) = self
            .dialog()
            .add_filter("PNG", &["png"])
            .set_file_name(format!("{stem}.png"))
            .save_file()
        else {
            return;
        };
        if p.extension().is_none() {
            p.set_extension("png");
        }
        self.remember_dir(&p);
        let doc = self.ed.doc.clone();
        self.spawn(ctx, "Exporting…", move || IoDone::Exported {
            result: io::export_png(&doc, &p).map_err(|e| e.to_string()),
            path: p,
        });
    }

    pub fn handle_dropped_and_pasted(&mut self, ctx: &egui::Context) {
        let (dropped, pasted) = ctx.input(|i| {
            let pasted: Vec<Arc<egui::ColorImage>> = i
                .events
                .iter()
                .filter_map(|e| {
                    if let egui::Event::PasteImage(img) = e {
                        Some(img.clone())
                    } else {
                        None
                    }
                })
                .collect();
            (i.raw.dropped_files.clone(), pasted)
        });
        for img in pasted {
            self.paste_image(&img);
        }
        for f in dropped {
            let p = f.path().to_path_buf();
            if p.as_os_str().is_empty() {
                continue;
            }
            // A layered file replaces the document; a flat image joins it
            // as a new layer, which is almost always the intent.
            if is_ora(&p) {
                self.request(ctx, Pending::Open(Some(p)));
            } else {
                self.import_layer(ctx, Some(p));
            }
        }
    }

    pub fn handle_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested())
            && !self.allow_close
            && self.ed.is_modified()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.modal = Some(Modal::Unsaved(Pending::Quit));
        }
    }

    pub fn update_title(&mut self, ctx: &egui::Context) {
        let t = format!(
            "{}{} — egaku",
            if self.ed.is_modified() { "• " } else { "" },
            self.name
        );
        if t != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(t.clone()));
            self.last_title = t;
        }
    }

    // ---- modals -------------------------------------------------------------

    pub fn modals(&mut self, ctx: &egui::Context) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        match modal {
            Modal::NewDoc => {
                let r = egui::Modal::new(egui::Id::unique("new-doc")).show(ctx, |ui| {
                    ui.set_width(340.0);
                    ui.heading("New canvas");
                    ui.add_space(6.0);
                    let p = &mut self.prefs;
                    egui::Grid::new("new-grid")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Width");
                            ui.add(
                                egui::DragValue::new(&mut p.new_width)
                                    .range(1..=egaku_core::document::MAX_CANVAS)
                                    .suffix(" px"),
                            );
                            ui.end_row();
                            ui.label("Height");
                            ui.add(
                                egui::DragValue::new(&mut p.new_height)
                                    .range(1..=egaku_core::document::MAX_CANVAS)
                                    .suffix(" px"),
                            );
                            ui.end_row();
                            ui.label("Paper");
                            ui.horizontal(|ui| {
                                let mut on = p.new_paper.is_some();
                                if ui.checkbox(&mut on, "").changed() {
                                    p.new_paper = on.then_some([255; 3]);
                                }
                                if let Some(c) = &mut p.new_paper {
                                    ui.color_edit_button_srgb(c);
                                } else {
                                    ui.weak("transparent");
                                }
                            });
                            ui.end_row();
                        });
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        for (name, w, h) in [
                            ("HD", 1920, 1080),
                            ("4K", 3840, 2160),
                            ("A4 300dpi", 2480, 3508),
                            ("Square", 4096, 4096),
                            ("Large", 8192, 8192),
                        ] {
                            if ui.small_button(name).clicked() {
                                (p.new_width, p.new_height) = (w, h);
                            }
                        }
                        if ui
                            .small_button("Swap")
                            .on_hover_text("Swap width and height")
                            .clicked()
                        {
                            (p.new_width, p.new_height) = (p.new_height, p.new_width);
                        }
                    });
                    let mb = p.new_width as f64 * p.new_height as f64 * 4.0 / 1048576.0;
                    ui.add_space(2.0);
                    ui.weak(format!(
                        "{mb:.0} MB per fully painted layer; empty areas cost nothing."
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let create = ui.button("Create").clicked()
                            || ui.input(|i| i.key_pressed(Key::Enter));
                        let cancel = ui.button("Cancel").clicked();
                        (create, cancel)
                    })
                    .inner
                });
                let (create, cancel) = r.inner;
                if create {
                    let p = &self.prefs;
                    let doc = Document::new(p.new_width.max(1), p.new_height.max(1), p.new_paper);
                    self.set_document(doc, None, "Untitled".into());
                } else if !(cancel || r.should_close()) {
                    self.modal = Some(Modal::NewDoc);
                }
            }
            Modal::Unsaved(pending) => {
                let what = match pending {
                    Pending::Quit => "quitting",
                    Pending::New => "starting a new canvas",
                    Pending::Open(_) => "opening another file",
                };
                let r = egui::Modal::new(egui::Id::unique("unsaved")).show(ctx, |ui| {
                    ui.set_width(360.0);
                    ui.heading("Unsaved changes");
                    ui.add_space(4.0);
                    ui.label(format!("Save changes to “{}” before {what}?", self.name));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            return 1;
                        }
                        if ui.button("Don't save").clicked() {
                            return 2;
                        }
                        if ui.button("Cancel").clicked() {
                            return 3;
                        }
                        0
                    })
                    .inner
                });
                match (r.inner, r.should_close()) {
                    (1, _) => {
                        if matches!(pending, Pending::Quit) {
                            self.quit_after_save = true;
                        }
                        self.save(ctx);
                        // Saving runs in the background on a snapshot; carry on
                        // once it is queued (not if the save dialog was cancelled).
                        if !matches!(pending, Pending::Quit) && self.io_busy.is_some() {
                            self.perform(ctx, pending);
                        }
                    }
                    (2, _) => self.perform(ctx, pending),
                    (3, _) | (_, true) => {}
                    _ => self.modal = Some(Modal::Unsaved(pending)),
                }
            }
            Modal::Error(msg) => {
                let r = egui::Modal::new(egui::Id::unique("error")).show(ctx, |ui| {
                    ui.set_max_width(420.0);
                    ui.heading("Something went wrong");
                    ui.add_space(4.0);
                    ui.label(&msg);
                    ui.add_space(8.0);
                    ui.button("OK").clicked()
                });
                if !(r.inner || r.should_close()) {
                    self.modal = Some(Modal::Error(msg));
                }
            }
            Modal::Shortcuts => {
                let r = egui::Modal::new(egui::Id::unique("shortcuts")).show(ctx, |ui| {
                    ui.set_width(460.0);
                    ui.heading("Keyboard & pointer");
                    ui.add_space(6.0);
                    egui::Grid::new("keys")
                        .num_columns(2)
                        .striped(true)
                        .spacing([18.0, 4.0])
                        .show(ui, |ui| {
                            for (k, v) in SHORTCUTS {
                                ui.monospace(*k);
                                ui.label(*v);
                                ui.end_row();
                            }
                        });
                    ui.add_space(8.0);
                    ui.button("Close").clicked()
                });
                if !(r.inner || r.should_close()) {
                    self.modal = Some(Modal::Shortcuts);
                }
            }
        }
    }

    pub fn store(&self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, PREFS_KEY, &self.prefs);
    }
}

pub const SHORTCUTS: &[(&str, &str)] = &[
    (
        "B  E  G  I  V  H",
        "Brush, Eraser, Fill, Eyedropper, Move, Hand",
    ),
    ("Space + drag", "Pan (or middle mouse / trackpad scroll)"),
    ("Ctrl+Space + drag", "Zoom (or mouse wheel / pinch)"),
    ("R + drag", "Rotate view (Shift snaps to 15°)"),
    ("M", "Mirror view"),
    ("Alt", "Eyedropper while held"),
    ("Shift + click", "Straight line from the last stroke"),
    ("[  ]", "Brush size"),
    ("Shift + [  ]", "Brush hardness"),
    ("1 … 0", "Brush opacity 10% … 100%"),
    ("X / D", "Swap / reset colours"),
    ("/", "Lock layer alpha"),
    ("Del", "Clear layer"),
    ("Ctrl+Z / Ctrl+Shift+Z", "Undo / Redo"),
    ("Ctrl+Shift+N", "New layer"),
    ("Ctrl+J / Ctrl+E", "Duplicate / Merge down"),
    ("Ctrl+0 / Ctrl+1", "Fit / 100%"),
    ("Ctrl+S / Ctrl+Shift+E", "Save / Export PNG"),
    ("Ctrl+V", "Paste image as layer"),
    ("Tab / F11", "Hide panels / Fullscreen"),
];

/// Consumes a key press whose own modifiers match `m` exactly. Each event's
/// modifiers are checked rather than the frame's, since a quick chord can be
/// pressed and released within a single frame.
fn consume_exact(i: &mut egui::InputState, m: Modifiers, k: Key) -> bool {
    let mut hit = false;
    i.events.retain(|e| {
        if let egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } = e
            && !hit
            && *key == k
            && modifiers.matches_exact(m)
        {
            hit = true;
            return false;
        }
        true
    });
    hit
}

fn is_ora(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ora"))
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled")
        .to_string()
}
