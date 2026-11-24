//! Application state, file operations, autosave and crash recovery.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::{self, Pos2};
use egaku_core::Editor;
use egaku_core::adjust::Adjustment;
use egaku_core::brush::BrushParams;
use egaku_core::color::Hsv;
use egaku_core::document::{Document, Kind, LayerId};
use egaku_core::editor::{Copied, Refusal};
use egaku_core::io::{self, Decoded, Format};
use egaku_core::object::TextObject;
use egaku_core::transform::{Affine, Source};

use crate::canvas::{CanvasTextures, View};
use crate::prefs::{Prefs, Tool};
use crate::theme;

/// What the pointer is currently doing on the canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    None,
    Stroke,
    Pan {
        last: Pos2,
    },
    Zoom {
        start: Pos2,
        start_zoom: f32,
    },
    Rotate {
        start_angle: f32,
        start_rot: f32,
    },
    /// Dragging the active layer (or selection) with the move tool.
    Move {
        start: Pos2,
    },
    /// Dragging a transform handle.
    Handle {
        handle: crate::handles::Handle,
        start: Pos2,
        start_delta: Affine,
    },
    Pick,
    /// Rectangle or ellipse selection (canvas coordinates).
    SelectBox {
        start: Pos2,
        end: Pos2,
    },
    Lasso,
    /// Dragging the selection outline itself with the select tool.
    MoveSelection {
        start: Pos2,
        end: Pos2,
    },
    Gradient {
        start: Pos2,
        end: Pos2,
    },
    Crop {
        handle: crate::handles::Handle,
        start: Pos2,
        start_rect: [f32; 4],
    },
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

/// Which export a dialog is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimFormat {
    Gif,
    Apng,
    Sheet,
    Sequence,
}

pub enum Modal {
    NewDoc,
    Unsaved(Pending),
    Error(String),
    Shortcuts,
    About,
    Export,
    ExportAnim(AnimFormat),
    ImageSize {
        w: u32,
        h: u32,
        lock: bool,
        smooth: bool,
    },
    CanvasSize {
        w: u32,
        h: u32,
        anchor: (i32, i32),
    },
    Adjust(Adjustment),
    Number {
        what: NumberOp,
        value: f32,
    },
    Rasterize(LayerId),
    Recover(Vec<crate::recovery::Entry>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberOp {
    Feather,
    Grow,
    Shrink,
}

impl NumberOp {
    pub fn title(self) -> &'static str {
        match self {
            NumberOp::Feather => "Feather selection",
            NumberOp::Grow => "Grow selection",
            NumberOp::Shrink => "Shrink selection",
        }
    }
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
        recovered: bool,
        result: Result<Document, String>,
    },
    Imported {
        name: String,
        at: Option<(f32, f32)>,
        result: Result<Decoded, String>,
    },
    Autosaved {
        state_id: u64,
        doc_gen: u64,
    },
}

/// Closed selection outlines in canvas coordinates.
pub type Contours = Arc<Vec<Vec<(f32, f32)>>>;

pub struct Toast {
    pub text: String,
    pub warn: bool,
    pub until: f64,
}

/// Text being typed on the canvas.
pub struct TextEditing {
    /// The layer, once something has been typed.
    pub layer: Option<LayerId>,
    pub obj: TextObject,
}

