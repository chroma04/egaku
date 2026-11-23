//! Modal dialogs.

use std::sync::Arc;

use eframe::egui::{self, Align2, Color32, Key, Rect, Sense, Stroke, StrokeKind, vec2};
use egaku_core::adjust::Adjustment;
use egaku_core::document::{Document, MAX_CANVAS};
use egaku_core::io::Format;
use egui_phosphor::regular as ph;

use crate::app::{AnimFormat, App, Modal, NumberOp, Pending};
use crate::theme;
use crate::widgets::{self, Scale, field, pct, segmented};

/// Shows a modal with a title; the body returns `Some(true)` to confirm,
/// `Some(false)` to cancel and `None` to stay open.
fn modal<R>(
    ctx: &egui::Context,
    id: &str,
    width: f32,
    title: &str,
    body: impl FnOnce(&mut egui::Ui) -> Option<R>,
) -> (Option<R>, bool) {
    let r = egui::Modal::new(egui::Id::unique(id)).show(ctx, |ui| {
        ui.set_width(width);
        ui.label(egui::RichText::new(title).font(theme::semibold(17.0)));
        ui.add_space(10.0);
        body(ui)
    });
    let closed = r.should_close();
    (r.inner, closed)
}

/// Primary / secondary buttons, right-aligned.
fn buttons(ui: &mut egui::Ui, ok: &str, enter_ok: bool) -> Option<bool> {
    ui.add_space(14.0);
    let mut out = None;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let primary = egui::Button::new(egui::RichText::new(ok).color(Color32::WHITE))
            .fill(theme::ACCENT)
            .min_size(vec2(84.0, 28.0));
        if ui.add(primary).clicked() || (enter_ok && ui.input(|i| i.key_pressed(Key::Enter))) {
            out = Some(true);
        }
        if ui
            .add(egui::Button::new("Cancel").min_size(vec2(76.0, 28.0)))
            .clicked()
        {
            out = Some(false);
        }
    });
    out
}

fn size_row(ui: &mut egui::Ui, label: &str, v: &mut u32) -> bool {
    ui.horizontal(|ui| {
        ui.add_sized(
            [56.0, 22.0],
            egui::Label::new(egui::RichText::new(label).color(theme::TEXT_DIM)),
        );
        ui.add(
            egui::DragValue::new(v)
                .range(1..=MAX_CANVAS)
                .suffix(" px")
                .speed(4.0),
        )
        .changed()
    })
    .inner
}

