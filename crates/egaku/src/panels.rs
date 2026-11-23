//! Menus, the tool options bar, tool strip, side dock and timeline.

use eframe::egui::{self, Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use egaku_core::adjust::Adjustment;
use egaku_core::blend::BlendMode;
use egaku_core::document::{Content, Kind, LayerId, Place};
use egaku_core::selection::SelectOp;
use egaku_core::thumb::thumbnail;
use egui_phosphor::regular as ph;

use crate::app::{AnimFormat, App, Modal, NumberOp, Pending};
use crate::color_picker;
use crate::prefs::{GradientKind, SelectShape, Tool, presets};
use crate::theme;
use crate::widgets::{self, Scale, field, icon_button, pct, segmented, toggle};

fn menu_item(ui: &mut egui::Ui, label: &str, keys: &str, enabled: bool) -> bool {
    ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(keys))
        .clicked()
}

impl App {
    pub fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(ph::PAINT_BRUSH_BROAD)
                    .font(theme::icon_font(16.0))
                    .color(theme::ACCENT),
            );
            ui.add_space(6.0);
            let busy = self.io_busy.is_some();
            ui.menu_button("File", |ui| {
                if menu_item(ui, "New…", "Ctrl+N", true) {
                    self.request(&ctx, Pending::New);
                }
                if menu_item(ui, "Open…", "Ctrl+O", true) {
                    self.request(&ctx, Pending::Open(None));
                }
                let recent = self.prefs.recent_files.clone();
                ui.add_enabled_ui(!recent.is_empty(), |ui| {
                    ui.menu_button("Open Recent", |ui| {
                        for p in recent {
                            if ui
                                .button(crate::app::file_name(&p))
                                .on_hover_text(p.display().to_string())
                                .clicked()
                            {
                                self.request(&ctx, Pending::Open(Some(p)));
                            }
                        }
                    });
                });
                ui.separator();
                if menu_item(ui, "Save", "Ctrl+S", !busy) {
                    self.save(&ctx);
                }
                if menu_item(ui, "Save As…", "Ctrl+Shift+S", !busy) {
                    self.save_as(&ctx);
                }
                ui.separator();
                if menu_item(ui, "Place Image…", "", true) {
                    self.import_image(&ctx, None, None);
                }
                if menu_item(ui, "Export…", "Ctrl+Shift+E", !busy) {
                    self.modal = Some(Modal::Export);
                }
                if menu_item(
                    ui,
                    "Export Animation…",
                    "",
                    !busy && self.ed.doc.state.frames > 1,
                ) {
                    self.modal = Some(Modal::ExportAnim(AnimFormat::Gif));
                }
                ui.separator();
                if menu_item(ui, "Quit", "", true) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| self.edit_menu(ui, &ctx));
            ui.menu_button("Image", |ui| self.image_menu(ui));
            ui.menu_button("Layer", |ui| self.layer_menu(ui));
            ui.menu_button("Select", |ui| self.select_menu(ui));
            ui.menu_button("Adjust", |ui| {
                for (i, a) in Adjustment::defaults().into_iter().enumerate() {
                    if i == 4 || i == 8 {
                        ui.separator();
                    }
                    let label = if a.has_params() {
                        format!("{}…", a.name())
                    } else {
                        a.name().to_string()
                    };
                    if menu_item(ui, &label, "", true) {
                        self.start_adjust(&ctx, a);
                    }
                }
            });
            ui.menu_button("View", |ui| {
                if menu_item(ui, "Fit to Window", "Ctrl+0", true) {
                    self.fit_pending = true;
                }
                if menu_item(ui, "Actual Pixels", "Ctrl+1", true) {
                    self.view.zoom = 1.0;
                }
                if menu_item(ui, "Mirror", "Shift+M", true) {
                    self.view.toggle_mirror();
                }
                if menu_item(ui, "Reset Rotation", "", self.view.rotation != 0.0) {
                    self.view.set_rotation(0.0);
                }
                ui.separator();
                ui.checkbox(&mut self.prefs.pixel_grid, "Pixel Grid  (at 800%+)");
                ui.horizontal(|ui| {
                    ui.label("Grid every");
                    ui.add(
                        egui::DragValue::new(&mut self.prefs.grid_size)
                            .range(0..=1024)
                            .suffix(" px"),
                    );
                });
                ui.checkbox(&mut self.prefs.show_timeline, "Timeline");
                ui.separator();
                if menu_item(ui, "Hide Panels", "Tab", true) {
                    self.ui_hidden = true;
                }
                if menu_item(ui, "Fullscreen", "F11", true) {
                    let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
                }
            });
            ui.menu_button("Help", |ui| {
                if menu_item(ui, "Keyboard Shortcuts", "F1", true) {
                    self.modal = Some(Modal::Shortcuts);
                }
                if menu_item(ui, "About egaku", "", true) {
                    self.modal = Some(Modal::About);
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                if let Some(b) = &self.io_busy {
                    ui.add(egui::Spinner::new().size(12.0));
                    ui.weak(b);
                }
            });
        });
    }

    fn edit_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
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
        if menu_item(ui, &undo, "Ctrl+Z", self.ed.history.can_undo()) {
            self.cancel_modes();
            self.ed.undo();
        }
        if menu_item(ui, &redo, "Ctrl+Shift+Z", self.ed.history.can_redo()) {
            self.cancel_modes();
            self.ed.redo();
        }
        ui.separator();
        if menu_item(ui, "Cut", "Ctrl+X", true) {
            self.copy(ctx, false, true);
        }
        if menu_item(ui, "Copy", "Ctrl+C", true) {
            self.copy(ctx, false, false);
        }
        if menu_item(ui, "Copy Merged", "Ctrl+Shift+C", true) {
            self.copy(ctx, true, false);
        }
        if menu_item(ui, "Paste", "Ctrl+V", self.clipboard.is_some()) {
            self.paste_internal();
        }
        ui.separator();
        if menu_item(ui, "Clear", "Del", true)
            && let Err(r) = self.ed.clear_selected()
        {
            self.refusal(ctx, r);
        }
        if menu_item(ui, "Fill with Colour", "Alt+Del", true) {
            let c = self.prefs.fg;
            if let Err(r) = self.ed.fill_selection(c) {
                self.refusal(ctx, r);
            }
        }
        ui.separator();
        if menu_item(ui, "Free Transform", "Ctrl+T", true) {
            self.start_free_transform(ctx);
        }
        let q = |s: &mut Self,
                 label: &str,
                 f: &dyn Fn(f32, f32, f32, f32) -> egaku_core::transform::Affine| {
            if let Err(r) = s.ed.quick_transform(label, f) {
                s.refusal(ctx, r);
            }
        };
        use egaku_core::transform::Affine;
        if menu_item(ui, "Flip Horizontal", "", true) {
            q(self, "Flip", &|cx, cy, _, _| {
                Affine::translate(cx, cy)
                    .compose(Affine::scale(-1.0, 1.0))
                    .compose(Affine::translate(-cx, -cy))
            });
        }
        if menu_item(ui, "Flip Vertical", "", true) {
            q(self, "Flip", &|cx, cy, _, _| {
                Affine::translate(cx, cy)
                    .compose(Affine::scale(1.0, -1.0))
                    .compose(Affine::translate(-cx, -cy))
            });
        }
        if menu_item(ui, "Rotate 90° Clockwise", "", true) {
            q(self, "Rotate", &|cx, cy, w, h| rot90(cx, cy, w, h, 1.0));
        }
        if menu_item(ui, "Rotate 90° Counter-clockwise", "", true) {
            q(self, "Rotate", &|cx, cy, w, h| rot90(cx, cy, w, h, -1.0));
        }
    }

    fn image_menu(&mut self, ui: &mut egui::Ui) {
        let (w, h) = (self.ed.doc.width, self.ed.doc.height);
        if menu_item(ui, "Image Size…", "", true) {
            self.modal = Some(Modal::ImageSize {
                w,
                h,
                lock: true,
                smooth: !self.prefs.brush.pixel,
            });
        }
        if menu_item(ui, "Canvas Size…", "", true) {
            self.modal = Some(Modal::CanvasSize {
                w,
                h,
                anchor: (0, 0),
            });
        }
        if menu_item(ui, "Crop to Selection", "", self.ed.selection().is_some()) {
            self.finish_modes();
            self.ed.crop_to_selection();
        }
        ui.separator();
        if menu_item(ui, "Rotate 90° Clockwise", "", true) {
            self.finish_modes();
            self.ed.rotate_canvas(1);
        }
        if menu_item(ui, "Rotate 90° Counter-clockwise", "", true) {
            self.finish_modes();
            self.ed.rotate_canvas(3);
        }
        if menu_item(ui, "Rotate 180°", "", true) {
            self.finish_modes();
            self.ed.rotate_canvas(2);
        }
        if menu_item(ui, "Flip Canvas Horizontally", "", true) {
            self.finish_modes();
            self.ed.flip_canvas(true);
        }
        if menu_item(ui, "Flip Canvas Vertically", "", true) {
            self.finish_modes();
            self.ed.flip_canvas(false);
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
                if ui.color_edit_button_srgb(&mut c).changed() {
                    self.ed.set_paper(Some(c));
                }
            } else {
                ui.weak("transparent");
            }
        });
        ui.separator();
        if menu_item(ui, "Flatten Image", "", self.ed.doc.layer_count() > 1) {
            self.finish_modes();
            self.ed.flatten();
        }
    }

    fn layer_menu(&mut self, ui: &mut egui::Ui) {
        let l = self.ed.doc.active_layer();
        let (id, kind, lock, clip) = (l.id, l.kind(), l.alpha_lock, l.clip);
        if menu_item(ui, "New Layer", "Ctrl+Shift+N", true) {
            self.ed.add_layer();
        }
        if menu_item(ui, "New Group", "", true) {
            self.ed.add_group();
        }
        if menu_item(ui, "Duplicate", "Ctrl+J", true) {
            self.ed.duplicate_layer(id);
        }
        if menu_item(
            ui,
            "Layer via Copy",
            "Ctrl+J",
            self.ed.selection().is_some(),
        ) {
            self.ed.layer_via_copy();
        }
        if menu_item(ui, "Delete", "", self.ed.doc.layer_count() > 1) {
            self.ed.delete_layer(id);
        }
        ui.separator();
        if menu_item(ui, "Group", "Ctrl+G", true) {
            self.ed.group_layer(id);
        }
        if menu_item(ui, "Ungroup", "Ctrl+Shift+G", kind == Kind::Group) {
            self.ed.ungroup(id);
        }
        if menu_item(ui, "Merge Down", "Ctrl+E", self.ed.can_merge_down(id)) {
            self.ed.merge_down(id);
        }
        if menu_item(
            ui,
            if clip {
                "Release Clipping"
            } else {
                "Clip to Layer Below"
            },
            "Ctrl+Alt+G",
            true,
        ) {
            self.ed.toggle_clip(id);
        }
        if menu_item(
            ui,
            if lock { "Unlock Alpha" } else { "Lock Alpha" },
            "/",
            kind == Kind::Raster,
        ) {
            self.ed
                .set_layer(id, "Alpha lock", |l| l.alpha_lock = !lock);
        }
        if menu_item(ui, "Rasterize", "", kind != Kind::Raster) {
            self.finish_modes();
            self.ed.rasterize(id);
        }
        ui.separator();
        if menu_item(ui, "Select Layer Pixels", "", true) {
            self.ed.select_layer_pixels(id, SelectOp::Replace);
        }
    }

    fn select_menu(&mut self, ui: &mut egui::Ui) {
        let has = self.ed.selection().is_some();
        if menu_item(ui, "All", "Ctrl+A", true) {
            self.ed.select_all();
        }
        if menu_item(ui, "Deselect", "Ctrl+D", has) {
            self.ed.deselect();
        }
        if menu_item(ui, "Invert", "Ctrl+Shift+I", true) {
            self.ed.invert_selection();
        }
        ui.separator();
        for (label, op, v) in [
            ("Feather…", NumberOp::Feather, 8.0),
            ("Grow…", NumberOp::Grow, 4.0),
            ("Shrink…", NumberOp::Shrink, 4.0),
        ] {
            if menu_item(ui, label, "", has) {
                self.modal = Some(Modal::Number { what: op, value: v });
            }
        }
        ui.separator();
        if menu_item(ui, "Layer Pixels", "", true) {
            let id = self.ed.doc.active_id();
            self.ed.select_layer_pixels(id, SelectOp::Replace);
        }
    }

    // ---- tool options bar -----------------------------------------------------

    pub fn context_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let tool = self.prefs.tool;
            let (icon, name) = if self.free_transform {
                (ph::BOUNDING_BOX, "Transform")
            } else {
                (tool.icon(), tool.name())
            };
            ui.label(
                egui::RichText::new(icon)
                    .font(theme::icon_font(17.0))
                    .color(theme::ACCENT),
            );
            ui.label(egui::RichText::new(name).font(theme::semibold(13.0)));
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            if self.free_transform {
                self.transform_options(ui);
                return;
            }
            match tool {
                Tool::Brush | Tool::Eraser => self.brush_options(ui, &ctx),
                Tool::Fill => {
                    let mut t = self.prefs.fill_tolerance as f32;
                    if field(
                        ui,
                        "Tolerance",
                        &mut t,
                        0.0,
                        255.0,
                        Scale::Linear,
                        &|v| format!("{v:.0}"),
                        130.0,
                    )
                    .changed()
                    {
                        self.prefs.fill_tolerance = t.round() as u8;
                    }
                    let mut g = self.prefs.fill_grow as f32;
                    if field(
                        ui,
                        "Grow",
                        &mut g,
                        0.0,
                        8.0,
                        Scale::Linear,
                        &|v| format!("{v:.0} px"),
                        100.0,
                    )
                    .on_hover_text("Expands the fill under anti-aliased line art")
                    .changed()
                    {
                        self.prefs.fill_grow = g.round() as u32;
                    }
                    pct(ui, "Opacity", &mut self.prefs.brush.opacity, 120.0);
                    segmented(
                        ui,
                        &mut self.prefs.fill_sample_all,
                        &[
                            (true, ph::STACK, "Fill by what you see (all layers)"),
                            (false, ph::SQUARE, "Fill by this layer only"),
                        ],
                    );
                }
                Tool::Gradient => {
                    segmented(
                        ui,
                        &mut self.prefs.gradient,
                        &[
                            (GradientKind::Linear, ph::GRADIENT, "Linear"),
                            (GradientKind::Radial, ph::CIRCLE_HALF, "Radial"),
                        ],
                    );
                    toggle(
                        ui,
                        &mut self.prefs.gradient_transparent,
                        ph::DROP_HALF,
                        "To transparent",
                        "Fade the main colour out instead of blending to the second colour",
                    );
                    pct(ui, "Opacity", &mut self.prefs.brush.opacity, 120.0);
                }
                Tool::Select => self.select_options(ui),
                Tool::Move => self.move_options(ui, &ctx),
                Tool::Text => self.text_options(ui),
                Tool::Crop => {
                    if let Some(r) = self.crop {
                        ui.label(format!("{:.0} × {:.0} px", r[2] - r[0], r[3] - r[1]));
                        if ui
                            .button(widgets::il(ph::CHECK, "Crop"))
                            .on_hover_text("Enter")
                            .clicked()
                        {
                            self.apply_crop();
                        }
                        if ui.button("Cancel").on_hover_text("Esc").clicked() {
                            self.crop = None;
                        }
                    } else {
                        widgets::caption(ui, "Drag on the canvas to choose what to keep.");
                    }
                    if self.ed.selection().is_some() && ui.button("Crop to Selection").clicked() {
                        self.ed.crop_to_selection();
                    }
                }
                Tool::Picker | Tool::Hand => widgets::caption(ui, tool.blurb()),
            }
        });
    }

    fn brush_options(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let tool = self.prefs.tool;
        if tool == Tool::Brush {
            let current = presets()
                .into_iter()
                .find(|p| p.params == self.prefs.brush)
                .map_or("Custom", |p| p.name);
            let r = ui.add(
                egui::Button::new(widgets::li(current, ph::CARET_DOWN))
                    .min_size(vec2(96.0, theme::ROW)),
            );
            egui::Popup::menu(&r).show(|ui| {
                ui.set_width(220.0);
                for p in presets() {
                    let tex = self.preset_preview(ctx, p.name, &p.params);
                    let sel = p.params == self.prefs.brush;
                    let (rect, resp) = ui.allocate_exact_size(vec2(216.0, 40.0), Sense::click());
                    if sel || resp.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            6.0,
                            if sel {
                                theme::BG_ACTIVE
                            } else {
                                theme::BG_HOVER
                            },
                        );
                    }
                    ui.painter().text(
                        rect.left_center() + vec2(10.0, 0.0),
                        Align2::LEFT_CENTER,
                        p.name,
                        egui::FontId::proportional(13.0),
                        theme::TEXT,
                    );
                    let img = Rect::from_min_size(rect.min + vec2(80.0, 4.0), vec2(128.0, 32.0));
                    ui.painter().image(
                        tex,
                        img,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    if resp.clicked() {
                        self.prefs.brush = p.params;
                        ui.close();
                    }
                }
            });
        }
        let b = self.current_brush_mut();
        if b.pixel {
            field(
                ui,
                "Size",
                &mut b.size,
                1.0,
                64.0,
                Scale::Linear,
                &|v| format!("{:.0} px", v.round()),
                120.0,
            );
            b.size = b.size.round().max(1.0);
        } else {
            field(
                ui,
                "Size",
                &mut b.size,
                1.0,
                1000.0,
                Scale::Log,
                &|v| {
                    if v < 10.0 {
                        format!("{v:.1} px")
                    } else {
                        format!("{v:.0} px")
                    }
                },
                130.0,
            );
        }
        pct(ui, "Opacity", &mut b.opacity, 120.0);
        if !b.pixel {
            pct(ui, "Flow", &mut b.flow, 100.0);
            pct(ui, "Hardness", &mut b.hardness, 120.0);
        }
        pct(ui, "Stabilizer", &mut b.smoothing, 124.0)
            .on_hover_text("Smooths the stroke behind the pen");
        toggle(
            ui,
            &mut b.pixel,
            ph::GRID_FOUR,
            "Pixel",
            "Pixel-art mode: hard, aliased pixels with no gaps",
        );
        let r = icon_button(
            ui,
            ph::SLIDERS_HORIZONTAL,
            theme::ROW,
            false,
            "Pen pressure and more",
        );
        egui::Popup::menu(&r)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_width(230.0);
                ui.label(egui::RichText::new("Pen pressure").font(theme::semibold(12.5)));
                ui.checkbox(&mut b.pressure_size, "Controls size");
                if b.pressure_size {
                    pct(ui, "Min size", &mut b.min_size, 220.0);
                }
                ui.checkbox(&mut b.pressure_opacity, "Controls opacity");
                ui.separator();
                let mut s = b.spacing;
                if field(
                    ui,
                    "Spacing",
                    &mut s,
                    0.01,
                    1.0,
                    Scale::Linear,
                    &|v| format!("{:.0}%", v * 100.0),
                    220.0,
                )
                .changed()
                {
                    b.spacing = s;
                }
                if b.pixel {
                    ui.checkbox(&mut b.pixel_perfect, "Pixel-perfect lines")
                        .on_hover_text("Removes doubled pixels at the corners of 1px lines");
                }
            });
    }

    fn select_options(&mut self, ui: &mut egui::Ui) {
        let opts: Vec<(SelectShape, &str, &str)> = SelectShape::ALL
            .iter()
            .map(|s| (*s, s.icon(), s.name()))
            .collect();
        segmented(ui, &mut self.prefs.select_shape, &opts);
        ui.add_space(4.0);
        segmented(
            ui,
            &mut self.select_mode,
            &[
                (SelectOp::Replace, ph::SQUARE, "New selection"),
                (SelectOp::Add, ph::UNITE_SQUARE, "Add (Shift)"),
                (SelectOp::Subtract, ph::SUBTRACT_SQUARE, "Subtract (Alt)"),
                (
                    SelectOp::Intersect,
                    ph::INTERSECT_SQUARE,
                    "Intersect (Shift+Alt)",
                ),
            ],
        );
        if self.prefs.select_shape == SelectShape::Wand {
            let mut t = self.prefs.wand_tolerance as f32;
            if field(
                ui,
                "Tolerance",
                &mut t,
                0.0,
                255.0,
                Scale::Linear,
                &|v| format!("{v:.0}"),
                130.0,
            )
            .changed()
            {
                self.prefs.wand_tolerance = t.round() as u8;
            }
            toggle(
                ui,
                &mut self.prefs.wand_sample_all,
                ph::STACK,
                "All layers",
                "Select by what you see rather than the active layer",
            );
        } else if self.prefs.select_shape != SelectShape::Rect {
            toggle(
                ui,
                &mut self.prefs.select_antialias,
                ph::CIRCLE_HALF,
                "Smooth edges",
                "Antialias the edge (turn off for pixel art)",
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(6.0);
            let has = self.ed.selection().is_some();
            if ui.add_enabled(has, egui::Button::new("Deselect")).clicked() {
                self.ed.deselect();
            }
            if ui.button("Invert").clicked() {
                self.ed.invert_selection();
            }
            if ui.button("All").clicked() {
                self.ed.select_all();
            }
        });
    }

    fn move_options(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        toggle(
            ui,
            &mut self.prefs.auto_select,
            ph::CURSOR_CLICK,
            "Pick layer",
            "Clicking chooses the layer under the pointer",
        );
        let l = self.ed.doc.active_layer();
        let (id, kind) = (l.id, l.kind());
        if let Content::Image(o) = &l.content {
            let mut smooth = o.smooth;
            if segmented(
                ui,
                &mut smooth,
                &[
                    (true, "Smooth", "Smooth scaling (photos)"),
                    (false, "Pixel", "Nearest-neighbour scaling (pixel art)"),
                ],
            ) {
                self.ed.set_image_smooth(id, smooth);
            }
        } else {
            segmented(
                ui,
                &mut self.prefs.smooth_transform,
                &[
                    (true, "Smooth", "Smooth scaling and rotation"),
                    (false, "Pixel", "Nearest-neighbour: keeps pixel art crisp"),
                ],
            );
        }
        ui.separator();
        use egaku_core::transform::Affine;
        let quick = |s: &mut Self, f: &dyn Fn(f32, f32, f32, f32) -> Affine| {
            if let Err(r) = s.ed.quick_transform("Transform", f) {
                s.refusal(ctx, r);
            }
        };
        if icon_button(
            ui,
            ph::FLIP_HORIZONTAL,
            theme::ROW,
            false,
            "Flip horizontally",
        )
        .clicked()
        {
            quick(self, &|cx, cy, _, _| {
                Affine::translate(cx, cy)
                    .compose(Affine::scale(-1.0, 1.0))
                    .compose(Affine::translate(-cx, -cy))
            });
        }
        if icon_button(ui, ph::FLIP_VERTICAL, theme::ROW, false, "Flip vertically").clicked() {
            quick(self, &|cx, cy, _, _| {
                Affine::translate(cx, cy)
                    .compose(Affine::scale(1.0, -1.0))
                    .compose(Affine::translate(-cx, -cy))
            });
        }
        if icon_button(
            ui,
            ph::ARROW_COUNTER_CLOCKWISE,
            theme::ROW,
            false,
            "Rotate 90° counter-clockwise",
        )
        .clicked()
        {
            quick(self, &|cx, cy, w, h| rot90(cx, cy, w, h, -1.0));
        }
        if icon_button(
            ui,
            ph::ARROW_CLOCKWISE,
            theme::ROW,
            false,
            "Rotate 90° clockwise",
        )
        .clicked()
        {
            quick(self, &|cx, cy, w, h| rot90(cx, cy, w, h, 1.0));
        }
        if kind != Kind::Text
            && kind != Kind::Image
            && ui
                .button(widgets::il(ph::BOUNDING_BOX, "Transform"))
                .on_hover_text("Scale and rotate with handles (Ctrl+T)")
                .clicked()
        {
            self.start_free_transform(ctx);
        }
        if matches!(kind, Kind::Text | Kind::Image) {
            widgets::caption(
                ui,
                "Drag the frame's handles to scale; just outside a corner to rotate.",
            );
        }
    }

    fn transform_options(&mut self, ui: &mut egui::Ui) {
        if let Some(t) = self.ed.transforming() {
            let f = t.delta.compose(t.frame);
            let (sx, sy) = f.scales();
            let (fx, fy) = t.frame.scales();
            ui.label(format!(
                "W {:.0}%   H {:.0}%   ∠ {:.1}°",
                sx / fx.max(1e-6) * 100.0,
                sy / fy.max(1e-6) * 100.0,
                f.angle().to_degrees()
            ));
        }
        let mut smooth = self.prefs.smooth_transform;
        if segmented(
            ui,
            &mut smooth,
            &[
                (true, "Smooth", "Smooth resampling"),
                (false, "Pixel", "Nearest neighbour (pixel art)"),
            ],
        ) {
            self.prefs.smooth_transform = smooth;
            if let Some(d) = self.ed.transforming().map(|t| t.delta) {
                self.ed.set_transform(d, smooth);
            }
        }
        widgets::caption(ui, "Shift: free / snap angle · Alt: from centre");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(6.0);
            if ui
                .add(egui::Button::new(widgets::il(ph::CHECK, "Apply")).fill(theme::ACCENT_BG))
                .on_hover_text("Enter")
                .clicked()
            {
                self.ed.commit_transform();
                self.free_transform = false;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                self.cancel_modes();
            }
        });
    }

    fn text_options(&mut self, ui: &mut egui::Ui) {
        // Edits apply to the text being typed (or the active text layer),
        // and become the defaults for new text.
        let mut obj = match &self.text_edit {
            Some(te) => Some(te.obj.clone()),
            None => match &self.ed.doc.active_layer().content {
                Content::Text(o) => Some((**o).clone()),
                _ => None,
            },
        };
        let d = &mut self.prefs.text;
        let (mut font, mut size, mut bold, mut italic, mut align, mut lh) = match &obj {
            Some(o) => (
                o.font.clone(),
                o.size,
                o.bold,
                o.italic,
                o.align as u8,
                o.line_height,
            ),
            None => (
                d.font.clone(),
                d.size,
                d.bold,
                d.italic,
                d.align_i,
                d.line_height,
            ),
        };
        let before = (font.clone(), size, bold, italic, align, lh);
        // Font family, searchable.
        let label = if font.is_empty() {
            "Sans-serif".to_string()
        } else {
            font.clone()
        };
        let r = ui.add(
            egui::Button::new(widgets::li(&label, ph::CARET_DOWN))
                .min_size(vec2(170.0, theme::ROW)),
        );
        egui::Popup::menu(&r)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_width(240.0);
                if self.fonts.is_none() {
                    self.fonts = Some(egaku_core::object::font_families());
                }
                ui.add(egui::TextEdit::singleline(&mut self.font_filter).hint_text("Search fonts"));
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        let filter = self.font_filter.to_lowercase();
                        if filter.is_empty()
                            && ui
                                .selectable_label(font.is_empty(), "Sans-serif (default)")
                                .clicked()
                        {
                            font.clear();
                            ui.close();
                        }
                        for f in self.fonts.as_ref().unwrap() {
                            if !filter.is_empty() && !f.to_lowercase().contains(&filter) {
                                continue;
                            }
                            if ui.selectable_label(*f == font, f).clicked() {
                                font = f.clone();
                                ui.close();
                            }
                        }
                    });
            });
        field(
            ui,
            "Size",
            &mut size,
            4.0,
            1000.0,
            Scale::Log,
            &|v| format!("{v:.0} px"),
            120.0,
        );
        toggle(ui, &mut bold, ph::TEXT_B, "", "Bold");
        toggle(ui, &mut italic, ph::TEXT_ITALIC, "", "Italic");
        segmented(
            ui,
            &mut align,
            &[
                (0, ph::TEXT_ALIGN_LEFT, "Align left"),
                (1, ph::TEXT_ALIGN_CENTER, "Centre"),
                (2, ph::TEXT_ALIGN_RIGHT, "Align right"),
            ],
        );
        field(
            ui,
            "Line",
            &mut lh,
            0.6,
            3.0,
            Scale::Linear,
            &|v| format!("{v:.2}"),
            100.0,
        )
        .on_hover_text("Line height");
        let d = &mut self.prefs.text;
        (d.font, d.size, d.bold, d.italic, d.align_i, d.line_height) =
            (font.clone(), size, bold, italic, align, lh);
        if (font.clone(), size, bold, italic, align, lh) != before
            && let Some(o) = &mut obj
        {
            o.font = font;
            o.size = size;
            o.bold = bold;
            o.italic = italic;
            o.align = self.prefs.text_align();
            o.line_height = lh;
            match &mut self.text_edit {
                Some(te) => {
                    te.obj = o.clone();
                    if let Some(id) = te.layer {
                        self.ed.set_text(id, o.clone());
                    }
                }
                None => {
                    let id = self.ed.doc.active_id();
                    self.ed.set_text(id, o.clone());
                }
            }
        }
        if ui.input(|i| i.pointer.any_released()) {
            self.ed.end_gesture();
        }
        if self.text_edit.is_none() {
            widgets::caption(
                ui,
                "Click the canvas to add text, or click text to edit it.",
            );
        }
    }

    fn preset_preview(
        &mut self,
        ctx: &egui::Context,
        name: &'static str,
        p: &egaku_core::brush::BrushParams,
    ) -> egui::TextureId {
        if let Some(t) = self.preset_previews.get(name) {
            return t.id();
        }
        let img = stroke_preview(p);
        let t = ctx.load_texture(format!("preset-{name}"), img, egui::TextureOptions::LINEAR);
        let id = t.id();
        self.preset_previews.insert(name, t);
        id
    }

    // ---- tool strip -----------------------------------------------------------

    pub fn tool_strip(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            for t in Tool::STRIP {
                let Some(t) = t else {
                    ui.add_space(8.0);
                    continue;
                };
                let sel = self.prefs.tool == t && !(self.free_transform && t != Tool::Move);
                let icon = if t == Tool::Select {
                    self.prefs.select_shape.icon()
                } else {
                    t.icon()
                };
                let name = if t == Tool::Select {
                    format!(
                        "{} ({})",
                        self.prefs.select_shape.name(),
                        self.prefs.select_shape.key()
                    )
                } else {
                    format!("{} ({})", t.name(), t.key())
                };
                let r = icon_button(ui, icon, 36.0, sel, "");
                let r = r.on_hover_ui(|ui| {
                    ui.label(egui::RichText::new(&name).font(theme::semibold(12.5)));
                    ui.label(
                        egui::RichText::new(t.blurb())
                            .size(12.0)
                            .color(theme::TEXT_DIM),
                    );
                });
                if r.clicked() {
                    self.set_tool(t);
                }
                if t == Tool::Select {
                    // A small corner mark: more shapes on right-click.
                    let p = r.rect.right_bottom() - vec2(5.0, 5.0);
                    ui.painter().add(egui::Shape::convex_polygon(
                        vec![p, p - vec2(4.0, 0.0), p - vec2(0.0, 4.0)],
                        theme::TEXT_FAINT,
                        Stroke::NONE,
                    ));
                    egui::Popup::context_menu(&r).show(|ui| {
                        for s in SelectShape::ALL {
                            if ui
                                .button(widgets::il(
                                    s.icon(),
                                    &format!("{}   {}", s.name(), s.key()),
                                ))
                                .clicked()
                            {
                                self.prefs.select_shape = s;
                                self.set_tool(Tool::Select);
                            }
                        }
                    });
                }
            }
            // Main / second colour.
            ui.add_space(6.0);
            let (rect, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
            let bg_r = Rect::from_min_size(rect.min + vec2(14.0, 14.0), vec2(24.0, 24.0));
            let fg_r = Rect::from_min_size(rect.min + vec2(2.0, 2.0), vec2(24.0, 24.0));
            let to_c = |c: [u8; 3]| Color32::from_rgb(c[0], c[1], c[2]);
            let bg_resp = ui.interact(bg_r, ui.scope_id().with("bgsw"), Sense::click());
            ui.painter().rect_filled(bg_r, 6.0, to_c(self.prefs.bg));
            ui.painter().rect_stroke(
                bg_r,
                6.0,
                Stroke::new(1.5, theme::BG_PANEL),
                StrokeKind::Outside,
            );
            ui.painter().rect_filled(fg_r, 6.0, to_c(self.prefs.fg));
            ui.painter().rect_stroke(
                fg_r,
                6.0,
                Stroke::new(2.0, theme::BG_PANEL),
                StrokeKind::Outside,
            );
            ui.painter().rect_stroke(
                fg_r,
                6.0,
                Stroke::new(1.0, Color32::from_white_alpha(40)),
                StrokeKind::Inside,
            );
            if bg_resp
                .on_hover_text("Second colour — click to swap (X)")
                .clicked()
            {
                self.swap_colors();
            }
        });
    }

    // ---- side dock --------------------------------------------------------------

    pub fn side_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let w = ui.available_width();
        let mut open = self.prefs.panel_colour;
        widgets::panel_header(ui, "Colour", &mut open, |ui| {
            let mut hex = self
                .hex_edit
                .clone()
                .unwrap_or_else(|| egaku_core::color::Rgb::from_u8(self.prefs.fg).to_hex());
            let r = ui.add(
                egui::TextEdit::singleline(&mut hex)
                    .desired_width(70.0)
                    .font(egui::TextStyle::Monospace)
                    .align(egui::Align2::CENTER_CENTER),
            );
            if r.changed() {
                if let Some(c) = egaku_core::color::Rgb::from_hex(&hex) {
                    self.prefs.fg = c.to_u8();
                    self.fg_hsv = c.to_hsv();
                    self.recolor_text();
                }
                self.hex_edit = Some(hex);
            }
            if r.lost_focus() {
                self.hex_edit = None;
            }
        });
        self.prefs.panel_colour = open;
        if open {
            let mut hsv = self.fg_hsv;
            if color_picker::picker(ui, &mut hsv, w) {
                self.fg_hsv = hsv;
                self.prefs.fg = color_picker::from_hsv(hsv);
                self.hex_edit = None;
                self.recolor_text();
            }
            ui.add_space(2.0);
        }
        widgets::divider(ui);
        let mut open = self.prefs.panel_swatches;
        widgets::panel_header(ui, "Swatches", &mut open, |ui| {
            if icon_button(ui, ph::PLUS, 22.0, false, "Add the main colour").clicked() {
                let c = self.prefs.fg;
                if !self.prefs.palette.contains(&c) {
                    self.prefs.palette.push(c);
                }
            }
        });
        self.prefs.panel_swatches = open;
        if open {
            let n = ((w + 4.0) / 22.0).floor().max(1.0) as usize;
            let sz = (w + 4.0) / n as f32 - 4.0;
            let mut remove = None;
            let palette = self.prefs.palette.clone();
            for row in palette.chunks(n).enumerate() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (i, c) in row.1.iter().enumerate() {
                        let r = widgets::swatch(ui, *c, vec2(sz, sz), *c == self.prefs.fg);
                        if r.clicked() {
                            self.set_fg(*c);
                        }
                        egui::Popup::context_menu(&r).show(|ui| {
                            if ui.button("Remove swatch").clicked() {
                                remove = Some(row.0 * n + i);
                            }
                        });
                    }
                });
            }
            if let Some(i) = remove {
                self.prefs.palette.remove(i);
            }
            if !self.prefs.recent.is_empty() {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    widgets::caption(ui, "Recent");
                    let recent = self.prefs.recent.clone();
                    let k = ((ui.available_width() + 3.0) / 17.0) as usize;
                    for c in recent.iter().take(k) {
                        if widgets::swatch(ui, *c, vec2(14.0, 14.0), *c == self.prefs.fg).clicked()
                        {
                            self.set_fg(*c);
                        }
                    }
                });
            }
        }
        widgets::divider(ui);
        self.layers_panel(ui);
    }

    fn layers_panel(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut open = true;
        let active = self.ed.doc.active_id();
        widgets::panel_header(ui, "Layers", &mut open, |ui| {
            let r = icon_button(ui, ph::DOTS_THREE, 22.0, false, "More");
            egui::Popup::menu(&r).show(|ui| self.layer_menu(ui));
            if icon_button(ui, ph::TRASH, 22.0, false, "Delete layer").clicked() {
                self.ed.delete_layer(active);
            }
            if icon_button(ui, ph::COPY, 22.0, false, "Duplicate (Ctrl+J)").clicked() {
                self.ed.duplicate_layer(active);
            }
            if icon_button(ui, ph::FOLDER_SIMPLE_PLUS, 22.0, false, "New group").clicked() {
                self.ed.add_group();
            }
            if icon_button(ui, ph::PLUS, 22.0, false, "New layer (Ctrl+Shift+N)").clicked() {
                self.ed.add_layer();
            }
        });

        // Properties of the active layer.
        let (mut blend, mut opacity, alpha_lock, clip, kind) = {
            let l = self.ed.doc.active_layer();
            (l.blend, l.opacity, l.alpha_lock, l.clip, l.kind())
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let w = ui.available_width();
            egui::ComboBox::from_id_salt("blend")
                .width(w - 64.0 - 112.0)
                .selected_text(blend.name())
                .show_ui(ui, |ui| {
                    for (i, m) in BlendMode::ALL.into_iter().enumerate() {
                        if [1, 5, 8].contains(&i) {
                            ui.separator();
                        }
                        ui.selectable_value(&mut blend, m, m.name());
                    }
                });
            if blend != self.ed.doc.active_layer().blend {
                self.ed.set_layer(active, "Blend mode", |l| l.blend = blend);
            }
            let r = pct(ui, "", &mut opacity, 108.0).on_hover_text("Layer opacity");
            if r.changed() {
                self.ed
                    .set_layer_coalesced(active, "Layer opacity", |l| l.opacity = opacity);
            }
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.ed.end_gesture();
            }
            let mut lock = alpha_lock;
            ui.add_enabled_ui(kind == Kind::Raster, |ui| {
                if toggle(ui, &mut lock, ph::LOCK_SIMPLE, "", "Lock transparency (/)").changed() {
                    self.ed
                        .set_layer(active, "Alpha lock", |l| l.alpha_lock = lock);
                }
            });
            let mut c = clip;
            if toggle(
                ui,
                &mut c,
                ph::ARROW_ELBOW_DOWN_RIGHT,
                "",
                "Clip to the layer below (Ctrl+Alt+G)",
            )
            .changed()
            {
                self.ed.toggle_clip(active);
            }
        });
        ui.add_space(4.0);

        let list_h = ui.available_height().max(60.0);
        egui::ScrollArea::vertical()
            .max_height(list_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.layer_rows(ui, &ctx);
            });
    }

    fn layer_thumb(
        &mut self,
        ctx: &egui::Context,
        id: LayerId,
        budget: &mut usize,
    ) -> Option<egui::TextureId> {
        let l = self.ed.doc.find(id)?;
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
        let tid = tex.id();
        self.thumbs.insert(l.id, (key, tex));
        Some(tid)
    }

    fn layer_rows(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let rows = self.ed.doc.display_rows();
        let row_h = 44.0;
        let mut budget = 6; // thumbnails regenerated per frame
        let mut select = None;
        let mut toggle_vis = None;
        let mut toggle_open = None;
        let mut drop_at: Option<Place> = None;
        let pointer = ctx.input(|i| i.pointer.interact_pos());
        let w = ui.available_width();
        let mut ids = vec![];
        self.ed.doc.walk(|l| ids.push(l.id));
        self.thumbs.retain(|id, _| ids.contains(id));
        let active = self.ed.doc.active_id();

        for (id, depth) in rows {
            let (rect, resp) = ui.allocate_exact_size(vec2(w, row_h), Sense::click_and_drag());
            let l = self.ed.doc.find(id).unwrap();
            let (name, visible, blend, opacity, lock, clip, kind) = (
                l.name.clone(),
                l.visible,
                l.blend,
                l.opacity,
                l.alpha_lock,
                l.clip,
                l.kind(),
            );
            let expanded = matches!(l.content, Content::Group { expanded: true, .. });
            let eff_visible = self.ed.doc.is_effectively_visible(id);
            let is_active = id == active;
            let p = ui.painter();
            let r = rect.shrink2(vec2(0.0, 1.5));
            if is_active {
                p.rect_filled(r, 6.0, theme::BG_ACTIVE);
                p.rect_filled(
                    Rect::from_min_size(r.min + vec2(0.0, 8.0), vec2(3.0, r.height() - 16.0)),
                    2.0,
                    theme::ACCENT,
                );
            } else if resp.hovered() {
                p.rect_filled(r, 6.0, theme::BG_RAISED);
            }
            let indent = depth as f32 * 14.0;
            let mut x = rect.left() + 6.0 + indent;

            // Visibility.
            let eye = Rect::from_center_size(pos2(x + 11.0, rect.center().y), vec2(22.0, 22.0));
            let eye_r = ui.interact(eye, ui.scope_id().with(("eye", id)), Sense::click());
            ui.painter().text(
                eye.center(),
                Align2::CENTER_CENTER,
                if visible { ph::EYE } else { ph::EYE_SLASH },
                theme::icon_font(15.0),
                if eye_r.hovered() {
                    Color32::WHITE
                } else if visible {
                    theme::TEXT_DIM
                } else {
                    theme::TEXT_FAINT
                },
            );
            if eye_r.clicked() {
                toggle_vis = Some(id);
            }
            x += 24.0;

            // Disclosure for groups; a hook for clipped layers.
            let caret = Rect::from_center_size(pos2(x + 7.0, rect.center().y), vec2(14.0, 22.0));
            let mut caret_r = None;
            if kind == Kind::Group {
                let cr = ui.interact(caret, ui.scope_id().with(("caret", id)), Sense::click());
                ui.painter().text(
                    caret.center(),
                    Align2::CENTER_CENTER,
                    if expanded {
                        ph::CARET_DOWN
                    } else {
                        ph::CARET_RIGHT
                    },
                    theme::icon_font(11.0),
                    if cr.hovered() {
                        Color32::WHITE
                    } else {
                        theme::TEXT_DIM
                    },
                );
                if cr.clicked() {
                    toggle_open = Some((id, !expanded));
                }
                caret_r = Some(cr);
            } else if clip {
                ui.painter().text(
                    caret.center(),
                    Align2::CENTER_CENTER,
                    ph::ARROW_ELBOW_DOWN_RIGHT,
                    theme::icon_font(12.0),
                    theme::ACCENT,
                );
            }
            x += 16.0;

            // Thumbnail (or a folder for groups) in the document's aspect.
            let slot = Rect::from_min_size(pos2(x, rect.top() + 6.0), vec2(40.0, row_h - 12.0));
            if kind == Kind::Group {
                ui.painter().rect_filled(slot, 5.0, theme::BG_RAISED);
                ui.painter().text(
                    slot.center(),
                    Align2::CENTER_CENTER,
                    if expanded {
                        ph::FOLDER_OPEN
                    } else {
                        ph::FOLDER_SIMPLE
                    },
                    theme::icon_font(18.0),
                    theme::TEXT_DIM,
                );
            } else {
                let (dw, dh) = (self.ed.doc.width as f32, self.ed.doc.height as f32);
                let s = (slot.width() / dw).min(slot.height() / dh);
                let th = Rect::from_center_size(
                    slot.center(),
                    vec2((dw * s).max(4.0), (dh * s).max(4.0)),
                );
                checker(ui.painter(), th);
                if let Some(tex) = self.layer_thumb(ctx, id, &mut budget) {
                    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    ui.painter().image(tex, th, uv, Color32::WHITE);
                }
                ui.painter().rect_stroke(
                    th,
                    2.0,
                    Stroke::new(1.0, Color32::from_black_alpha(160)),
                    StrokeKind::Outside,
                );
                let badge = match kind {
                    Kind::Text => Some(ph::TEXT_T),
                    Kind::Image => Some(ph::IMAGE),
                    _ => None,
                };
                if let Some(b) = badge {
                    let br = Rect::from_center_size(
                        slot.right_bottom() - vec2(3.0, 3.0),
                        vec2(16.0, 16.0),
                    );
                    ui.painter().rect_filled(br, 4.0, theme::BG_APP);
                    ui.painter().text(
                        br.center(),
                        Align2::CENTER_CENTER,
                        b,
                        theme::icon_font(11.0),
                        theme::TEXT,
                    );
                }
            }
            x = slot.right() + 10.0;

            // Name, or an inline editor when renaming.
            let name_rect = Rect::from_min_max(
                pos2(x, rect.top() + 5.0),
                pos2(rect.right() - 26.0, rect.top() + 25.0),
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
                        self.ed.set_layer(id, "Rename layer", |l| l.name = text);
                    }
                    self.renaming = None;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.renaming = None;
                }
            } else {
                let col = if eff_visible {
                    theme::TEXT
                } else {
                    theme::TEXT_FAINT
                };
                let font = if is_active {
                    theme::semibold(13.0)
                } else {
                    egui::FontId::proportional(13.0)
                };
                let galley = ui
                    .painter()
                    .layout(name.clone(), font, col, name_rect.width());
                let clipped = ui
                    .painter()
                    .with_clip_rect(name_rect.expand2(vec2(0.0, 4.0)));
                clipped.galley(
                    pos2(name_rect.left(), name_rect.center().y - 8.0),
                    galley,
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
            if sub.is_empty() {
                sub = match kind {
                    Kind::Text => "Text".into(),
                    Kind::Image => "Image".into(),
                    Kind::Group => format!(
                        "{} layers",
                        self.ed.doc.find(id).map_or(0, |l| l.children().len())
                    ),
                    Kind::Raster => String::new(),
                };
            }
            ui.painter().text(
                pos2(x, rect.bottom() - 12.0),
                Align2::LEFT_CENTER,
                sub,
                egui::FontId::proportional(11.0),
                theme::TEXT_FAINT,
            );
            if lock {
                ui.painter().text(
                    pos2(rect.right() - 14.0, rect.center().y),
                    Align2::CENTER_CENTER,
                    ph::LOCK_SIMPLE,
                    theme::icon_font(12.0),
                    theme::TEXT_DIM,
                );
            }

            let on_control = eye_r.hovered() || caret_r.as_ref().is_some_and(|c| c.hovered());
            if resp.double_clicked() && !on_control {
                if kind == Kind::Text {
                    self.ed.select_layer(id);
                    self.set_tool(Tool::Text);
                    if let Content::Text(o) = &self.ed.doc.active_layer().content {
                        self.text_edit = Some(crate::app::TextEditing {
                            layer: Some(id),
                            obj: (**o).clone(),
                        });
                    }
                } else {
                    self.renaming = Some((id, name.clone()));
                }
            } else if resp.clicked() && !on_control {
                if ui.input(|i| i.modifiers.command) {
                    // Ctrl-click a layer to select its pixels.
                    self.ed.select_layer_pixels(id, SelectOp::Replace);
                } else {
                    select = Some(id);
                }
            }
            egui::Popup::context_menu(&resp).show(|ui| {
                self.ed.select_layer(id);
                if ui.button("Rename").clicked() {
                    self.renaming = Some((id, name.clone()));
                }
                ui.separator();
                self.layer_menu(ui);
            });
            if resp.drag_started() {
                self.layer_drag = Some(id);
            }
            // Drop target: above or below this row, or into a group.
            if let (Some(from), Some(pp)) = (self.layer_drag, pointer)
                && from != id
                && rect.contains(pp)
                && !self.ed.doc.ancestors(id).contains(&from)
            {
                let place = self.ed.doc.place_of(id).unwrap();
                let rel = (pp.y - rect.top()) / rect.height();
                let into = kind == Kind::Group && (0.3..0.7).contains(&rel);
                if into {
                    ui.painter().rect_stroke(
                        r,
                        6.0,
                        Stroke::new(2.0, theme::ACCENT),
                        StrokeKind::Inside,
                    );
                    let n = self.ed.doc.find(id).map_or(0, |l| l.children().len());
                    drop_at = Some(Place {
                        parent: Some(id),
                        index: n,
                    });
                } else {
                    let above = rel < 0.5;
                    let y = if above { rect.top() } else { rect.bottom() };
                    ui.painter().hline(
                        (rect.left() + indent)..=rect.right(),
                        y,
                        Stroke::new(2.0, theme::ACCENT),
                    );
                    // Rows run top-down, indices bottom-up.
                    let mut index = if above { place.index + 1 } else { place.index };
                    if let Some(fp) = self.ed.doc.place_of(from)
                        && fp.parent == place.parent
                        && fp.index < index
                    {
                        index -= 1;
                    }
                    drop_at = Some(Place {
                        parent: place.parent,
                        index,
                    });
                }
            }
        }

        if let Some(id) = toggle_vis {
            let v = !self.ed.doc.find(id).unwrap().visible;
            self.ed
                .set_layer(id, if v { "Show layer" } else { "Hide layer" }, |l| {
                    l.visible = v
                });
        }
        if let Some((id, o)) = toggle_open {
            self.ed.set_expanded(id, o);
        }
        if let Some(id) = select {
            self.finish_modes();
            self.ed.select_layer(id);
        }
        if self.layer_drag.is_some() && !ctx.input(|i| i.pointer.any_down()) {
            if let (Some(from), Some(to)) = (self.layer_drag, drop_at) {
                self.ed.move_layer(from, to);
            }
            self.layer_drag = None;
        }
    }

    // ---- timeline -----------------------------------------------------------------

    pub fn timeline(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let s = &self.ed.doc.state;
        let (frames, frame, fps) = (s.frames, s.frame, s.fps);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(egui::RichText::new("Timeline").font(theme::semibold(12.5)));
            ui.add_space(8.0);
            if icon_button(ui, ph::SKIP_BACK, theme::ROW, false, "Previous frame (,)").clicked() {
                self.ed.set_frame(frame.saturating_sub(1));
            }
            let playing = self.playing.is_some();
            if icon_button(
                ui,
                if playing { ph::PAUSE } else { ph::PLAY },
                theme::ROW,
                playing,
                "Play / pause (P)",
            )
            .clicked()
            {
                self.toggle_play(&ctx);
            }
            if icon_button(ui, ph::SKIP_FORWARD, theme::ROW, false, "Next frame (.)").clicked() {
                self.ed.set_frame((frame + 1).min(frames - 1));
            }
            ui.separator();
            if icon_button(ui, ph::PLUS, theme::ROW, false, "New blank frame").clicked() {
                self.playing = None;
                self.finish_modes();
                self.ed.add_frame(false);
            }
            if icon_button(ui, ph::COPY, theme::ROW, false, "Duplicate frame").clicked() {
                self.playing = None;
                self.finish_modes();
                self.ed.add_frame(true);
            }
            ui.add_enabled_ui(frames > 1, |ui| {
                if icon_button(ui, ph::TRASH, theme::ROW, false, "Delete frame").clicked() {
                    self.playing = None;
                    self.ed.delete_frame();
                }
                if icon_button(ui, ph::CARET_LEFT, theme::ROW, false, "Move frame earlier")
                    .clicked()
                {
                    self.ed.move_frame(-1);
                }
                if icon_button(ui, ph::CARET_RIGHT, theme::ROW, false, "Move frame later").clicked()
                {
                    self.ed.move_frame(1);
                }
            });
            ui.separator();
            let mut onion = self.prefs.onion;
            if toggle(
                ui,
                &mut onion,
                ph::STACK_SIMPLE,
                "Onion skin",
                "Show neighbouring frames faintly (red before, green after)",
            )
            .changed()
            {
                self.prefs.onion = onion;
            }
            let mut f = fps as f32;
            if field(
                ui,
                "FPS",
                &mut f,
                1.0,
                60.0,
                Scale::Linear,
                &|v| format!("{v:.0}"),
                90.0,
            )
            .changed()
            {
                self.ed.set_fps(f.round() as u32);
            }
            if ui.input(|i| i.pointer.any_released()) {
                self.ed.end_gesture();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                if frames == 1
                    && icon_button(ui, ph::X, theme::ROW, false, "Hide timeline").clicked()
                {
                    self.prefs.show_timeline = false;
                }
                widgets::caption(ui, &format!("Frame {} of {frames}", frame + 1));
            });
        });
        let want_onion = (self.prefs.onion && frames > 1).then(egaku_core::editor::Onion::default);
        self.ed.set_onion(want_onion);

        // Frame cells.
        ui.add_space(2.0);
        egui::ScrollArea::horizontal()
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    let l = self.ed.doc.active_layer();
                    let has: Vec<bool> = (0..frames)
                        .map(|f| self.ed.doc.cel(l, f).is_some_and(|c| !c.is_empty()))
                        .collect();
                    for (f, has) in has.into_iter().enumerate() {
                        let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 34.0), Sense::click());
                        let sel = f == frame;
                        let bg = if sel {
                            theme::ACCENT_BG
                        } else if resp.hovered() {
                            theme::BG_HOVER
                        } else {
                            theme::BG_RAISED
                        };
                        ui.painter().rect_filled(rect, 5.0, bg);
                        if sel {
                            ui.painter().rect_stroke(
                                rect,
                                5.0,
                                Stroke::new(1.5, theme::ACCENT),
                                StrokeKind::Inside,
                            );
                        }
                        ui.painter().text(
                            rect.center_top() + vec2(0.0, 11.0),
                            Align2::CENTER_CENTER,
                            format!("{}", f + 1),
                            egui::FontId::proportional(11.0),
                            if sel { Color32::WHITE } else { theme::TEXT_DIM },
                        );
                        let dot = rect.center_bottom() - vec2(0.0, 9.0);
                        if has {
                            ui.painter().circle_filled(
                                dot,
                                3.5,
                                if sel { theme::ACCENT } else { theme::TEXT_DIM },
                            );
                        } else {
                            ui.painter().circle_stroke(
                                dot,
                                3.0,
                                Stroke::new(1.0, theme::TEXT_FAINT),
                            );
                        }
                        if resp
                            .on_hover_text(if has {
                                "This layer has a drawing here"
                            } else {
                                "Empty on this layer"
                            })
                            .clicked()
                        {
                            self.playing = None;
                            self.finish_modes();
                            self.ed.set_frame(f);
                        }
                    }
                });
            });
    }
}