pub struct App {
    pub ed: Editor,
    pub path: Option<PathBuf>,
    pub name: String,
    pub view: View,
    pub fit_pending: bool,
    /// The next canvas size change keeps the view (undo/redo).
    pub keep_view: bool,
    pub textures: CanvasTextures,
    pub tex_size_gen: u64,
    pub prefs: Prefs,
    /// Foreground colour in HSV, kept separately so hue survives greys.
    pub fg_hsv: Hsv,
    pub hex_edit: Option<String>,
    pub gesture: Gesture,
    pub pen_pressure: Option<f32>,
    pub pen_eraser: bool,
    pub tablet: Option<crate::tablet::Tablet>,
    pub hover: Option<Pos2>,
    pub ui_hidden: bool,
    pub toast: Option<Toast>,
    pub modal: Option<Modal>,
    pub thumbs: HashMap<LayerId, (usize, egui::TextureHandle)>,
    pub renaming: Option<(LayerId, String)>,
    pub rename_fresh: bool,
    pub layer_drag: Option<LayerId>,
    /// Marching ants, cached per selection.
    pub sel_outline: Option<(usize, Contours)>,
    pub lasso: Vec<Pos2>,
    /// Crop rectangle in canvas pixels: x0, y0, x1, y1.
    pub crop: Option<[f32; 4]>,
    /// Free transform (Ctrl+T) is showing handles until committed.
    pub free_transform: bool,
    pub text_edit: Option<TextEditing>,
    /// Playback start time and frame.
    pub playing: Option<(f64, usize)>,
    pub clipboard: Option<Copied>,
    /// The system clipboard, opened on first paste.
    pub system_clipboard: Option<arboard::Clipboard>,
    /// Selection mode chosen in the tool options; modifier keys override it.
    pub select_mode: egaku_core::selection::SelectOp,
    /// The mode in effect for the current selection drag.
    pub press_op: egaku_core::selection::SelectOp,
    pub fonts: Option<Vec<String>>,
    pub font_filter: String,
    /// Stroke thumbnails of brushes, keyed by name, with the settings
    /// they were drawn for.
    pub preset_previews: HashMap<String, (egaku_core::brush::BrushParams, egui::TextureHandle)>,
    pub pad: Option<crate::brush_editor::Pad>,
    io_tx: Sender<IoDone>,
    io_rx: Receiver<IoDone>,
    pub io_busy: Option<String>,
    quit_after_save: bool,
    allow_close: bool,
    doc_gen: u64,
    last_title: String,
    autosaved_id: u64,
    last_autosave: f64,
    autosave_busy: bool,
    pub recovery: crate::recovery::Recovery,
}