impl App {
    pub fn modals(&mut self, ctx: &egui::Context) {
        let Some(current) = self.modal.take() else {
            return;
        };
        match current {
            Modal::NewDoc => self.new_doc_dialog(ctx),
            Modal::Unsaved(pending) => {
                let what = match pending {
                    Pending::Quit => "quitting",
                    Pending::New => "starting a new canvas",
                    Pending::Open(_) => "opening another file",
                };
                let name = self.name.clone();
                let (r, closed) = modal(ctx, "unsaved", 380.0, "Unsaved changes", |ui| {
                    ui.label(format!("Save changes to “{name}” before {what}?"));
                    ui.add_space(14.0);
                    let mut out = None;
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let save =
                            egui::Button::new(egui::RichText::new("Save").color(Color32::WHITE))
                                .fill(theme::ACCENT)
                                .min_size(vec2(76.0, 28.0));
                        if ui.add(save).clicked() {
                            out = Some(1);
                        }
                        if ui
                            .add(egui::Button::new("Cancel").min_size(vec2(76.0, 28.0)))
                            .clicked()
                        {
                            out = Some(3);
                        }
                        if ui
                            .add(egui::Button::new("Don't save").min_size(vec2(90.0, 28.0)))
                            .clicked()
                        {
                            out = Some(2);
                        }
                    });
                    out
                });
                match (r, closed) {
                    (Some(1), _) => {
                        if matches!(pending, Pending::Quit) {
                            self.set_quit_after_save();
                        }
                        self.save(ctx);
                        // Saving runs in the background on a snapshot; carry on
                        // once it is queued (not if the save dialog was cancelled).
                        if !matches!(pending, Pending::Quit) && self.io_busy.is_some() {
                            self.perform(ctx, pending);
                        }
                    }
                    (Some(2), _) => self.perform(ctx, pending),
                    (Some(3), _) | (_, true) => {}
                    _ => self.modal = Some(Modal::Unsaved(pending)),
                }
            }
            Modal::Error(msg) => {
                let (r, closed) = modal(ctx, "error", 420.0, "Something went wrong", |ui| {
                    ui.label(&msg);
                    ui.add_space(12.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::Button::new("OK").min_size(vec2(76.0, 28.0)))
                            .clicked()
                            .then_some(())
                    })
                    .inner
                });
                if r.is_none() && !closed {
                    self.modal = Some(Modal::Error(msg));
                }
            }
            Modal::Shortcuts => {
                let (r, closed) = modal(ctx, "shortcuts", 560.0, "Keyboard & pointer", |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(520.0)
                        .show(ui, |ui| {
                            egui::Grid::new("keys")
                                .num_columns(2)
                                .spacing([20.0, 7.0])
                                .show(ui, |ui| {
                                    for (k, v) in crate::input::SHORTCUTS {
                                        ui.label(
                                            egui::RichText::new(*k).monospace().color(theme::TEXT),
                                        );
                                        ui.label(egui::RichText::new(*v).color(theme::TEXT_DIM));
                                        ui.end_row();
                                    }
                                });
                        });
                    ui.add_space(10.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::Button::new("Close").min_size(vec2(76.0, 28.0)))
                            .clicked()
                            .then_some(())
                    })
                    .inner
                });
                if r.is_none() && !closed {
                    self.modal = Some(Modal::Shortcuts);
                }
            }
            Modal::About => {
                let (r, closed) = modal(ctx, "about", 380.0, "egaku", |ui| {
                    ui.label(
                        egui::RichText::new("egaku (描く): to draw, to paint")
                            .color(theme::TEXT_DIM),
                    );
                    ui.add_space(8.0);
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    ui.label("A fast painting and image editor for everyone.");
                    ui.add_space(6.0);
                    widgets::caption(
                        ui,
                        "Inter typeface © The Inter Project Authors (OFL). Phosphor icons (MIT).",
                    );
                    ui.add_space(10.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::Button::new("Close").min_size(vec2(76.0, 28.0)))
                            .clicked()
                            .then_some(())
                    })
                    .inner
                });
                if r.is_none() && !closed {
                    self.modal = Some(Modal::About);
                }
            }
            Modal::Export => {
                let mut fmt = self.prefs.export_format;
                let mut scale = self.prefs.export_scale.max(1);
                let (w, h) = (self.ed.doc.width, self.ed.doc.height);
                let (r, closed) = modal(ctx, "export", 400.0, "Export image", |ui| {
                    let mut kind = match fmt {
                        Format::Png => 0,
                        Format::Jpeg(_) => 1,
                        Format::Webp => 2,
                    };
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [64.0, 22.0],
                            egui::Label::new(egui::RichText::new("Format").color(theme::TEXT_DIM)),
                        );
                        segmented(
                            ui,
                            &mut kind,
                            &[
                                (0, "PNG", "Lossless, keeps transparency"),
                                (1, "JPG", "Small files for photos; no transparency"),
                                (2, "WebP", "Lossless WebP"),
                            ],
                        );
                    });
                    let q = match fmt {
                        Format::Jpeg(q) => q,
                        _ => 90,
                    };
                    fmt = match kind {
                        1 => Format::Jpeg(q),
                        2 => Format::Webp,
                        _ => Format::Png,
                    };
                    if let Format::Jpeg(q) = &mut fmt {
                        let mut v = *q as f32;
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [64.0, 22.0],
                                egui::Label::new(
                                    egui::RichText::new("Quality").color(theme::TEXT_DIM),
                                ),
                            );
                            field(
                                ui,
                                "",
                                &mut v,
                                1.0,
                                100.0,
                                Scale::Linear,
                                &|v| format!("{v:.0}"),
                                200.0,
                            );
                        });
                        *q = v.round() as u8;
                    }
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [64.0, 22.0],
                            egui::Label::new(egui::RichText::new("Scale").color(theme::TEXT_DIM)),
                        );
                        segmented(
                            ui,
                            &mut scale,
                            &[
                                (1, "1×", "Actual size"),
                                (2, "2×", "Double, crisp pixels"),
                                (4, "4×", "Quadruple, crisp pixels"),
                                (8, "8×", "8×, crisp pixels"),
                            ],
                        );
                    });
                    ui.add_space(4.0);
                    widgets::caption(
                        ui,
                        &format!(
                            "{} × {} px{}",
                            w * scale,
                            h * scale,
                            if scale > 1 {
                                " · enlarged without smoothing, ideal for pixel art"
                            } else {
                                ""
                            }
                        ),
                    );
                    buttons(ui, "Export…", true)
                });
                match (r, closed) {
                    (Some(true), _) => self.export(ctx, fmt, scale),
                    (Some(false), _) | (_, true) => {}
                    _ => {
                        self.prefs.export_format = fmt;
                        self.prefs.export_scale = scale;
                        self.modal = Some(Modal::Export);
                    }
                }
            }
            Modal::ExportAnim(mut fmt) => {
                let mut scale = self.prefs.export_scale.max(1);
                let frames = self.ed.doc.state.frames;
                let mut cols: u32 = ctx
                    .data(|d| d.get_temp(egui::Id::unique("sheet-cols")))
                    .unwrap_or(frames.min(8) as u32);
                let (r, closed) = modal(ctx, "export-anim", 420.0, "Export animation", |ui| {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [64.0, 22.0],
                            egui::Label::new(egui::RichText::new("Format").color(theme::TEXT_DIM)),
                        );
                        segmented(
                            ui,
                            &mut fmt,
                            &[
                                (AnimFormat::Gif, "GIF", "Plays everywhere; 256 colours"),
                                (
                                    AnimFormat::Apng,
                                    "APNG",
                                    "Animated PNG: full colour and transparency",
                                ),
                                (
                                    AnimFormat::Sheet,
                                    "Sheet",
                                    "All frames in one image (for games)",
                                ),
                                (AnimFormat::Sequence, "Files", "One PNG per frame"),
                            ],
                        );
                    });
                    if fmt == AnimFormat::Sheet {
                        let mut c = cols as f32;
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [64.0, 22.0],
                                egui::Label::new(
                                    egui::RichText::new("Columns").color(theme::TEXT_DIM),
                                ),
                            );
                            field(
                                ui,
                                "",
                                &mut c,
                                1.0,
                                frames.max(1) as f32,
                                Scale::Linear,
                                &|v| format!("{v:.0}"),
                                200.0,
                            );
                        });
                        cols = c.round().max(1.0) as u32;
                    }
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [64.0, 22.0],
                            egui::Label::new(egui::RichText::new("Scale").color(theme::TEXT_DIM)),
                        );
                        segmented(
                            ui,
                            &mut scale,
                            &[(1, "1×", ""), (2, "2×", ""), (4, "4×", ""), (8, "8×", "")],
                        );
                    });
                    widgets::caption(
                        ui,
                        &format!("{frames} frames at {} fps", self.ed.doc.state.fps),
                    );
                    buttons(ui, "Export…", true)
                });
                ctx.data_mut(|d| d.insert_temp(egui::Id::unique("sheet-cols"), cols));
                match (r, closed) {
                    (Some(true), _) => {
                        self.prefs.export_scale = scale;
                        self.export_animation(ctx, fmt, scale, cols);
                    }
                    (Some(false), _) | (_, true) => {}
                    _ => {
                        self.prefs.export_scale = scale;
                        self.modal = Some(Modal::ExportAnim(fmt));
                    }
                }
            }
            Modal::ImageSize {
                mut w,
                mut h,
                mut lock,
                mut smooth,
            } => {
                let (ow, oh) = (self.ed.doc.width, self.ed.doc.height);
                let (r, closed) = modal(ctx, "image-size", 360.0, "Image size", |ui| {
                    let (pw, ph_) = (w, h);
                    size_row(ui, "Width", &mut w);
                    size_row(ui, "Height", &mut h);
                    if lock {
                        if w != pw {
                            h = ((w as f64 * oh as f64 / ow as f64).round() as u32)
                                .clamp(1, MAX_CANVAS);
                        } else if h != ph_ {
                            w = ((h as f64 * ow as f64 / oh as f64).round() as u32)
                                .clamp(1, MAX_CANVAS);
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.add_space(60.0);
                        ui.checkbox(&mut lock, "Keep proportions");
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(60.0);
                        for pct in [25u32, 50, 200, 400] {
                            if ui.small_button(format!("{pct}%")).clicked() {
                                w = (ow * pct / 100).clamp(1, MAX_CANVAS);
                                h = (oh * pct / 100).clamp(1, MAX_CANVAS);
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [56.0, 22.0],
                            egui::Label::new(
                                egui::RichText::new("Resample").color(theme::TEXT_DIM),
                            ),
                        );
                        segmented(
                            ui,
                            &mut smooth,
                            &[
                                (true, "Smooth", "Best for photos and painting"),
                                (false, "Pixel", "Nearest neighbour: keeps pixel art crisp"),
                            ],
                        );
                    });
                    buttons(ui, "Resize", true)
                });
                match (r, closed) {
                    (Some(true), _) => {
                        self.finish_modes();
                        self.ed.resize_image(w, h, smooth);
                    }
                    (Some(false), _) | (_, true) => {}
                    _ => self.modal = Some(Modal::ImageSize { w, h, lock, smooth }),
                }
            }
            Modal::CanvasSize {
                mut w,
                mut h,
                mut anchor,
            } => {
                let (ow, oh) = (self.ed.doc.width, self.ed.doc.height);
                let (r, closed) = modal(ctx, "canvas-size", 360.0, "Canvas size", |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            size_row(ui, "Width", &mut w);
                            size_row(ui, "Height", &mut h);
                            widgets::caption(ui, &format!("Currently {ow} × {oh}"));
                        });
                        ui.add_space(12.0);
                        // Anchor grid: where the existing image sits.
                        let (rect, _) = ui.allocate_exact_size(vec2(72.0, 72.0), Sense::hover());
                        for ay in -1..=1 {
                            for ax in -1..=1 {
                                let c = rect.center() + vec2(ax as f32 * 24.0, ay as f32 * 24.0);
                                let cell = Rect::from_center_size(c, vec2(20.0, 20.0));
                                let resp = ui.interact(
                                    cell,
                                    ui.scope_id().with(("anchor", ax, ay)),
                                    Sense::click(),
                                );
                                let sel = anchor == (ax, ay);
                                ui.painter().rect_filled(
                                    cell,
                                    4.0,
                                    if sel {
                                        theme::ACCENT
                                    } else if resp.hovered() {
                                        theme::BG_HOVER
                                    } else {
                                        theme::BG_RAISED
                                    },
                                );
                                if resp.clicked() {
                                    anchor = (ax, ay);
                                }
                            }
                        }
                    });
                    buttons(ui, "Apply", true)
                });
                match (r, closed) {
                    (Some(true), _) => {
                        let off = |a: i32, old: u32, new: u32| match a {
                            -1 => 0,
                            0 => (new as i64 - old as i64) / 2,
                            _ => new as i64 - old as i64,
                        };
                        self.finish_modes();
                        self.ed
                            .resize_canvas(w, h, off(anchor.0, ow, w), off(anchor.1, oh, h));
                    }
                    (Some(false), _) | (_, true) => {}
                    _ => self.modal = Some(Modal::CanvasSize { w, h, anchor }),
                }
            }
            Modal::Adjust(mut a) => {
                let before = a;
                let (r, closed) = modal(ctx, "adjust", 360.0, a.name(), |ui| {
                    adjust_controls(ui, &mut a);
                    buttons(ui, "Apply", true)
                });
                if a != before {
                    self.ed.update_filter(Arc::new(a));
                }
                match (r, closed) {
                    (Some(true), _) => self.ed.commit_filter(a.name()),
                    (Some(false), _) | (_, true) => self.ed.cancel_filter(),
                    _ => self.modal = Some(Modal::Adjust(a)),
                }
            }
            Modal::Number { what, mut value } => {
                let (r, closed) = modal(ctx, "number", 320.0, what.title(), |ui| {
                    let max = if what == NumberOp::Feather {
                        250.0
                    } else {
                        100.0
                    };
                    field(
                        ui,
                        "Pixels",
                        &mut value,
                        0.0,
                        max,
                        Scale::Linear,
                        &|v| format!("{v:.0} px"),
                        280.0,
                    );
                    buttons(ui, "OK", true)
                });
                match (r, closed) {
                    (Some(true), _) => match what {
                        NumberOp::Feather => self.ed.feather_selection(value),
                        NumberOp::Grow => self.ed.grow_selection(value.round() as i32),
                        NumberOp::Shrink => self.ed.grow_selection(-(value.round() as i32)),
                    },
                    (Some(false), _) | (_, true) => {}
                    _ => self.modal = Some(Modal::Number { what, value }),
                }
            }
            Modal::Rasterize(id) => {
                let (name, kind) = self.ed.doc.find(id).map_or((String::new(), "layer"), |l| {
                    (
                        l.name.clone(),
                        match l.kind() {
                            egaku_core::document::Kind::Text => "text layer",
                            egaku_core::document::Kind::Image => "image layer",
                            _ => "layer",
                        },
                    )
                });
                let (r, closed) = modal(ctx, "rasterize", 380.0, "Rasterize layer?", |ui| {
                    ui.label(format!(
                        "“{name}” is a {kind}. To paint on it, it needs to become plain pixels."
                    ));
                    ui.add_space(4.0);
                    widgets::caption(
                        ui,
                        "You won't be able to edit it as text or rescale the original afterwards. Tip: paint on a new layer above instead, clipped to it (Ctrl+Alt+G).",
                    );
                    buttons(ui, "Rasterize", true)
                });
                match (r, closed) {
                    (Some(true), _) => self.ed.rasterize(id),
                    (Some(false), _) | (_, true) => {}
                    _ => self.modal = Some(Modal::Rasterize(id)),
                }
            }
            Modal::Recover(list) => {
                let mut pick: Option<(usize, bool)> = None;
                let (r, closed) = modal(ctx, "recover", 440.0, "Recover unsaved work?", |ui| {
                    ui.label("egaku didn't close properly last time. These autosaved documents can be restored:");
                    ui.add_space(8.0);
                    for (i, e) in list.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(ph::FILE_IMAGE)
                                    .font(theme::icon_font(18.0))
                                    .color(theme::ACCENT),
                            );
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(&e.name).font(theme::semibold(13.0)));
                                let ago = e.when.elapsed().map_or(String::new(), |d| {
                                    let m = d.as_secs() / 60;
                                    if m < 60 {
                                        format!("{m} min ago")
                                    } else if m < 48 * 60 {
                                        format!("{} h ago", m / 60)
                                    } else {
                                        format!("{} days ago", m / 1440)
                                    }
                                });
                                widgets::caption(ui, &format!("Autosaved {ago}"));
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Restore").clicked() {
                                        pick = Some((i, true));
                                    }
                                    if ui.button("Discard").clicked() {
                                        pick = Some((i, false));
                                    }
                                },
                            );
                        });
                    }
                    ui.add_space(10.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::Button::new("Later").min_size(vec2(76.0, 28.0)))
                            .clicked()
                            .then_some(())
                    })
                    .inner
                });
                let mut list = list;
                if let Some((i, restore)) = pick {
                    let e = list.remove(i);
                    if restore {
                        self.open_recovered(ctx, e.path.clone());
                    } else {
                        self.recovery.discard(&e);
                    }
                }
                if r.is_none() && !closed && !list.is_empty() && !pick.is_some_and(|p| p.1) {
                    self.modal = Some(Modal::Recover(list));
                }
            }
        }
    }

    fn new_doc_dialog(&mut self, ctx: &egui::Context) {
        struct P {
            name: &'static str,
            icon: &'static str,
            w: u32,
            h: u32,
            paper: bool,
        }
        let presets = [
            P {
                name: "Screen",
                icon: ph::MONITOR,
                w: 1920,
                h: 1080,
                paper: true,
            },
            P {
                name: "4K",
                icon: ph::TELEVISION,
                w: 3840,
                h: 2160,
                paper: true,
            },
            P {
                name: "Square",
                icon: ph::SQUARE,
                w: 2048,
                h: 2048,
                paper: true,
            },
            P {
                name: "A4 · 300 dpi",
                icon: ph::FILE,
                w: 2480,
                h: 3508,
                paper: true,
            },
            P {
                name: "Phone",
                icon: ph::DEVICE_MOBILE,
                w: 1170,
                h: 2532,
                paper: true,
            },
            P {
                name: "Photo 3:2",
                icon: ph::CAMERA,
                w: 6000,
                h: 4000,
                paper: true,
            },
            P {
                name: "Pixel 64",
                icon: ph::GRID_FOUR,
                w: 64,
                h: 64,
                paper: false,
            },
            P {
                name: "Pixel 256",
                icon: ph::GRID_NINE,
                w: 256,
                h: 256,
                paper: false,
            },
        ];
        let p = &mut self.prefs;
        let mut create = false;
        let mut pixel_choice = false;
        let (r, closed) = modal(ctx, "new-doc", 470.0, "New canvas", |ui| {
            // Preset cards.
            let card = vec2(108.0, 70.0);
            for row in presets.chunks(4) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for pr in row {
                        let (rect, resp) = ui.allocate_exact_size(card, Sense::click());
                        let sel = p.new_width == pr.w && p.new_height == pr.h;
                        ui.painter().rect_filled(
                            rect,
                            8.0,
                            if sel {
                                theme::ACCENT_BG
                            } else if resp.hovered() {
                                theme::BG_HOVER
                            } else {
                                theme::BG_RAISED
                            },
                        );
                        if sel {
                            ui.painter().rect_stroke(
                                rect,
                                8.0,
                                Stroke::new(1.5, theme::ACCENT),
                                StrokeKind::Inside,
                            );
                        }
                        ui.painter().text(
                            rect.center_top() + vec2(0.0, 20.0),
                            Align2::CENTER_CENTER,
                            pr.icon,
                            theme::icon_font(20.0),
                            if sel { theme::ACCENT } else { theme::TEXT_DIM },
                        );
                        ui.painter().text(
                            rect.center_top() + vec2(0.0, 42.0),
                            Align2::CENTER_CENTER,
                            pr.name,
                            egui::FontId::proportional(12.5),
                            theme::TEXT,
                        );
                        ui.painter().text(
                            rect.center_top() + vec2(0.0, 57.0),
                            Align2::CENTER_CENTER,
                            format!("{} × {}", pr.w, pr.h),
                            egui::FontId::proportional(10.5),
                            theme::TEXT_FAINT,
                        );
                        if resp.clicked() {
                            (p.new_width, p.new_height) = (pr.w, pr.h);
                            p.new_paper = pr.paper.then_some([255; 3]);
                            pixel_choice = !pr.paper;
                        }
                        if resp.double_clicked() {
                            create = true;
                        }
                    }
                });
                ui.add_space(4.0);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    size_row(ui, "Width", &mut p.new_width);
                    size_row(ui, "Height", &mut p.new_height);
                });
                if ui
                    .add(
                        egui::Button::new(crate::widgets::ic(ph::ARROWS_LEFT_RIGHT))
                            .min_size(vec2(28.0, 50.0)),
                    )
                    .on_hover_text("Swap width and height")
                    .clicked()
                {
                    (p.new_width, p.new_height) = (p.new_height, p.new_width);
                }
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new("Background").color(theme::TEXT_DIM));
                    ui.horizontal(|ui| {
                        let mut on = p.new_paper.is_some();
                        if segmented(
                            ui,
                            &mut on,
                            &[
                                (true, "Colour", "An opaque paper colour"),
                                (false, "None", "Transparent"),
                            ],
                        ) {
                            p.new_paper = on.then_some([255; 3]);
                        }
                        if let Some(c) = &mut p.new_paper {
                            ui.color_edit_button_srgb(c);
                        }
                    });
                });
            });
            let mb = p.new_width as f64 * p.new_height as f64 * 4.0 / 1048576.0;
            ui.add_space(4.0);
            widgets::caption(
                ui,
                &format!("{mb:.0} MB per fully painted layer; empty areas cost nothing."),
            );
            let b = buttons(ui, "Create", true);
            if create { Some(true) } else { b }
        });
        if pixel_choice {
            // Starting pixel art: switch the brush to pixel mode.
            self.prefs.brush = crate::prefs::presets()[5].params;
            self.prefs.smooth_transform = false;
        }
        match (r, closed) {
            (Some(true), _) => {
                let p = &self.prefs;
                let doc = Document::new(p.new_width.max(1), p.new_height.max(1), p.new_paper);
                self.set_document(doc, None, "Untitled".into());
            }
            (Some(false), _) | (_, true) => {}
            _ => self.modal = Some(Modal::NewDoc),
        }
    }
}