/// A quarter turn about a centre, keeping whole pixels aligned when the
/// box has odd/even dimensions.
fn rot90(cx: f32, cy: f32, w: f32, h: f32, dir: f32) -> egaku_core::transform::Affine {
    use egaku_core::transform::Affine;
    // Snap the centre so the rotated box lands on the pixel grid.
    let (cx, cy) = if (w.round() as i64 - h.round() as i64) % 2 != 0 {
        (cx.floor() + 0.5, cy.floor() + 0.5)
    } else {
        (cx.round(), cy.round())
    };
    let r = Affine {
        m: if dir > 0.0 {
            [0.0, 1.0, -1.0, 0.0, 0.0, 0.0]
        } else {
            [0.0, -1.0, 1.0, 0.0, 0.0, 0.0]
        },
    };
    Affine::translate(cx, cy)
        .compose(r)
        .compose(Affine::translate(-cx, -cy))
}

fn checker(p: &egui::Painter, r: Rect) {
    p.rect_filled(r, 2.0, Color32::from_gray(176));
    let cell = 5.0;
    let clip = p.with_clip_rect(r);
    for cy in 0..((r.height() / cell).ceil() as i32) {
        for cx in 0..((r.width() / cell).ceil() as i32) {
            if (cx + cy) % 2 == 0 {
                clip.rect_filled(
                    Rect::from_min_size(
                        r.min + vec2(cx as f32 * cell, cy as f32 * cell),
                        vec2(cell, cell),
                    ),
                    0.0,
                    Color32::from_gray(212),
                );
            }
        }
    }
}

