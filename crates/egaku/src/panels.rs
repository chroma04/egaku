//! Menus, tool strip, side panel and status bar.

use eframe::egui::{self, Color32, Rect, Sense, Stroke, vec2};
use egaku_core::blend::BlendMode;
use egaku_core::thumb::thumbnail;

use crate::app::{App, Modal, Pending};
use crate::color_picker;
use crate::icons::{self, Icon};
use crate::prefs::{Tool, presets};
use crate::theme;

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(title.to_uppercase())
            .size(10.5)
            .color(theme::TEXT_DIM)
            .strong(),
    );
    ui.add_space(2.0);
}

/// A percentage slider over a 0..=1 value.
fn pct(ui: &mut egui::Ui, label: &str, v: &mut f32) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_sized([62.0, 18.0], egui::Label::new(label));
        let mut p = *v * 100.0;
        let r = ui.add(
            egui::Slider::new(&mut p, 0.0..=100.0)
                .suffix("%")
                .fixed_decimals(0),
        );
        if r.changed() {
            *v = p / 100.0;
        }
        r
    })
    .inner
}

impl App {
    pub fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            let sc = |s: &str| egui::RichText::new(s.to_string());
            let item = |ui: &mut egui::Ui, label: &str, keys: &str, enabled: bool| {
                ui.add_enabled(enabled, egui::Button::new(sc(label)).shortcut_text(keys))
                    .clicked()
            };
            ui.menu_button("File", |ui| {
                if item(ui, "New…", "Ctrl+N", true) {
                    self.request(&ctx, Pending::New);
                }
                if item(ui, "Open…", "Ctrl+O", true) {
                    self.request(&ctx, Pending::Open(None));
                }
                ui.separator();
                if item(ui, "Save", "Ctrl+S", self.io_busy.is_none()) {
                    self.save(&ctx);
                }
                if item(ui, "Save As…", "Ctrl+Shift+S", self.io_busy.is_none()) {
                    self.save_as(&ctx);
                }
                ui.separator();
                if item(ui, "Import Image as Layer…", "", true) {
                    self.import_layer(&ctx, None);
                }
                if item(ui, "Export PNG…", "Ctrl+Shift+E", self.io_busy.is_none()) {
                    self.export_png(&ctx);
                }
                ui.separator();
                if item(ui, "Quit", "", true) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                let undo = self
                    .ed
                    .history
                    .undo_label()
                    .map(|l| format!("Undo {l}"))
                    .unwrap_or("Undo".into());
                let redo = self
                    .ed
                    .history
                    .redo_label()
                    .map(|l| format!("Redo {l}"))
                    .unwrap_or("Redo".into());
                if item(ui, &undo, "Ctrl+Z", self.ed.history.can_undo()) {
                    self.ed.undo();
                }
                if item(ui, &redo, "Ctrl+Shift+Z", self.ed.history.can_redo()) {
                    self.ed.redo();
                }
                ui.separator();
                if item(ui, "Clear Layer", "Del", true) {
                    self.ed.clear_layer();
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Paper");
                    let mut on = self.ed.doc.state.paper.is_some();
                    if ui.checkbox(&mut on, "").changed() {
                        self.ed.set_paper(on.then_some([255; 3]));
                        self.ed.end_gesture();
                    }
                    if let Some(mut c) = self.ed.doc.state.paper {
                        let r = ui.color_edit_button_srgb(&mut c);
                        if r.changed() {
                            self.ed.set_paper(Some(c));
                        }
                    }
                });
            });
            ui.menu_button("Layer", |ui| {
                if item(ui, "New Layer", "Ctrl+Shift+N", true) {
                    self.ed.add_layer();
                }
                if item(ui, "Duplicate", "Ctrl+J", true) {
                    self.ed.duplicate_layer();
                }
                if item(ui, "Merge Down", "Ctrl+E", self.ed.can_merge_down()) {
                    self.ed.merge_down();
                }
                if item(ui, "Delete", "", self.ed.doc.layers().len() > 1) {
                    self.ed.delete_layer();
                }
                ui.separator();
                let locked = self.ed.doc.active_layer().alpha_lock;
                if item(
                    ui,
                    if locked { "Unlock Alpha" } else { "Lock Alpha" },
                    "/",
                    true,
                ) {
                    self.ed
                        .commit("Alpha lock", |d| d.active_layer_mut().alpha_lock = !locked);
                }
            });
            ui.menu_button("View", |ui| {
                if item(ui, "Fit to Window", "Ctrl+0", true) {
                    self.fit_pending = true;
                }
                if item(ui, "Actual Pixels", "Ctrl+1", true) {
                    self.view.zoom = 1.0;
                }
                if item(ui, "Mirror", "M", true) {
                    self.view.toggle_mirror();
                }
                if item(ui, "Reset Rotation", "", self.view.rotation != 0.0) {
                    self.view.set_rotation(0.0);
                }
                ui.separator();
                if item(ui, "Hide Panels", "Tab", true) {
                    self.ui_hidden = true;
                }
                if item(ui, "Fullscreen", "F11", true) {
                    let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
                }
            });
            ui.menu_button("Help", |ui| {
                if item(ui, "Shortcuts", "F1", true) {
                    self.modal = Some(Modal::Shortcuts);
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(b) = &self.io_busy {
                    ui.add(egui::Spinner::new().size(12.0));
                    ui.weak(b);
                }
            });
        });
    }

    pub fn tool_strip(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.vertical_centered(|ui| {
            for t in Tool::ALL {
                let sel = self.prefs.tool == t;
                if icons::button(
                    ui,
                    t.icon(),
                    32.0,
                    sel,
                    &format!("{} ({})", t.name(), t.key()),
                )
                .clicked()
                {
                    self.prefs.tool = t;
                }
                if t == Tool::Fill {
                    ui.add_space(6.0);
                }
            }
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);
            // Foreground / background colour pair.
            let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
            let bg_r = Rect::from_min_size(rect.min + vec2(13.0, 13.0), vec2(22.0, 22.0));
            let fg_r = Rect::from_min_size(rect.min, vec2(22.0, 22.0));
            let to_c = |c: [u8; 3]| Color32::from_rgb(c[0], c[1], c[2]);
            let bg_resp = ui.interact(bg_r, ui.scope_id().with("bgsw"), Sense::click());
            ui.painter().rect_filled(bg_r, 3.0, to_c(self.prefs.bg));
            ui.painter().rect_stroke(
                bg_r,
                3.0,
                Stroke::new(1.0, Color32::from_gray(90)),
                egui::StrokeKind::Inside,
            );
            ui.painter().rect_filled(fg_r, 3.0, to_c(self.prefs.fg));
            ui.painter().rect_stroke(
                fg_r,
                3.0,
                Stroke::new(1.0, Color32::from_gray(200)),
                egui::StrokeKind::Inside,
            );
            if bg_resp.on_hover_text("Swap colours (X)").clicked() {
                self.swap_colors();
            }
            ui.add_space(8.0);
            if icons::button(ui, Icon::Mirror, 28.0, self.view.mirror, "Mirror view (M)").clicked()
            {
                self.view.toggle_mirror();
            }
        });
    }

    pub fn side_panel(&mut self, ui: &mut egui::Ui) {
        let w = ui.available_width();
        section(ui, "Colour");
        let mut hsv = self.fg_hsv;
        if color_picker::picker(ui, &mut hsv, w) {
            self.fg_hsv = hsv;
            self.prefs.fg = color_picker::from_hsv(hsv);
            self.hex_edit = None;
        }
        ui.horizontal(|ui| {
            let mut hex = self
                .hex_edit
                .clone()
                .unwrap_or_else(|| egaku_core::color::Rgb::from_u8(self.prefs.fg).to_hex());
            let r = ui.add(
                egui::TextEdit::singleline(&mut hex)
                    .desired_width(72.0)
                    .font(egui::TextStyle::Monospace),
            );
            if r.changed() {
                if let Some(c) = egaku_core::color::Rgb::from_hex(&hex) {
                    self.prefs.fg = c.to_u8();
                    self.fg_hsv = c.to_hsv();
                }
                self.hex_edit = Some(hex);
            }
            if r.lost_focus() {
                self.hex_edit = None;
            }
            ui.spacing_mut().item_spacing.x = 3.0;
            let recent = self.prefs.recent.clone();
            for c in recent
                .iter()
                .take(((ui.available_width() + 3.0) / 17.0) as usize)
            {
                if color_picker::swatch(ui, *c, vec2(14.0, 14.0), *c == self.prefs.fg).clicked() {
                    self.set_fg(*c);
                }
            }
        });

        self.tool_options(ui);
        ui.add_space(4.0);
        ui.separator();
        self.layers_panel(ui);
    }

    fn tool_options(&mut self, ui: &mut egui::Ui) {
        let tool = self.prefs.tool;
        section(ui, tool.name());
        match tool {
            Tool::Brush | Tool::Eraser => {
                if tool == Tool::Brush {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for p in presets() {
                            let same = p.params == self.prefs.brush;
                            if ui.selectable_label(same, p.name).clicked() {
                                self.prefs.brush = p.params;
                            }
                        }
                    });
                    ui.add_space(2.0);
                }
                let b = self.current_brush_mut();
                ui.horizontal(|ui| {
                    ui.add_sized([62.0, 18.0], egui::Label::new("Size"));
                    ui.add(
                        egui::Slider::new(&mut b.size, 1.0..=1000.0)
                            .logarithmic(true)
                            .suffix(" px")
                            .max_decimals(1),
                    );
                });
                pct(ui, "Opacity", &mut b.opacity);
                pct(ui, "Flow", &mut b.flow);
                pct(ui, "Hardness", &mut b.hardness);
                pct(ui, "Stabilizer", &mut b.smoothing)
                    .on_hover_text("Smooths the stroke behind the pen");
                egui::CollapsingHeader::new("Dynamics")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.checkbox(&mut b.pressure_size, "Pressure → size");
                        if b.pressure_size {
                            pct(ui, "Min size", &mut b.min_size);
                        }
                        ui.checkbox(&mut b.pressure_opacity, "Pressure → opacity");
                        ui.horizontal(|ui| {
                            ui.add_sized([62.0, 18.0], egui::Label::new("Spacing"));
                            let mut p = b.spacing * 100.0;
                            if ui
                                .add(
                                    egui::Slider::new(&mut p, 1.0..=100.0)
                                        .suffix("%")
                                        .fixed_decimals(0),
                                )
                                .changed()
                            {
                                b.spacing = p / 100.0;
                            }
                        });
                    });
            }
            Tool::Fill => {
                ui.horizontal(|ui| {
                    ui.add_sized([62.0, 18.0], egui::Label::new("Tolerance"));
                    ui.add(egui::Slider::new(&mut self.prefs.fill_tolerance, 0..=255));
                });
                ui.horizontal(|ui| {
                    ui.add_sized([62.0, 18.0], egui::Label::new("Grow"));
                    ui.add(egui::Slider::new(&mut self.prefs.fill_grow, 0..=8).suffix(" px"))
                        .on_hover_text("Expands the fill under anti-aliased line art");
                });
                ui.horizontal(|ui| {
                    ui.add_sized([62.0, 18.0], egui::Label::new("Sample"));
                    ui.selectable_value(&mut self.prefs.fill_sample_all, true, "All layers");
                    ui.selectable_value(&mut self.prefs.fill_sample_all, false, "This layer");
                });
                ui.weak("Uses the brush opacity.");
            }
            Tool::Picker => {
                ui.weak("Click or drag to pick what you see. Hold Alt with any painting tool.");
            }
            Tool::Move => {
                ui.weak("Drag to move the active layer.");
            }
            Tool::Hand => {
                ui.weak("Drag to pan. Hold Space with any tool.");
            }
        }
    }

    fn layers_panel(&mut self, ui: &mut egui::Ui) {
        section(ui, "Layers");
        let ctx = ui.ctx().clone();

        // Properties of the active layer.
        let (mut blend, mut opacity, alpha_lock) = {
            let l = self.ed.doc.active_layer();
            (l.blend, l.opacity, l.alpha_lock)
        };
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("blend")
                .width(104.0)
                .selected_text(blend.name())
                .show_ui(ui, |ui| {
                    for m in BlendMode::ALL {
                        ui.selectable_value(&mut blend, m, m.name());
                    }
                });
            if blend != self.ed.doc.active_layer().blend {
                self.ed
                    .commit("Blend mode", |d| d.active_layer_mut().blend = blend);
            }
            if icons::button(ui, Icon::Lock, 22.0, alpha_lock, "Lock alpha (/)").clicked() {
                self.ed.commit("Alpha lock", |d| {
                    d.active_layer_mut().alpha_lock = !alpha_lock
                });
            }
        });
        ui.horizontal(|ui| {
            ui.add_sized([62.0, 18.0], egui::Label::new("Opacity"));
            let mut p = opacity * 100.0;
            let r = ui.add(
                egui::Slider::new(&mut p, 0.0..=100.0)
                    .suffix("%")
                    .fixed_decimals(0),
            );
            if r.changed() {
                opacity = p / 100.0;
                self.ed
                    .commit_coalesced("Layer opacity", |d| d.active_layer_mut().opacity = opacity);
            }
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.ed.end_gesture();
            }
        });

        // Action buttons along the bottom; reserve their space first.
        let footer_h = 42.0;
        let list_h = (ui.available_height() - footer_h).max(60.0);
        egui::ScrollArea::vertical()
            .max_height(list_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.layer_rows(ui, &ctx);
            });
        ui.separator();
        ui.horizontal(|ui| {
            if icons::button(ui, Icon::Plus, 24.0, false, "New layer (Ctrl+Shift+N)").clicked() {
                self.ed.add_layer();
            }
            if icons::button(ui, Icon::Duplicate, 24.0, false, "Duplicate (Ctrl+J)").clicked() {
                self.ed.duplicate_layer();
            }
            let m = icons::button(ui, Icon::MergeDown, 24.0, false, "Merge down (Ctrl+E)");
            if m.clicked() {
                self.ed.merge_down();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icons::button(ui, Icon::Trash, 24.0, false, "Delete layer").clicked() {
                    self.ed.delete_layer();
                }
                ui.weak(format!("{}", self.ed.doc.layers().len()));
            });
        });
    }

    fn layer_thumb(
        &mut self,
        ctx: &egui::Context,
        i: usize,
        budget: &mut usize,
    ) -> Option<egui::TextureId> {
        let l = &self.ed.doc.layers()[i];
        let key = std::sync::Arc::as_ptr(&l.tiles) as usize;
        match self.thumbs.get(&l.id) {
            Some((k, t)) if *k == key || *budget == 0 => return Some(t.id()),
            None if *budget == 0 => return None,
            _ => {}
        }
        *budget -= 1;
        let (tw, th, px) = thumbnail(&l.tiles, self.ed.doc.width, self.ed.doc.height, 64);
        let img = egui::ColorImage::from_rgba_premultiplied([tw as usize, th as usize], &px);
        let tex = ctx.load_texture(format!("thumb-{}", l.id), img, egui::TextureOptions::LINEAR);
        let id = tex.id();
        self.thumbs.insert(l.id, (key, tex));
        Some(id)
    }

    fn layer_rows(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let n = self.ed.doc.layers().len();
        let row_h = 40.0;
        let mut budget = 6; // thumbnails regenerated per frame
        let mut select = None;
        let mut toggle_vis = None;
        let mut drop_at: Option<usize> = None;
        let pointer = ctx.input(|i| i.pointer.interact_pos());
        let w = ui.available_width();
        // Drop the cache for deleted layers.
        let ids: Vec<_> = self.ed.doc.layers().iter().map(|l| l.id).collect();
        self.thumbs.retain(|id, _| ids.contains(id));

        for i in (0..n).rev() {
            let (rect, resp) = ui.allocate_exact_size(vec2(w, row_h), Sense::click_and_drag());
            let l = &self.ed.doc.layers()[i];
            let (id, name, visible, blend, opacity, lock) = (
                l.id,
                l.name.clone(),
                l.visible,
                l.blend,
                l.opacity,
                l.alpha_lock,
            );
            let active = i == self.ed.doc.active();
            let p = ui.painter();
            let bg = if active {
                theme::ACCENT_DIM
            } else if resp.hovered() {
                Color32::from_gray(44)
            } else {
                Color32::TRANSPARENT
            };
            p.rect_filled(rect.shrink2(vec2(0.0, 1.0)), 4.0, bg);

            // Visibility toggle.
            let eye =
                Rect::from_min_size(rect.min + vec2(4.0, (row_h - 22.0) / 2.0), vec2(22.0, 22.0));
            let eye_r = ui.interact(eye, ui.scope_id().with(("eye", id)), Sense::click());
            icons::paint(
                ui.painter(),
                eye.shrink(3.0),
                if visible { Icon::Eye } else { Icon::EyeOff },
                if eye_r.hovered() {
                    Color32::WHITE
                } else if visible {
                    theme::TEXT
                } else {
                    theme::TEXT_DIM
                },
            );
            if eye_r.clicked() {
                toggle_vis = Some(i);
            }

            // Thumbnail over a checkerboard.
            let th = Rect::from_min_size(rect.min + vec2(30.0, 4.0), vec2(44.0, row_h - 8.0));
            let p = ui.painter();
            p.rect_filled(th, 2.0, Color32::from_gray(180));
            let cell = 6.0;
            for cy in 0..((th.height() / cell).ceil() as i32) {
                for cx in 0..((th.width() / cell).ceil() as i32) {
                    if (cx + cy) % 2 == 0 {
                        let r = Rect::from_min_size(
                            th.min + vec2(cx as f32 * cell, cy as f32 * cell),
                            vec2(cell, cell),
                        )
                        .intersect(th);
                        p.rect_filled(r, 0.0, Color32::from_gray(214));
                    }
                }
            }
            if let Some(tex) = self.layer_thumb(ctx, i, &mut budget) {
                let (dw, dh) = (self.ed.doc.width as f32, self.ed.doc.height as f32);
                let s = (th.width() / dw).min(th.height() / dh);
                let r = Rect::from_center_size(th.center(), vec2(dw * s, dh * s));
                ui.painter().image(
                    tex,
                    r,
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            ui.painter().rect_stroke(
                th,
                2.0,
                Stroke::new(1.0, Color32::from_gray(20)),
                egui::StrokeKind::Outside,
            );

            // Name, or an inline editor when renaming.
            let text_x = th.right() + 8.0;
            let name_rect = Rect::from_min_max(
                egui::pos2(text_x, rect.top() + 3.0),
                egui::pos2(rect.right() - 4.0, rect.top() + 22.0),
            );
            let renaming = self.renaming.as_ref().is_some_and(|(rid, _)| *rid == id);
            if renaming {
                let mut text = self.renaming.as_ref().unwrap().1.clone();
                let r = ui.put(name_rect, egui::TextEdit::singleline(&mut text));
                r.request_focus();
                self.renaming = Some((id, text.clone()));
                if r.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let text = text.trim().to_string();
                    if !text.is_empty() && text != name {
                        self.ed
                            .commit("Rename layer", |d| d.state.layers[i].name = text);
                    }
                    self.renaming = None;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.renaming = None;
                }
            } else {
                let col = if visible {
                    theme::TEXT
                } else {
                    theme::TEXT_DIM
                };
                ui.painter().text(
                    name_rect.left_center(),
                    egui::Align2::LEFT_CENTER,
                    &name,
                    egui::FontId::proportional(13.0),
                    col,
                );
            }
            let mut sub = String::new();
            if blend != BlendMode::Normal {
                sub += blend.name();
            }
            if opacity < 0.999 {
                if !sub.is_empty() {
                    sub += " · ";
                }
                sub += &format!("{:.0}%", opacity * 100.0);
            }
            ui.painter().text(
                egui::pos2(text_x, rect.bottom() - 10.0),
                egui::Align2::LEFT_CENTER,
                sub,
                egui::FontId::proportional(11.0),
                theme::TEXT_DIM,
            );
            if lock {
                let lr = Rect::from_min_size(
                    egui::pos2(rect.right() - 20.0, rect.bottom() - 18.0),
                    vec2(14.0, 14.0),
                );
                icons::paint(ui.painter(), lr, Icon::Lock, theme::TEXT_DIM);
            }

            if resp.double_clicked() && !eye_r.hovered() {
                self.renaming = Some((id, name.clone()));
            } else if resp.clicked() && !eye_r.hovered() {
                select = Some(i);
            }
            if resp.drag_started() {
                self.layer_drag = Some(i);
            }
            // Drop target indicator.
            if let (Some(from), Some(pp)) = (self.layer_drag, pointer)
                && from != i
                && rect.contains(pp)
            {
                let above = pp.y < rect.center().y;
                let y = if above { rect.top() } else { rect.bottom() };
                ui.painter()
                    .hline(rect.x_range(), y, Stroke::new(2.0, theme::ACCENT));
                // Rows are drawn top-down from the top layer.
                let target = if above { i + 1 } else { i };
                let target = if target > from { target - 1 } else { target };
                drop_at = Some(target.min(n - 1));
            }
        }

        if let Some(i) = toggle_vis {
            let v = !self.ed.doc.layers()[i].visible;
            self.ed
                .commit(if v { "Show layer" } else { "Hide layer" }, |d| {
                    d.state.layers[i].visible = v
                });
        }
        if let Some(i) = select {
            self.ed.select_layer(i);
        }
        if self.layer_drag.is_some() && !ctx.input(|i| i.pointer.any_down()) {
            if let (Some(from), Some(to)) = (self.layer_drag, drop_at) {
                self.ed.move_layer(from, to);
            }
            self.layer_drag = None;
        }
    }

    pub fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let hint = match self.prefs.tool {
                Tool::Brush | Tool::Eraser => {
                    "Shift-click: straight line · Alt: pick colour · [ ]: size"
                }
                Tool::Fill => "Click to fill · Alt: pick colour",
                Tool::Picker => "Click or drag to pick",
                Tool::Move => "Drag to move the layer",
                Tool::Hand => "Drag to pan",
            };
            ui.weak(hint);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mb = |b: usize| b as f64 / 1048576.0;
                ui.weak(format!(
                    "pixels {:.0} MB · undo {:.0} MB",
                    mb(self.ed.doc.pixel_bytes()),
                    mb(self.ed.history.memory())
                ))
                .on_hover_text(format!(
                    "{} GPU tiles cached",
                    self.textures.texture_count()
                ));
                ui.separator();
                if self.view.mirror
                    && ui
                        .small_button("mirrored")
                        .on_hover_text("Click to unmirror (M)")
                        .clicked()
                {
                    self.view.toggle_mirror();
                }
                let deg = self.view.rotation.to_degrees();
                if deg.abs() > 0.05
                    && ui
                        .small_button(format!("{deg:.0}°"))
                        .on_hover_text("Click to reset rotation")
                        .clicked()
                {
                    self.view.set_rotation(0.0);
                }
                let z = egui::RichText::new(format!("{:.0}%", self.view.zoom * 100.0)).monospace();
                ui.menu_button(z, |ui| {
                    if ui.button("Fit").clicked() {
                        self.fit_pending = true;
                    }
                    for z in [0.25, 0.5, 1.0, 2.0, 4.0] {
                        if ui.button(format!("{:.0}%", z * 100.0)).clicked() {
                            self.view.zoom = z;
                        }
                    }
                });
                ui.weak(format!("{} × {}", self.ed.doc.width, self.ed.doc.height));
            });
        });
    }
}