/// Sliders for an adjustment's settings.
fn adjust_controls(ui: &mut egui::Ui, a: &mut Adjustment) {
    let w = 340.0;
    let signed = |v: f32| format!("{:+.0}", v * 100.0);
    match a {
        Adjustment::BrightnessContrast {
            brightness,
            contrast,
        } => {
            field(
                ui,
                "Brightness",
                brightness,
                -1.0,
                1.0,
                Scale::Linear,
                &signed,
                w,
            );
            field(
                ui,
                "Contrast",
                contrast,
                -1.0,
                1.0,
                Scale::Linear,
                &signed,
                w,
            );
        }
        Adjustment::HueSaturation {
            hue,
            saturation,
            lightness,
        } => {
            hue_bar(ui, *hue, w);
            field(
                ui,
                "Hue",
                hue,
                -180.0,
                180.0,
                Scale::Linear,
                &|v| format!("{v:+.0}°"),
                w,
            );
            field(
                ui,
                "Saturation",
                saturation,
                -1.0,
                1.0,
                Scale::Linear,
                &signed,
                w,
            );
            field(
                ui,
                "Lightness",
                lightness,
                -1.0,
                1.0,
                Scale::Linear,
                &signed,
                w,
            );
        }
        Adjustment::Levels {
            black,
            white,
            gamma,
            out_black,
            out_white,
        } => {
            widgets::caption(ui, "Input");
            field(
                ui,
                "Shadows",
                black,
                0.0,
                0.99,
                Scale::Linear,
                &|v| format!("{:.0}", v * 255.0),
                w,
            );
            field(
                ui,
                "Midtones",
                gamma,
                0.1,
                5.0,
                Scale::Log,
                &|v| format!("{v:.2}"),
                w,
            );
            field(
                ui,
                "Highlights",
                white,
                0.01,
                1.0,
                Scale::Linear,
                &|v| format!("{:.0}", v * 255.0),
                w,
            );
            *white = white.max(*black + 0.01);
            widgets::caption(ui, "Output");
            field(
                ui,
                "Black",
                out_black,
                0.0,
                1.0,
                Scale::Linear,
                &|v| format!("{:.0}", v * 255.0),
                w,
            );
            field(
                ui,
                "White",
                out_white,
                0.0,
                1.0,
                Scale::Linear,
                &|v| format!("{:.0}", v * 255.0),
                w,
            );
        }
        Adjustment::Color {
            temperature,
            tint,
            vibrance,
        } => {
            field(
                ui,
                "Temperature",
                temperature,
                -1.0,
                1.0,
                Scale::Linear,
                &signed,
                w,
            )
            .on_hover_text("Cooler ← → warmer");
            field(ui, "Tint", tint, -1.0, 1.0, Scale::Linear, &signed, w)
                .on_hover_text("Green ← → magenta");
            field(
                ui,
                "Vibrance",
                vibrance,
                -1.0,
                1.0,
                Scale::Linear,
                &signed,
                w,
            )
            .on_hover_text("Boosts muted colours more than vivid ones");
        }
        Adjustment::Threshold { level } => {
            field(
                ui,
                "Level",
                level,
                0.0,
                1.0,
                Scale::Linear,
                &|v| format!("{:.0}", v * 255.0),
                w,
            );
        }
        Adjustment::Posterize { levels } => {
            let mut v = *levels as f32;
            field(
                ui,
                "Levels",
                &mut v,
                2.0,
                32.0,
                Scale::Linear,
                &|v| format!("{v:.0}"),
                w,
            );
            *levels = v.round() as u32;
        }
        Adjustment::Blur { radius } => {
            field(
                ui,
                "Radius",
                radius,
                0.5,
                100.0,
                Scale::Log,
                &|v| format!("{v:.1} px"),
                w,
            );
        }
        Adjustment::Sharpen { amount, radius } => {
            pct(ui, "Amount", amount, w);
            field(
                ui,
                "Radius",
                radius,
                0.5,
                20.0,
                Scale::Log,
                &|v| format!("{v:.1} px"),
                w,
            );
        }
        Adjustment::Noise { amount, mono } => {
            pct(ui, "Amount", amount, w);
            ui.checkbox(mono, "Monochrome");
        }
        Adjustment::Pixelate { size } => {
            let mut v = *size as f32;
            field(
                ui,
                "Block size",
                &mut v,
                2.0,
                128.0,
                Scale::Log,
                &|v| format!("{v:.0} px"),
                w,
            );
            *size = v.round() as u32;
        }
        Adjustment::Invert | Adjustment::Desaturate => {}
    }
    ui.add_space(4.0);
    widgets::caption(
        ui,
        "Previewing on the canvas. Applies to the selection if there is one.",
    );
}

/// The hue wheel unrolled, shifted by the current hue offset.
fn hue_bar(ui: &mut egui::Ui, shift: f32, w: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(w, 10.0), Sense::hover());
    let mut mesh = egui::Mesh::default();
    for i in 0..=36 {
        let t = i as f32 / 36.0;
        let c = egaku_core::color::Hsv {
            h: (t + shift / 360.0).rem_euclid(1.0),
            s: 0.85,
            v: 0.95,
        }
        .to_rgb()
        .to_u8();
        let col = Color32::from_rgb(c[0], c[1], c[2]);
        let x = rect.left() + rect.width() * t;
        mesh.colored_vertex(egui::pos2(x, rect.top()), col);
        mesh.colored_vertex(egui::pos2(x, rect.bottom()), col);
        if i > 0 {
            let k = i as u32 * 2;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k + 1, k);
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
}