const PREFS_KEY: &str = "egaku-prefs";

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        egaku_core::object::warm_up_fonts();
        let prefs: Prefs = cc
            .storage
            .and_then(|s| eframe::get_value(s, PREFS_KEY))
            .unwrap_or_default();
        prefs.register_tips();
        let doc = Document::new(prefs.new_width, prefs.new_height, prefs.new_paper);
        let (io_tx, io_rx) = channel();
        let tablet = crate::tablet::Tablet::new(cc);
        let mut app = App {
            ed: Editor::new(doc),
            path: None,
            name: "Untitled".into(),
            view: View::default(),
            fit_pending: true,
            keep_view: false,
            textures: CanvasTextures::default(),
            tex_size_gen: 0,
            fg_hsv: crate::color_picker::to_hsv(prefs.fg),
            prefs,
            hex_edit: None,
            gesture: Gesture::None,
            pen_pressure: None,
            pen_eraser: false,
            tablet,
            hover: None,
            ui_hidden: false,
            toast: None,
            modal: None,
            thumbs: HashMap::new(),
            renaming: None,
            rename_fresh: false,
            layer_drag: None,
            sel_outline: None,
            lasso: vec![],
            crop: None,
            free_transform: false,
            text_edit: None,
            playing: None,
            clipboard: None,
            system_clipboard: None,
            select_mode: Default::default(),
            press_op: Default::default(),
            fonts: None,
            font_filter: String::new(),
            preset_previews: HashMap::new(),
            pad: None,
            io_tx,
            io_rx,
            io_busy: None,
            quit_after_save: false,
            allow_close: false,
            doc_gen: 0,
            last_title: String::new(),
            autosaved_id: 0,
            last_autosave: 0.0,
            autosave_busy: false,
            recovery: crate::recovery::Recovery::new(),
        };
        let found = app.recovery.leftovers();
        if !found.is_empty() {
            app.modal = Some(Modal::Recover(found));
        }
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

    /// Sets the blend mode of layers as one undo step. Pass Through only
    /// applies to groups; other layers get Normal.
    pub fn set_blend(&mut self, ids: &[LayerId], blend: egaku_core::blend::BlendMode) {
        use egaku_core::blend::BlendMode;
        self.ed.set_layers(ids, "Blend mode", |l| {
            l.blend = if blend == BlendMode::PassThrough && !l.is_group() {
                BlendMode::Normal
            } else {
                blend
            };
        });
    }

    /// Steps the selected layers' blend mode through the menu order.
    pub fn cycle_blend(&mut self, ctx: &egui::Context, step: i32) {
        use egaku_core::blend::BlendMode;
        let l = self.ed.doc.active_layer();
        let group = l.is_group();
        let modes: Vec<BlendMode> = BlendMode::ALL
            .into_iter()
            .filter(|m| group || *m != BlendMode::PassThrough)
            .collect();
        let i = modes.iter().position(|m| *m == l.blend).unwrap_or(0) as i32;
        let next = modes[(i + step).rem_euclid(modes.len() as i32) as usize];
        let ids = self.ed.selected_layers();
        self.set_blend(&ids, next);
        self.toast(ctx, format!("Blend mode: {}", next.name()), false);
    }

    pub fn set_fg(&mut self, c: [u8; 3]) {
        self.prefs.fg = c;
        self.fg_hsv = crate::color_picker::to_hsv(c);
        self.hex_edit = None;
        self.recolor_text();
    }

    pub fn swap_colors(&mut self) {
        let (f, b) = (self.prefs.fg, self.prefs.bg);
        self.prefs.bg = f;
        self.set_fg(b);
    }

    /// While editing text, the main colour is the text colour.
    pub fn recolor_text(&mut self) {
        if let Some(te) = &mut self.text_edit
            && te.obj.color != self.prefs.fg
        {
            te.obj.color = self.prefs.fg;
            if let Some(id) = te.layer {
                let obj = te.obj.clone();
                self.ed.set_text(id, obj);
            }
        }
    }

    pub fn set_tool(&mut self, t: Tool) {
        if self.prefs.tool == t {
            return;
        }
        self.finish_modes();
        self.prefs.tool = t;
    }

    /// Commits whatever modal canvas interaction is in progress.
    pub fn finish_modes(&mut self) {
        if self.free_transform {
            self.ed.commit_transform();
            self.free_transform = false;
        }
        if self.text_edit.take().is_some() {
            self.ed.end_gesture();
        }
        self.crop = None;
    }

    /// Steps back in history. The view stays where it is, even when the
    /// step changes the canvas size.
    pub fn undo(&mut self) {
        self.cancel_modes();
        let g = self.ed.size_gen;
        self.ed.undo();
        if self.ed.size_gen != g {
            self.keep_view = true;
        }
    }

    pub fn redo(&mut self) {
        self.cancel_modes();
        let g = self.ed.size_gen;
        self.ed.redo();
        if self.ed.size_gen != g {
            self.keep_view = true;
        }
    }

    /// Abandons modal interactions.
    pub fn cancel_modes(&mut self) {
        self.ed.cancel_stroke();
        self.ed.cancel_transform();
        self.free_transform = false;
        self.crop = None;
        self.lasso.clear();
        if self.text_edit.take().is_some() {
            self.ed.end_gesture();
        }
        self.gesture = Gesture::None;
    }

    pub fn refusal(&mut self, ctx: &egui::Context, r: Refusal) {
        let msg = match r {
            Refusal::LayerHidden => "This layer is hidden — show it to edit it".to_string(),
            Refusal::LayerEmpty => "Nothing to move on this layer".to_string(),
            Refusal::NotPaintable(Kind::Group) => {
                "Choose a layer inside the group to paint on".to_string()
            }
            Refusal::NotPaintable(_) => {
                let id = self.ed.doc.active_id();
                self.modal = Some(Modal::Rasterize(id));
                return;
            }
            Refusal::NoPixels => {
                "No pixels to erase in the selection here — deselect (Ctrl+D) to delete the layers"
                    .to_string()
            }
            Refusal::Busy => return,
        };
        self.toast(ctx, msg, true);
    }

    pub fn start_free_transform(&mut self, ctx: &egui::Context) {
        if self.free_transform {
            return;
        }
        if self.text_edit.take().is_some() {
            self.ed.end_gesture();
        }
        match self.ed.begin_transform(self.prefs.smooth_transform) {
            Ok(()) => {
                self.free_transform = true;
                if self.prefs.tool != Tool::Move {
                    self.prefs.tool = Tool::Move;
                }
            }
            Err(r) => self.refusal(ctx, r),
        }
    }

    pub fn start_adjust(&mut self, ctx: &egui::Context, a: Adjustment) {
        self.finish_modes();
        if !a.has_params() {
            if let Err(r) = self.ed.apply_filter(a.name(), Arc::new(a)) {
                self.refusal(ctx, r);
            }
            return;
        }
        match self.ed.begin_filter(Arc::new(a)) {
            Ok(()) => self.modal = Some(Modal::Adjust(a)),
            Err(r) => self.refusal(ctx, r),
        }
    }

    pub fn copy(&mut self, ctx: &egui::Context, merged: bool, cut: bool) {
        let c = if cut {
            self.ed.cut()
        } else {
            self.ed.copy(merged)
        };
        match c {
            Some(c) => {
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [c.width as usize, c.height as usize],
                    &c.rgba,
                );
                ctx.copy_image(img);
                self.clipboard = Some(c);
            }
            None => self.toast(ctx, "Nothing to copy here", true),
        }
    }

    /// Pastes the app's own clipboard in place; returns false if the system
    /// clipboard should be used instead.
    pub fn paste_internal(&mut self) -> bool {
        match self.clipboard.take() {
            Some(c) => {
                self.ed.paste("Pasted", &c);
                self.clipboard = Some(c);
                true
            }
            None => false,
        }
    }

    pub fn set_quit_after_save(&mut self) {
        self.quit_after_save = true;
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
            if !matches!(done, IoDone::Autosaved { .. }) {
                self.io_busy = None;
            }
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
                            self.recovery.clear();
                        }
                        self.prefs.remember_file(path.clone());
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
                IoDone::Opened {
                    path,
                    recovered,
                    result,
                } => match result {
                    Ok(doc) => {
                        if recovered {
                            let (orig, name) = self.recovery.meta_of(&path);
                            self.set_document(doc, orig, format!("{name} (recovered)"));
                            // Make sure it reads as unsaved.
                            let id = self.ed.doc.active_id();
                            self.ed.set_layer(id, "Recover", |_| {});
                            let _ = std::fs::remove_file(&path);
                            self.recovery.remove_meta(&path);
                        } else {
                            let native = is_ora(&path);
                            self.prefs.remember_file(path.clone());
                            self.set_document(doc, native.then(|| path.clone()), file_name(&path));
                        }
                    }
                    Err(e) => {
                        self.modal = Some(Modal::Error(format!(
                            "Could not open {}:\n{e}",
                            path.display()
                        )))
                    }
                },
                IoDone::Imported { name, at, result } => match result {
                    Ok(d) => {
                        if d.converted {
                            self.toast(ctx, "Converted the image's colour profile to sRGB", false);
                        }
                        let src = Source::from_rgba(d.width, d.height, &d.rgba);
                        self.ed.place_image(&name, src, at);
                        self.set_tool(Tool::Move);
                    }
                    Err(e) => {
                        self.modal = Some(Modal::Error(format!("Could not import image:\n{e}")))
                    }
                },
                IoDone::Autosaved { state_id, doc_gen } => {
                    self.autosave_busy = false;
                    if doc_gen == self.doc_gen {
                        self.autosaved_id = state_id;
                    }
                }
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
        self.sel_outline = None;
        self.crop = None;
        self.free_transform = false;
        self.text_edit = None;
        self.playing = None;
        self.autosaved_id = 0;
        self.recovery.clear();
        if self.ed.doc.state.frames > 1 {
            self.prefs.show_timeline = true;
        }
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
                self.recovery.clear();
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

    /// Asks for an image and makes it the current brush's tip.
    pub fn load_brush_tip(&mut self, ctx: &egui::Context) {
        let Some(path) = self
            .dialog()
            .set_title("Brush tip image")
            .add_filter("Images", io::IMAGE_EXTENSIONS)
            .pick_file()
        else {
            return;
        };
        self.remember_dir(&path);
        match io::decode(&path) {
            Ok(d) => self.use_brush_tip(ctx, d.width, d.height, &d.rgba),
            Err(e) => self.toast(ctx, format!("Couldn't open that image: {e}"), true),
        }
    }

    /// Makes the selected part of the canvas (as seen) the brush's tip.
    pub fn brush_tip_from_selection(&mut self, ctx: &egui::Context) {
        if self.ed.doc.state.selection.is_none() {
            self.toast(ctx, "Select the shape to use as a tip first", true);
            return;
        }
        if let Some(c) = self.ed.copy(true) {
            self.use_brush_tip(ctx, c.width, c.height, &c.rgba);
        }
    }

    fn use_brush_tip(&mut self, ctx: &egui::Context, w: u32, h: u32, rgba: &[u8]) {
        use egaku_core::brush::TipShape;
        use egaku_core::tip::TipImage;
        let Some(tip) = TipImage::from_rgba(w, h, rgba) else {
            self.toast(ctx, "That image is empty, so it can't be a tip", true);
            return;
        };
        let (tw, th) = (tip.width(), tip.height());
        let saved = crate::prefs::SavedTip::new(&tip);
        let id = egaku_core::tip::register(tip);
        if !self.prefs.brush_tips.iter().any(|t| t.id == id) {
            self.prefs.brush_tips.push(saved);
        }
        let b = self.current_brush_mut();
        b.tip = TipShape::Image(id);
        // Keep the tip's own proportions and turn.
        b.roundness = 1.0;
        b.angle = 0.0;
        if let Some(pad) = &mut self.pad {
            pad.clear();
        }
        self.toast(ctx, format!("Brush tip: {tw} × {th} px"), false);
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
            recovered: false,
        });
    }

    pub fn open_recovered(&mut self, ctx: &egui::Context, path: PathBuf) {
        let p = path.clone();
        self.spawn(ctx, "Recovering…", move || IoDone::Opened {
            result: io::open(&p).map_err(|e| e.to_string()),
            path: p,
            recovered: true,
        });
    }

    /// Places an image file as a transformable image layer.
    pub fn import_image(
        &mut self,
        ctx: &egui::Context,
        path: Option<PathBuf>,
        at: Option<(f32, f32)>,
    ) {
        let Some(path) = path.or_else(|| {
            self.dialog()
                .add_filter("Images", io::IMAGE_EXTENSIONS)
                .pick_file()
        }) else {
            return;
        };
        self.remember_dir(&path);
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Image")
            .to_string();
        self.spawn(ctx, "Importing…", move || IoDone::Imported {
            name,
            at,
            result: io::decode(&path).map_err(|e| e.to_string()),
        });
    }

    /// Places straight-alpha pixels as a new image layer centred at `at`
    /// (the canvas centre if `None`).
    fn place_pasted(&mut self, w: u32, h: u32, rgba: &[u8], at: Option<(f32, f32)>) {
        let src = Source::from_rgba(w, h, rgba);
        self.ed.place_image("Pasted image", src, at);
        self.set_tool(Tool::Move);
    }

    /// The image on the system clipboard, as straight-alpha RGBA.
    fn system_image(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        if self.system_clipboard.is_none() {
            self.system_clipboard = arboard::Clipboard::new().ok();
        }
        let img = self.system_clipboard.as_mut()?.get_image().ok()?;
        let (w, h) = (img.width as u32, img.height as u32);
        (w > 0 && h > 0 && img.bytes.len() == w as usize * h as usize * 4)
            .then(|| (w, h, img.bytes.into_owned()))
    }

    /// Paste (Ctrl+V). The app's own copy pastes in place as pixels, as long
    /// as the system clipboard still holds it; an image copied elsewhere
    /// since is placed as a new image layer.
    pub fn paste(&mut self, ctx: &egui::Context) {
        let sys = self.system_image();
        let ours = match (&sys, &self.clipboard) {
            (Some((w, h, px)), Some(c)) => same_image(c, *w, *h, px),
            (None, Some(_)) => true,
            _ => false,
        };
        if ours {
            self.paste_internal();
        } else if let Some((w, h, px)) = sys {
            self.place_pasted(w, h, &px, None);
        } else {
            self.toast(ctx, "The clipboard has no image", true);
        }
    }

    /// Paste as New Image (Ctrl+Shift+V): whatever the clipboard holds
    /// becomes an image layer, which can be scaled without loss.
    pub fn paste_as_image(&mut self, ctx: &egui::Context) {
        let sys = self.system_image();
        let ours = match (&sys, &self.clipboard) {
            (Some((w, h, px)), Some(c)) => same_image(c, *w, *h, px),
            (None, Some(_)) => true,
            _ => false,
        };
        if ours && let Some(c) = self.clipboard.take() {
            // Our own copy keeps its place on the canvas.
            let at = (
                c.x as f32 + c.width as f32 * 0.5,
                c.y as f32 + c.height as f32 * 0.5,
            );
            self.place_pasted(c.width, c.height, &c.rgba, Some(at));
            self.clipboard = Some(c);
        } else if let Some((w, h, px)) = sys {
            self.place_pasted(w, h, &px, None);
        } else {
            self.toast(ctx, "The clipboard has no image", true);
        }
    }

    pub fn save(&mut self, ctx: &egui::Context) {
        match self.path.clone() {
            Some(p) if is_ora(&p) => self.save_to(ctx, p),
            _ => self.save_as(ctx),
        }
    }

    fn stem(&self) -> String {
        let n = self.name.trim_end_matches(" (recovered)");
        n.rsplit_once('.').map_or(n, |(s, _)| s).to_string()
    }

    pub fn save_as(&mut self, ctx: &egui::Context) {
        let Some(mut p) = self
            .dialog()
            .add_filter("OpenRaster", &["ora"])
            .set_file_name(format!("{}.ora", self.stem()))
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
        self.finish_modes();
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

    /// Exports the flattened image in the chosen format.
    pub fn export(&mut self, ctx: &egui::Context, format: Format, scale: u32) {
        let ext = format.extension();
        let Some(mut p) = self
            .dialog()
            .add_filter(ext.to_uppercase(), &[ext])
            .set_file_name(format!("{}.{ext}", self.stem()))
            .save_file()
        else {
            return;
        };
        if p.extension().is_none() {
            p.set_extension(ext);
        }
        self.remember_dir(&p);
        self.prefs.export_format = format;
        self.prefs.export_scale = scale;
        self.finish_modes();
        let doc = self.ed.doc.clone();
        self.spawn(ctx, "Exporting…", move || IoDone::Exported {
            result: io::export(&doc, &p, format, scale).map_err(|e| e.to_string()),
            path: p,
        });
    }

    pub fn export_animation(
        &mut self,
        ctx: &egui::Context,
        fmt: AnimFormat,
        scale: u32,
        columns: u32,
    ) {
        let (ext, filter) = match fmt {
            AnimFormat::Gif => ("gif", "GIF"),
            AnimFormat::Apng => ("png", "Animated PNG"),
            AnimFormat::Sheet => ("png", "PNG"),
            AnimFormat::Sequence => ("png", "PNG sequence"),
        };
        let suffix = match fmt {
            AnimFormat::Sheet => "-sheet",
            AnimFormat::Sequence => "-0001",
            _ => "",
        };
        let Some(mut p) = self
            .dialog()
            .add_filter(filter, &[ext])
            .set_file_name(format!("{}{suffix}.{ext}", self.stem()))
            .save_file()
        else {
            return;
        };
        if p.extension().is_none() {
            p.set_extension(ext);
        }
        self.remember_dir(&p);
        self.finish_modes();
        let n = self.ed.doc.state.frames;
        let (w, h) = (self.ed.doc.width, self.ed.doc.height);
        let fps = self.ed.doc.state.fps;
        // Frames render from a snapshot on the worker thread.
        let ed_doc = self.ed.doc.clone();
        self.spawn(ctx, "Exporting animation…", move || {
            let ed = Editor::new(ed_doc);
            let frames: Vec<Vec<u8>> = (0..n)
                .map(|f| io::upscale(w, h, &ed.render_frame(f), scale).2)
                .collect();
            let (w, h) = (w * scale, h * scale);
            let result = match fmt {
                AnimFormat::Gif => io::export_gif(&p, w, h, &frames, fps),
                AnimFormat::Apng => io::export_apng(&p, w, h, &frames, fps),
                AnimFormat::Sheet => {
                    let (sw, sh, sheet) = io::sprite_sheet(w, h, &frames, columns);
                    io::encode(sw, sh, &sheet, Format::Png).and_then(|b| io::write_atomic(&p, &b))
                }
                AnimFormat::Sequence => {
                    let stem = p
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("frame")
                        .trim_end_matches("-0001")
                        .to_string();
                    let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
                    frames.iter().enumerate().try_for_each(|(i, f)| {
                        let fp = dir.join(format!("{stem}-{:04}.png", i + 1));
                        io::encode(w, h, f, Format::Png).and_then(|b| io::write_atomic(&fp, &b))
                    })
                }
            };
            IoDone::Exported {
                result: result.map_err(|e| e.to_string()),
                path: p,
            }
        });
    }

    pub fn handle_dropped_and_pasted(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            let p = f.path().to_path_buf();
            if p.as_os_str().is_empty() {
                continue;
            }
            // A layered file replaces the document; a flat image joins it
            // as a new image layer, which is almost always the intent.
            if is_ora(&p) {
                self.request(ctx, Pending::Open(Some(p)));
            } else {
                self.import_image(ctx, Some(p), None);
            }
        }
    }

    pub fn handle_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) {
            if !self.allow_close && self.ed.is_modified() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.modal = Some(Modal::Unsaved(Pending::Quit));
            } else {
                self.recovery.clear();
            }
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

    /// Saves a recovery copy in the background every minute or so while
    /// there are unsaved changes.
    pub fn autosave(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let id = self.ed.state_id();
        if !self.ed.is_modified()
            || id == self.autosaved_id
            || self.autosave_busy
            || self.ed.is_busy()
        {
            return;
        }
        if now - self.last_autosave < 60.0 {
            ctx.request_repaint_after(std::time::Duration::from_secs(61));
            return;
        }
        let Some(target) = self.recovery.target(&self.name, self.path.as_deref()) else {
            return;
        };
        self.last_autosave = now;
        self.autosave_busy = true;
        let doc = self.ed.doc.clone();
        let doc_gen = self.doc_gen;
        let tx = self.io_tx.clone();
        std::thread::spawn(move || {
            let _ = io::ora::save(&doc, &target);
            let _ = tx.send(IoDone::Autosaved {
                state_id: id,
                doc_gen,
            });
        });
    }

    pub fn store(&self, storage: &mut dyn eframe::Storage) {
        // Only keep the tip images some brush still uses.
        let mut prefs = self.prefs.clone();
        let used = prefs.tips_in_use();
        prefs.brush_tips.retain(|t| used.contains(&t.id));
        eframe::set_value(storage, PREFS_KEY, &prefs);
    }

    /// The topmost visible pixel-bearing layer at a canvas point.
    pub fn layer_at(&self, x: f32, y: f32) -> Option<LayerId> {
        if x < 0.0 || y < 0.0 {
            return None;
        }
        fn go(ls: &[egaku_core::Layer], x: i64, y: i64) -> Option<LayerId> {
            for l in ls.iter().rev() {
                if !l.is_shown() {
                    continue;
                }
                if l.is_group() {
                    if let Some(id) = go(l.children(), x, y) {
                        return Some(id);
                    }
                } else if l.tiles.pixel(x, y)[3] > 24 {
                    return Some(l.id);
                }
            }
            None
        }
        go(self.ed.doc.layers(), x as i64, y as i64)
    }
}