/// Renders a sample S-curve stroke with the brush engine itself.
fn stroke_preview(p: &egaku_core::brush::BrushParams) -> egui::ColorImage {
    use egaku_core::brush::{Mask, StrokeEngine};
    let (w, h) = (256u32, 64u32);
    let mut params = *p;
    params.size = if p.pixel {
        4.0
    } else {
        p.size.clamp(2.0, 22.0)
    };
    params.smoothing = 0.0;
    let mut eng = StrokeEngine::new(params);
    let mut mask = Mask::new(w, h);
    let mut dabs = vec![];
    let mut dirty = vec![];
    for i in 0..=80 {
        let t = i as f32 / 80.0;
        let x = 16.0 + t * 224.0;
        let y = 32.0 + (t * std::f32::consts::TAU).sin() * 14.0;
        let pr = (t * std::f32::consts::PI).sin().max(0.15);
        dabs.clear();
        eng.add_point(x, y, pr, &mut dabs);
        for d in &dabs {
            mask.stamp(d, &mut dirty);
        }
    }
    dabs.clear();
    eng.finish(&mut dabs);
    for d in &dabs {
        mask.stamp(d, &mut dirty);
    }
    let mut px = vec![Color32::TRANSPARENT; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let a = (mask.value(x, y) >> 8) as u8;
            px[(y * w + x) as usize] = Color32::from_white_alpha(a);
        }
    }
    egui::ColorImage::new([w as usize, h as usize], px)
}