pub fn is_ora(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ora"))
}

pub fn file_name(p: &Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled")
        .to_string()
}

/// Consumes a key press whose own modifiers match `m` exactly. Each event's
/// modifiers are checked rather than the frame's, since a quick chord can be
/// pressed and released within a single frame.
pub fn consume_exact(i: &mut egui::InputState, m: egui::Modifiers, k: egui::Key) -> bool {
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

/// Whether the system clipboard's image is the one this app copied. The
/// round trip through the platform can nudge colours (premultiplication,
/// colour management) and drop alpha, so this compares the size and the
/// colour of opaque pixels, loosely.
fn same_image(c: &Copied, w: u32, h: u32, px: &[u8]) -> bool {
    if c.width != w || c.height != h || c.rgba.len() != px.len() {
        return false;
    }
    let n = (w as usize) * (h as usize);
    let step = (n / 65_536).max(1);
    let (mut diff, mut count) = (0u64, 0u64);
    for i in (0..n).step_by(step) {
        let (a, b) = (&c.rgba[i * 4..i * 4 + 4], &px[i * 4..i * 4 + 4]);
        if a[3] < 250 {
            continue;
        }
        for ch in 0..3 {
            diff += (a[ch] as i32 - b[ch] as i32).unsigned_abs() as u64;
        }
        count += 3;
    }
    count == 0 || diff <= count * 6
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_own_copy_after_a_lossy_round_trip() {
        let rgba: Vec<u8> = (0..16 * 8 * 4)
            .map(|i| if i % 4 == 3 { 255 } else { (i * 7 % 256) as u8 })
            .collect();
        let c = Copied {
            x: 3,
            y: 4,
            width: 16,
            height: 8,
            rgba: rgba.clone(),
        };
        assert!(same_image(&c, 16, 8, &rgba));
        // Off by one or two, as premultiplication or colour management may do.
        let nudged: Vec<u8> = rgba
            .iter()
            .enumerate()
            .map(|(i, v)| if i % 4 == 3 { *v } else { v.saturating_add(2) })
            .collect();
        assert!(same_image(&c, 16, 8, &nudged));
        // Another image of the same size, and one of another size.
        let other: Vec<u8> = rgba.iter().map(|v| v.wrapping_add(90)).collect();
        assert!(!same_image(&c, 16, 8, &other));
        assert!(!same_image(&c, 8, 16, &rgba));
    }
}
