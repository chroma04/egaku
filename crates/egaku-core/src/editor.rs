//! The editing API: every user-visible operation, with undo and display
//! invalidation handled in one place. The GUI is a thin shell around this,
//! and everything here runs (and is tested) headless.
//!
//! Long-running interactions (a stroke, a transform, a filter preview) are
//! shown through the compositor's override hook while in progress and only
//! touch the document when committed, so each is one cheap undo step and
//! cancelling is free.

use std::sync::Arc;

use crate::adjust::Filter;
use crate::blend::{BlendMode, blend_tile};
use crate::brush::{
    BrushParams, Dab, Mask, MaskTile, Paint, PaintMode, StrokeEngine, apply_mask_tile_sel,
};
use crate::document::{Content, DocState, Document, Kind, Layer, LayerId, Place};
use crate::fill::{FillParams, flood_fill};
use crate::history::{Changes, History, diff_states, snapshot_cost};
use crate::object::{ImageObject, TextObject};
use crate::render::{NoOverride, Ov, Projection, compose_list, layer_content};
use crate::selection::{SelectOp, Selection};
use crate::tile::{TILE, TILE_PX, Tile, TileCoord, TileGrid, TileRect};
use crate::transform::{Affine, Source};

struct ActiveStroke {
    engine: StrokeEngine,
    mask: Mask,
    paint: Paint,
    layer: LayerId,
    sel: Option<Arc<Selection>>,
}

/// A free transform (or move) in progress.
pub struct Transforming {
    pub layer: LayerId,
    pub kind: Kind,
    /// What is being transformed, and how it maps to the canvas at the start.
    src: Arc<Source>,
    src_m: Affine,
    /// The handle frame: box-space → canvas at the start, and the box size.
    pub frame: Affine,
    pub size: (f32, f32),
    /// The user's transform, applied on top (canvas → canvas).
    pub delta: Affine,
    pub smooth: bool,
    /// Raster layers: what stays behind when a selection is floated.
    base: Option<Arc<TileGrid>>,
    sel: Option<Arc<Selection>>,
}

impl Transforming {
    fn matrix(&self) -> Affine {
        self.delta.compose(self.src_m)
    }

    /// Corners of the handle frame in canvas coordinates, clockwise from the
    /// top-left.
    pub fn quad(&self) -> [(f32, f32); 4] {
        let m = self.delta.compose(self.frame);
        let (w, h) = self.size;
        [
            m.apply(0.0, 0.0),
            m.apply(w, 0.0),
            m.apply(w, h),
            m.apply(0.0, h),
        ]
    }

    fn dest_rect(&self, cols: u32, rows: u32) -> TileRect {
        let r = self.src.dest_tiles(&self.matrix(), cols, rows);
        match &self.base {
            Some(b) => b.bounds().map_or(r, |br| r.union(br)),
            None => r,
        }
    }
}

struct Filtering {
    layer: LayerId,
    filter: Arc<dyn Filter>,
    sel: Option<Arc<Selection>>,
    rect: TileRect,
}

/// Onion skin settings for animation.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Onion {
    pub before: usize,
    pub after: usize,
    /// Opacity of the nearest frame; further ones fade.
    pub opacity: f32,
}

impl Default for Onion {
    fn default() -> Self {
        Onion {
            before: 1,
            after: 1,
            opacity: 0.35,
        }
    }
}

/// Why an edit could not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    LayerHidden,
    LayerEmpty,
    Busy,
    /// The active layer is a group, text or image; painting needs pixels.
    NotPaintable(Kind),
}

/// Pixels copied out of the document: straight RGBA at a canvas position.
pub struct Copied {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct Editor {
    pub doc: Document,
    pub history: History,
    pub projection: Projection,
    /// Bumped when the canvas size changes (the GPU cache must be rebuilt).
    pub size_gen: u64,
    pub onion: Option<Onion>,
    state_id: u64,
    next_state_id: u64,
    saved_id: u64,
    stroke: Option<ActiveStroke>,
    transform: Option<Transforming>,
    filter: Option<Filtering>,
    coalesce: Option<String>,
    last_point: Option<(f32, f32)>,
    dabs: Vec<Dab>,
    dirty: Vec<TileCoord>,
}

/// The override the editor hands to the compositor.
struct EdOv<'a> {
    stroke: Option<&'a ActiveStroke>,
    transform: Option<(&'a Transforming, Affine, Affine)>,
    filter: Option<&'a Filtering>,
    alpha_lock: bool,
    onion: Vec<(LayerId, Arc<TileGrid>, [u8; 3], u8)>,
}

impl Ov for EdOv<'_> {
    fn layer(&self) -> Option<LayerId> {
        self.stroke
            .map(|s| s.layer)
            .or(self.transform.map(|t| t.0.layer))
            .or(self.filter.map(|f| f.layer))
            .or(self.onion.first().map(|o| o.0))
    }

    fn tile(&self, l: &Layer, c: TileCoord) -> Option<Option<Tile>> {
        if let Some(s) = self.stroke
            && s.layer == l.id
        {
            let m = s.mask.get(c)?;
            let sel_t = match &s.sel {
                Some(sel) => Some(sel.tile(c)?),
                None => None,
            };
            if (s.paint.mode == PaintMode::Erase || s.paint.alpha_lock) && l.tiles.get(c).is_none()
            {
                return None;
            }
            let mut t = l
                .tiles
                .get(c)
                .map_or_else(|| Tile::filled([0; 4]), |t| (**t).clone());
            apply_mask_tile_sel(&mut t, m, sel_t, &s.paint);
            return Some(Some(t));
        }
        if let Some((t, m, inv)) = &self.transform
            && t.layer == l.id
        {
            let mut out = t
                .base
                .as_ref()
                .and_then(|b| b.get(c))
                .map(|x| (**x).clone());
            let r = if !t.smooth && m.is_translation() && t.src_m == Affine::IDENTITY {
                let (dx, dy) = (m.m[4].round() as i64, m.m[5].round() as i64);
                t.src.base.shifted_tile(c, dx, dy)
            } else {
                t.src.render_tile(m, inv, t.smooth, c)
            };
            if let Some(r) = r {
                match &mut out {
                    Some(o) => blend_tile(o, &r, 255, BlendMode::Normal),
                    None => out = Some(r),
                }
            }
            return Some(out);
        }
        if let Some(f) = self.filter
            && f.layer == l.id
        {
            if !f.rect.contains(c) {
                return None;
            }
            let new = f.filter.apply(&l.tiles, c);
            return Some(restrict(
                l.tiles.get(c).map(|t| &**t),
                new,
                f.sel.as_deref(),
                c,
                self.alpha_lock,
            ));
        }
        None
    }

    fn under(&self, l: &Layer, c: TileCoord, out: &mut Tile) {
        for (id, cel, tint, op) in &self.onion {
            if *id != l.id {
                continue;
            }
            let Some(src) = cel.get(c) else { continue };
            let mut t = (**src).clone();
            for p in t.px.iter_mut() {
                let a = p[3] as u32;
                *p = [
                    crate::blend::div255(tint[0] as u32 * a) as u8,
                    crate::blend::div255(tint[1] as u32 * a) as u8,
                    crate::blend::div255(tint[2] as u32 * a) as u8,
                    p[3],
                ];
            }
            blend_tile(out, &t, *op, BlendMode::Normal);
        }
    }
}

/// Mixes filtered pixels with the originals by selection coverage, and
/// keeps the original alpha when the layer's alpha is locked.
fn restrict(
    orig: Option<&Tile>,
    new: Option<Tile>,
    sel: Option<&Selection>,
    c: TileCoord,
    alpha_lock: bool,
) -> Option<Tile> {
    let st = match sel {
        Some(s) => match s.tile(c) {
            None => return orig.cloned(),
            Some(t) => Some(t),
        },
        None => None,
    };
    let empty = Tile::filled([0; 4]);
    let o = orig.unwrap_or(&empty);
    let mut n = new.unwrap_or_else(|| empty.clone());
    for i in 0..TILE_PX {
        if let Some(st) = st {
            let s = st.v[i] as u32;
            if s != 255 {
                for ch in 0..4 {
                    n.px[i][ch] = crate::blend::div255(
                        n.px[i][ch] as u32 * s + o.px[i][ch] as u32 * (255 - s),
                    ) as u8;
                }
            }
        }
        if alpha_lock {
            let (na, oa) = (n.px[i][3] as u32, o.px[i][3] as u32);
            if na == 0 {
                n.px[i] = [0; 4];
            } else if na != oa {
                for ch in 0..3 {
                    n.px[i][ch] = ((n.px[i][ch] as u32 * oa + na / 2) / na).min(oa) as u8;
                }
                n.px[i][3] = oa as u8;
            }
        }
    }
    if n.is_empty() { None } else { Some(n) }
}

/// Exact pixel bounds of a grid's content.
pub fn content_bounds(g: &TileGrid) -> Option<(u32, u32, u32, u32)> {
    let s = Selection::from_alpha(g.cols() * TILE as u32, g.rows() * TILE as u32, g);
    s.bounds()
}

impl Editor {
    pub fn new(doc: Document) -> Self {
        let projection = Projection::new(doc.width, doc.height);
        Editor {
            doc,
            history: History::default(),
            projection,
            size_gen: 0,
            onion: None,
            state_id: 0,
            next_state_id: 1,
            saved_id: 0,
            stroke: None,
            transform: None,
            filter: None,
            coalesce: None,
            last_point: None,
            dabs: vec![],
            dirty: vec![],
        }
    }

    pub fn is_modified(&self) -> bool {
        self.state_id != self.saved_id
    }

    pub fn mark_saved(&mut self) {
        self.saved_id = self.state_id;
    }

    /// Identifies the current document state; pass it to
    /// [`mark_saved_at`](Self::mark_saved_at) when a background save finishes.
    pub fn state_id(&self) -> u64 {
        self.state_id
    }

    pub fn mark_saved_at(&mut self, id: u64) {
        self.saved_id = id;
    }

    pub fn is_busy(&self) -> bool {
        self.stroke.is_some() || self.transform.is_some() || self.filter.is_some()
    }

    fn onion_cels(&self) -> Vec<(LayerId, Arc<TileGrid>, [u8; 3], u8)> {
        let Some(o) = self.onion else { return vec![] };
        let s = &self.doc.state;
        if s.frames < 2 {
            return vec![];
        }
        let l = self.doc.active_layer();
        if !l.is_paintable() {
            return vec![];
        }
        let mut out = vec![];
        let fade = |k: usize| (o.opacity * 0.6f32.powi(k as i32 - 1) * 255.0) as u8;
        for k in 1..=o.before {
            if let Some(f) = s.frame.checked_sub(k)
                && let Some(c) = self.doc.cel(l, f)
            {
                out.push((l.id, c, [230, 60, 60], fade(k)));
            }
        }
        for k in 1..=o.after {
            let f = s.frame + k;
            if f < s.frames
                && let Some(c) = self.doc.cel(l, f)
            {
                out.push((l.id, c, [40, 170, 90], fade(k)));
            }
        }
        out
    }

    /// Recomposites the display. `visible` is in level-0 tile coordinates.
    /// Returns true if more work is pending.
    pub fn update_projection(&mut self, visible: Option<TileRect>, budget: usize) -> bool {
        let ov = EdOv {
            stroke: self.stroke.as_ref(),
            transform: self.transform.as_ref().and_then(|t| {
                let m = t.matrix();
                m.inverse().map(|inv| (t, m, inv))
            }),
            filter: self.filter.as_ref(),
            alpha_lock: self
                .filter
                .as_ref()
                .and_then(|f| self.doc.find(f.layer))
                .is_some_and(|l| l.alpha_lock),
            onion: self.onion_cels(),
        };
        self.projection.update(&self.doc, &ov, visible, budget)
    }

    /// Applies `f` as one undoable step.
    pub fn commit(&mut self, label: &str, f: impl FnOnce(&mut Document)) {
        self.coalesce = None;
        self.commit_inner(label, f);
    }

    fn commit_inner(&mut self, label: &str, f: impl FnOnce(&mut Document)) {
        let before = self.doc.state.clone();
        f(&mut self.doc);
        self.fix_active();
        let cost = snapshot_cost(&before, &self.doc.state);
        self.invalidate_between(&before);
        self.history.push(before, label, self.state_id, cost);
        self.state_id = self.next_state_id;
        self.next_state_id += 1;
    }

    /// Like [`commit`](Self::commit), but successive calls with the same label
    /// merge into one undo step until [`end_gesture`](Self::end_gesture). Used
    /// for slider drags.
    pub fn commit_coalesced(&mut self, label: &str, f: impl FnOnce(&mut Document)) {
        if self.coalesce.as_deref() == Some(label) {
            let before = self.doc.state.clone();
            f(&mut self.doc);
            self.fix_active();
            self.invalidate_between(&before);
            self.state_id = self.next_state_id;
            self.next_state_id += 1;
        } else {
            self.commit_inner(label, f);
            self.coalesce = Some(label.to_string());
        }
    }

    pub fn end_gesture(&mut self) {
        self.coalesce = None;
    }

    /// Keeps the active layer pointing at something that exists.
    fn fix_active(&mut self) {
        if self.doc.find(self.doc.state.active).is_some() {
            return;
        }
        if self.doc.state.layers.is_empty() {
            let l = self.doc.new_layer("Layer 1");
            self.doc.state.layers.push(l);
        }
        self.doc.state.active = self.doc.state.layers.last().unwrap().id;
    }

    fn invalidate_between(&mut self, other: &DocState) {
        if self.doc.state.size != other.size
            || self.projection.size() != (self.doc.width, self.doc.height)
        {
            self.doc.sync_size();
            self.projection = Projection::new(self.doc.width, self.doc.height);
            self.size_gen += 1;
            return;
        }
        self.projection.reset_below_cache();
        match diff_states(other, &self.doc.state) {
            Changes::All => self.projection.invalidate_all(),
            Changes::Tiles(t) => self.projection.invalidate_many(t),
        }
    }

    pub fn invalidate_all(&mut self) {
        self.projection.reset_below_cache();
        self.projection.invalidate_all();
    }

    fn cancel_all(&mut self) {
        self.cancel_stroke();
        self.cancel_transform();
        self.cancel_filter();
        self.coalesce = None;
    }

    pub fn undo(&mut self) -> bool {
        self.cancel_all();
        let cur = std::mem::replace(&mut self.doc.state, empty_state());
        match self.history.undo(cur, self.state_id) {
            Ok(s) => self.restore(s.state, s.state_id),
            Err(cur) => {
                self.doc.state = cur;
                false
            }
        }
    }

    pub fn redo(&mut self) -> bool {
        self.cancel_all();
        let cur = std::mem::replace(&mut self.doc.state, empty_state());
        match self.history.redo(cur, self.state_id) {
            Ok(s) => self.restore(s.state, s.state_id),
            Err(cur) => {
                self.doc.state = cur;
                false
            }
        }
    }

    fn restore(&mut self, state: DocState, id: u64) -> bool {
        let old = std::mem::replace(&mut self.doc.state, state);
        self.invalidate_between(&old);
        self.state_id = id;
        true
    }

    // ---- painting -------------------------------------------------------

    fn check_paintable(&self) -> Result<LayerId, Refusal> {
        if self.is_busy() {
            return Err(Refusal::Busy);
        }
        let l = self.doc.active_layer();
        if !self.doc.is_effectively_visible(l.id) {
            return Err(Refusal::LayerHidden);
        }
        if !l.is_paintable() {
            return Err(Refusal::NotPaintable(l.kind()));
        }
        Ok(l.id)
    }

    pub fn begin_stroke(
        &mut self,
        params: BrushParams,
        paint: Paint,
        x: f32,
        y: f32,
        pressure: f32,
    ) -> Result<(), Refusal> {
        let layer = self.check_paintable()?;
        let paint = Paint {
            alpha_lock: self.doc.active_layer().alpha_lock,
            ..paint
        };
        self.stroke = Some(ActiveStroke {
            engine: StrokeEngine::new(params),
            mask: Mask::new(self.doc.width, self.doc.height),
            paint,
            layer,
            sel: self.doc.state.selection.clone(),
        });
        self.stroke_to(x, y, pressure);
        Ok(())
    }

    pub fn stroke_to(&mut self, x: f32, y: f32, pressure: f32) {
        let Some(s) = &mut self.stroke else { return };
        self.dabs.clear();
        s.engine.add_point(x, y, pressure, &mut self.dabs);
        self.stamp_pending();
    }

    fn stamp_pending(&mut self) {
        let Some(s) = &mut self.stroke else { return };
        self.dirty.clear();
        for d in &self.dabs {
            s.mask.stamp(d, &mut self.dirty);
        }
        self.dirty.sort_unstable();
        self.dirty.dedup();
        self.projection.invalidate_many(self.dirty.iter().copied());
    }

    pub fn end_stroke(&mut self) {
        if self.stroke.is_none() {
            return;
        }
        self.dabs.clear();
        let s = self.stroke.as_mut().unwrap();
        s.engine.finish(&mut self.dabs);
        self.stamp_pending();
        let s = self.stroke.take().unwrap();
        self.last_point = s.engine.position();
        let label = if s.paint.mode == PaintMode::Erase {
            "Erase"
        } else {
            "Brush stroke"
        };
        self.apply_mask(label, s.layer, &s.mask, s.paint, s.sel.as_deref());
    }

    pub fn cancel_stroke(&mut self) {
        if let Some(s) = self.stroke.take() {
            self.projection.reset_below_cache();
            self.projection
                .invalidate_many(s.mask.touched().iter().copied());
        }
    }

    /// Where the previous stroke ended, for shift-click straight lines.
    pub fn last_point(&self) -> Option<(f32, f32)> {
        self.last_point
    }

    /// Draws a straight line as one stroke.
    pub fn line(
        &mut self,
        params: BrushParams,
        paint: Paint,
        from: (f32, f32),
        to: (f32, f32),
    ) -> Result<(), Refusal> {
        let params = BrushParams {
            smoothing: 0.0,
            ..params
        };
        self.begin_stroke(params, paint, from.0, from.1, 1.0)?;
        self.stroke_to(to.0, to.1, 1.0);
        self.end_stroke();
        self.last_point = Some(to);
        Ok(())
    }

    fn apply_mask(
        &mut self,
        label: &str,
        layer: LayerId,
        mask: &Mask,
        paint: Paint,
        sel: Option<&Selection>,
    ) {
        if mask.is_empty() {
            self.projection.reset_below_cache();
            return;
        }
        let touched = mask.touched().to_vec();
        self.commit(label, |doc| {
            let Some(l) = doc.find_mut(layer) else { return };
            let grid = l.tiles_mut();
            for &c in &touched {
                let m = mask.get(c).unwrap();
                let st = match sel {
                    Some(s) => match s.tile(c) {
                        Some(t) => Some(t),
                        None => continue,
                    },
                    None => None,
                };
                if (paint.mode == PaintMode::Erase || paint.alpha_lock) && grid.get(c).is_none() {
                    continue;
                }
                apply_mask_tile_sel(grid.get_mut_or_insert(c), m, st, &paint);
            }
            grid.prune(touched.iter().copied());
        });
    }

    /// Bucket fill at `(x, y)`. With `sample_all`, the region is found on the
    /// visible composite rather than the active layer alone.
    pub fn fill(
        &mut self,
        x: u32,
        y: u32,
        p: &FillParams,
        sample_all: bool,
        paint: Paint,
    ) -> Result<(), Refusal> {
        let layer = self.check_paintable()?;
        let paint = Paint {
            alpha_lock: self.doc.active_layer().alpha_lock,
            ..paint
        };
        let mask = self.flood(x, y, p, sample_all);
        if let Some(mask) = mask {
            let sel = self.doc.state.selection.clone();
            self.apply_mask("Fill", layer, &mask, paint, sel.as_deref());
        }
        Ok(())
    }

    fn flood(&mut self, x: u32, y: u32, p: &FillParams, sample_all: bool) -> Option<Mask> {
        let (w, h) = (self.doc.width, self.doc.height);
        if sample_all {
            self.projection.update_all(&self.doc, &NoOverride);
            let proj = &self.projection;
            flood_fill(w, h, x, y, |x, y| proj.pixel(x, y), p)
        } else {
            let grid = &self.doc.active_layer().tiles;
            flood_fill(w, h, x, y, |x, y| grid.pixel(x as i64, y as i64), p)
        }
    }

    /// Paints the whole selection (or canvas) with a colour.
    pub fn fill_selection(&mut self, color: [u8; 3]) -> Result<(), Refusal> {
        let layer = self.check_paintable()?;
        let sel = self.selection_or_all();
        let mask = full_mask(&sel);
        let paint = Paint {
            color,
            mode: PaintMode::Paint,
            alpha_lock: self.doc.active_layer().alpha_lock,
        };
        self.apply_mask("Fill selection", layer, &mask, paint, Some(&sel));
        Ok(())
    }

    /// Deletes the selected pixels, or clears the layer without a selection.
    pub fn clear_selected(&mut self) -> Result<(), Refusal> {
        let layer = self.check_paintable()?;
        match self.doc.state.selection.clone() {
            None => {
                if self.doc.active_layer().tiles.is_empty() {
                    return Ok(());
                }
                self.commit("Clear layer", |doc| {
                    let e = doc.empty_grid();
                    if let Some(l) = doc.find_mut(layer) {
                        l.tiles = e;
                    }
                });
            }
            Some(sel) => {
                let mask = full_mask(&sel);
                let paint = Paint {
                    color: [0; 3],
                    mode: PaintMode::Erase,
                    alpha_lock: false,
                };
                self.apply_mask("Delete", layer, &mask, paint, Some(&sel));
            }
        }
        Ok(())
    }

    // ---- selection --------------------------------------------------------

    pub fn selection(&self) -> Option<&Selection> {
        self.doc.state.selection.as_deref()
    }

    fn selection_or_all(&self) -> Selection {
        self.doc
            .state
            .selection
            .as_deref()
            .cloned()
            .unwrap_or_else(|| Selection::all(self.doc.width, self.doc.height))
    }

    /// Replaces the selection (an empty one deselects).
    pub fn set_selection(&mut self, label: &str, sel: Option<Selection>) {
        let sel = sel.filter(|s| !s.is_empty()).map(Arc::new);
        if sel.is_none() && self.doc.state.selection.is_none() {
            return;
        }
        self.commit(label, |doc| doc.state.selection = sel);
    }

    /// Combines a shape with the current selection.
    pub fn select_shape(&mut self, shape: Selection, op: SelectOp) {
        let new = match (&self.doc.state.selection, op) {
            (Some(cur), op) if op != SelectOp::Replace => cur.combine(&shape, op),
            (None, SelectOp::Subtract | SelectOp::Intersect) => return,
            _ => shape,
        };
        self.set_selection("Select", Some(new));
    }

    /// Selects a contiguous region of similar colour.
    pub fn magic_wand(&mut self, x: u32, y: u32, tolerance: u8, sample_all: bool, op: SelectOp) {
        let p = FillParams {
            tolerance,
            grow: 0,
            opacity: 1.0,
        };
        if let Some(m) = self.flood(x, y, &p, sample_all) {
            let s = Selection::from_mask(self.doc.width, self.doc.height, &m);
            self.select_shape(s, op);
        }
    }

    pub fn select_all(&mut self) {
        let s = Selection::all(self.doc.width, self.doc.height);
        self.set_selection("Select all", Some(s));
    }

    pub fn deselect(&mut self) {
        self.set_selection("Deselect", None);
    }

    pub fn invert_selection(&mut self) {
        let s = self.selection_or_all();
        let inv = if self.doc.state.selection.is_some() {
            s.inverted()
        } else {
            Selection::empty(self.doc.width, self.doc.height)
        };
        self.set_selection("Invert selection", Some(inv));
    }

    pub fn feather_selection(&mut self, r: f32) {
        if let Some(s) = self.selection().map(|s| s.feathered(r)) {
            self.set_selection("Feather", Some(s));
        }
    }

    pub fn grow_selection(&mut self, by: i32) {
        if let Some(s) = self.selection().map(|s| s.grown(by)) {
            self.set_selection(if by > 0 { "Grow" } else { "Shrink" }, Some(s));
        }
    }

    /// Selects the opaque pixels of a layer.
    pub fn select_layer_pixels(&mut self, id: LayerId, op: SelectOp) {
        let Some(l) = self.doc.find(id) else { return };
        let g = self.layer_pixels(l);
        let s = Selection::from_alpha(self.doc.width, self.doc.height, &g);
        self.select_shape(s, op);
    }

    // ---- transforms ---------------------------------------------------------

    /// The layer's own pixels (a group's composite).
    fn layer_pixels(&self, l: &Layer) -> TileGrid {
        if !l.is_group() {
            return (*l.tiles).clone();
        }
        let mut g = TileGrid::new(self.doc.width, self.doc.height);
        let mut rect: Option<TileRect> = None;
        l.walk(&mut |d| {
            if let Some(b) = d.tiles.bounds() {
                rect = Some(rect.map_or(b, |r| r.union(b)));
            }
        });
        if let Some(r) = rect {
            for c in r.iter() {
                if let Some(t) = layer_content(l, c, &NoOverride) {
                    g.set(c, Some(Arc::new(t.into_owned())));
                }
            }
        }
        g
    }

    /// Starts transforming the active layer, or the selected part of it.
    pub fn begin_transform(&mut self, smooth: bool) -> Result<(), Refusal> {
        if self.is_busy() {
            return Err(Refusal::Busy);
        }
        let l = self.doc.active_layer();
        if !self.doc.is_effectively_visible(l.id) {
            return Err(Refusal::LayerHidden);
        }
        let (w, h) = (self.doc.width, self.doc.height);
        let id = l.id;
        let kind = l.kind();
        let t = match &l.content {
            Content::Image(obj) => Transforming {
                layer: id,
                kind,
                src: obj.source.clone(),
                src_m: obj.transform,
                frame: obj.transform,
                size: obj.natural_size(),
                delta: Affine::IDENTITY,
                smooth: obj.smooth,
                base: None,
                sel: None,
            },
            Content::Text(obj) => Transforming {
                layer: id,
                kind,
                src: Arc::new(Source::new((*l.tiles).clone(), w, h)),
                src_m: Affine::IDENTITY,
                frame: obj.transform,
                size: obj.bbox,
                delta: Affine::IDENTITY,
                smooth: true,
                base: None,
                sel: None,
            },
            _ => {
                let pixels = self.layer_pixels(l);
                let sel = if l.is_group() {
                    None
                } else {
                    self.doc.state.selection.clone()
                };
                let (float, base) = match &sel {
                    Some(s) => split_by_selection(&pixels, s),
                    None => (pixels, None),
                };
                let Some((x0, y0, x1, y1)) = content_bounds(&float) else {
                    return Err(Refusal::LayerEmpty);
                };
                Transforming {
                    layer: id,
                    kind,
                    src: Arc::new(Source::new(float, w, h)),
                    src_m: Affine::IDENTITY,
                    frame: Affine::translate(x0 as f32, y0 as f32),
                    size: ((x1 - x0) as f32, (y1 - y0) as f32),
                    delta: Affine::IDENTITY,
                    smooth,
                    base: base.map(Arc::new),
                    sel,
                }
            }
        };
        let (cols, rows) = self.doc.grid_dims();
        let r = t.dest_rect(cols, rows);
        self.transform = Some(t);
        self.projection.invalidate_rect(r);
        Ok(())
    }

    pub fn transforming(&self) -> Option<&Transforming> {
        self.transform.as_ref()
    }

    /// Sets the transform applied on top of the starting position.
    pub fn set_transform(&mut self, delta: Affine, smooth: bool) {
        let (cols, rows) = self.doc.grid_dims();
        let Some(t) = &mut self.transform else { return };
        let old = t.dest_rect(cols, rows);
        t.delta = delta;
        t.smooth = smooth;
        let new = t.dest_rect(cols, rows);
        self.projection.invalidate_rect(old);
        self.projection.invalidate_rect(new);
    }

    pub fn cancel_transform(&mut self) {
        if let Some(t) = self.transform.take() {
            let (cols, rows) = self.doc.grid_dims();
            self.projection.reset_below_cache();
            self.projection.invalidate_rect(t.dest_rect(cols, rows));
            let id = TileRect {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
            };
            let start = t.src.dest_tiles(&t.src_m, cols, rows);
            self.projection
                .invalidate_rect(if start.is_empty() { id } else { start });
        }
    }

    pub fn commit_transform(&mut self) {
        let Some(t) = self.transform.take() else {
            return;
        };
        let (cols, rows) = self.doc.grid_dims();
        let start = t.src.dest_tiles(&t.src_m, cols, rows);
        self.projection.invalidate_rect(start);
        self.projection.reset_below_cache();
        if t.delta == Affine::IDENTITY {
            self.projection.invalidate_rect(t.dest_rect(cols, rows));
            return;
        }
        let (w, h) = (self.doc.width, self.doc.height);
        let delta = t.delta;
        let label = if delta.is_translation() {
            "Move"
        } else {
            "Transform"
        };
        match t.kind {
            Kind::Image | Kind::Text => {
                self.commit(label, |doc| {
                    if let Some(l) = doc.find_mut(t.layer) {
                        transform_object(l, &delta, w, h);
                    }
                });
            }
            Kind::Group => {
                let smooth = t.smooth;
                self.commit(label, |doc| {
                    if let Some(l) = doc.find_mut(t.layer) {
                        l.walk_mut(&mut |d| match d.kind() {
                            Kind::Raster => {
                                d.tiles = Arc::new(transform_grid(&d.tiles, &delta, smooth, w, h))
                            }
                            Kind::Image | Kind::Text => transform_object(d, &delta, w, h),
                            Kind::Group => {}
                        });
                    }
                });
            }
            Kind::Raster => {
                let rendered = if !t.smooth && delta.is_translation() && t.base.is_none() {
                    t.src
                        .base
                        .translated(delta.m[4].round() as i64, delta.m[5].round() as i64)
                } else {
                    t.src.render(&t.matrix(), t.smooth, w, h)
                };
                let mut out = t
                    .base
                    .as_deref()
                    .cloned()
                    .unwrap_or_else(|| TileGrid::new(w, h));
                if t.base.is_none() {
                    out = rendered;
                } else {
                    for (c, tile) in rendered.iter() {
                        if out.get(c).is_none() {
                            out.set(c, Some(tile.clone()));
                        } else {
                            blend_tile(out.get_mut_or_insert(c), tile, 255, BlendMode::Normal);
                        }
                    }
                }
                let new_sel = t.sel.as_ref().map(|s| {
                    let g = transform_grid(&s.to_grid(), &delta, t.smooth, w, h);
                    Arc::new(Selection::from_alpha(w, h, &g))
                });
                self.commit(label, |doc| {
                    if let Some(l) = doc.find_mut(t.layer) {
                        l.tiles = Arc::new(out);
                    }
                    if new_sel.is_some() {
                        doc.state.selection = new_sel;
                    }
                });
            }
        }
    }

    /// Flips or rotates the active layer in place, about its centre.
    pub fn quick_transform(
        &mut self,
        label: &str,
        f: impl Fn(f32, f32, f32, f32) -> Affine,
    ) -> Result<(), Refusal> {
        self.begin_transform(true)?;
        let t = self.transform.as_ref().unwrap();
        let q = t.quad();
        let cx = q.iter().map(|p| p.0).sum::<f32>() / 4.0;
        let cy = q.iter().map(|p| p.1).sum::<f32>() / 4.0;
        let (w, h) = t.size;
        let d = f(cx, cy, w, h);
        let lossless = d.m.iter().all(|v| (v - v.round()).abs() < 1e-4) || {
            // 90° steps around a centre on a half-pixel are lossless too.
            let [a, b, c, dd, ..] = d.m;
            [a, b, c, dd]
                .iter()
                .all(|v| (v.abs() - v.abs().round()).abs() < 1e-4)
        };
        self.set_transform(d, !lossless);
        let _ = label;
        self.commit_transform();
        Ok(())
    }

    // ---- filters --------------------------------------------------------------

    /// Starts previewing a filter on the active layer.
    pub fn begin_filter(&mut self, f: Arc<dyn Filter>) -> Result<(), Refusal> {
        let layer = self.check_paintable()?;
        let sel = self.doc.state.selection.clone();
        let rect = self.filter_rect(&*f);
        self.filter = Some(Filtering {
            layer,
            filter: f,
            sel,
            rect,
        });
        self.projection.invalidate_rect(rect);
        Ok(())
    }

    fn filter_rect(&self, f: &dyn Filter) -> TileRect {
        let l = self.doc.active_layer();
        let (cols, rows) = self.doc.grid_dims();
        let t = TILE as i64;
        let m = f.margin() as i64;
        if f.full_canvas() {
            return TileRect {
                x0: 0,
                y0: 0,
                x1: cols,
                y1: rows,
            };
        }
        match l.tiles.bounds() {
            None => TileRect {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
            },
            Some(b) => TileRect::from_px(
                b.x0 as i64 * t - m,
                b.y0 as i64 * t - m,
                b.x1 as i64 * t + m,
                b.y1 as i64 * t + m,
                cols,
                rows,
            ),
        }
    }

    /// Changes the filter being previewed (e.g. as a slider moves).
    pub fn update_filter(&mut self, f: Arc<dyn Filter>) {
        let rect = self.filter_rect(&*f);
        let Some(fl) = &mut self.filter else { return };
        let old = fl.rect;
        fl.filter = f;
        fl.rect = rect;
        self.projection.invalidate_rect(old);
        self.projection.invalidate_rect(rect);
    }

    pub fn filtering(&self) -> bool {
        self.filter.is_some()
    }

    pub fn cancel_filter(&mut self) {
        if let Some(f) = self.filter.take() {
            self.projection.reset_below_cache();
            self.projection.invalidate_rect(f.rect);
        }
    }

    pub fn commit_filter(&mut self, label: &str) {
        let Some(f) = self.filter.take() else { return };
        self.projection.reset_below_cache();
        let Some(l) = self.doc.find(f.layer) else {
            return;
        };
        let src = l.tiles.clone();
        let alpha_lock = l.alpha_lock;
        use rayon::prelude::*;
        let coords: Vec<TileCoord> = f.rect.iter().collect();
        let tiles: Vec<(TileCoord, Option<Tile>)> = coords
            .par_iter()
            .map(|&c| {
                let new = f.filter.apply(&src, c);
                (
                    c,
                    restrict(
                        src.get(c).map(|t| &**t),
                        new,
                        f.sel.as_deref(),
                        c,
                        alpha_lock,
                    ),
                )
            })
            .collect();
        self.commit(label, |doc| {
            if let Some(l) = doc.find_mut(f.layer) {
                let g = l.tiles_mut();
                for (c, t) in tiles {
                    g.set(c, t.map(Arc::new));
                }
            }
        });
    }

    /// Applies a filter in one go.
    pub fn apply_filter(&mut self, label: &str, f: Arc<dyn Filter>) -> Result<(), Refusal> {
        self.begin_filter(f)?;
        self.commit_filter(label);
        Ok(())
    }

    // ---- layers -------------------------------------------------------------

    pub fn select_layer(&mut self, id: LayerId) {
        if self.doc.find(id).is_some() && !self.is_busy() && self.doc.state.active != id {
            self.doc.state.active = id;
            if self.onion.is_some() {
                self.invalidate_all();
            }
        }
    }

    /// Where a new layer goes: just above the active one, beside it.
    fn insert_place(&self) -> Place {
        let p = self.doc.place_of(self.doc.state.active).unwrap_or(Place {
            parent: None,
            index: self.doc.layers().len(),
        });
        Place {
            parent: p.parent,
            index: p.index + 1,
        }
    }

    pub fn add_layer(&mut self) {
        let place = self.insert_place();
        self.commit("New layer", |doc| {
            let l = doc.new_layer(doc.next_layer_name());
            let id = l.id;
            doc.put(l, place);
            doc.state.active = id;
        });
    }

    pub fn add_group(&mut self) {
        let place = self.insert_place();
        self.commit("New group", |doc| {
            let g = doc.new_group(doc.next_name("Group"));
            let id = g.id;
            doc.put(g, place);
            doc.state.active = id;
        });
    }

    /// Inserts an already built layer above the active one.
    pub fn insert_layer(&mut self, label: &str, mut layer: Layer) {
        let place = self.insert_place();
        self.commit(label, |doc| {
            doc.reassign_ids(&mut layer);
            let id = layer.id;
            doc.put(layer, place);
            doc.state.active = id;
        });
    }

    pub fn delete_layer(&mut self, id: LayerId) -> bool {
        if self.doc.layer_count() <= 1 || self.doc.find(id).is_none() {
            return false;
        }
        if self.doc.layers().len() == 1 && self.doc.layers()[0].id == id {
            return false;
        }
        self.commit("Delete layer", |doc| {
            let Some((_, place)) = doc.take(id) else {
                return;
            };
            if doc.state.active == id || doc.find(doc.state.active).is_none() {
                let sib = doc.siblings(place.parent);
                doc.state.active = if sib.is_empty() {
                    place.parent.unwrap_or(0)
                } else {
                    sib[place.index.saturating_sub(1).min(sib.len() - 1)].id
                };
            }
        });
        true
    }

    pub fn duplicate_layer(&mut self, id: LayerId) {
        let Some(place) = self.doc.place_of(id) else {
            return;
        };
        self.commit("Duplicate layer", |doc| {
            let mut l = doc.find(id).unwrap().clone();
            doc.reassign_ids(&mut l);
            l.name = format!("{} copy", l.name);
            let nid = l.id;
            doc.put(
                l,
                Place {
                    parent: place.parent,
                    index: place.index + 1,
                },
            );
            doc.state.active = nid;
        });
    }

    /// The layer directly below `id` among its siblings.
    fn below(&self, id: LayerId) -> Option<LayerId> {
        let p = self.doc.place_of(id)?;
        let i = p.index.checked_sub(1)?;
        Some(self.doc.siblings(p.parent)[i].id)
    }

    pub fn can_merge_down(&self, id: LayerId) -> bool {
        self.below(id).is_some_and(|b| {
            self.doc.find(id).is_some_and(|l| l.visible)
                && self.doc.find(b).is_some_and(|l| l.visible)
        })
    }

    /// Bakes a layer into the one beneath it.
    pub fn merge_down(&mut self, id: LayerId) -> bool {
        if !self.can_merge_down(id) {
            return false;
        }
        let lower_id = self.below(id).unwrap();
        let (upper, lower) = (
            self.doc.find(id).unwrap().clone(),
            self.doc.find(lower_id).unwrap().clone(),
        );
        // Composite the pair as they look, with the lower layer's own
        // opacity and mode set aside (it keeps them as properties).
        let mut l2 = lower.clone();
        l2.opacity = 1.0;
        l2.blend = BlendMode::Normal;
        l2.clip = false;
        let pair = [l2, upper];
        let mut rect: Option<TileRect> = None;
        for l in &pair {
            l.walk(&mut |d| {
                if let Some(b) = d.tiles.bounds() {
                    rect = Some(rect.map_or(b, |r| r.union(b)));
                }
            });
        }
        let mut g = TileGrid::new(self.doc.width, self.doc.height);
        if let Some(r) = rect {
            for c in r.iter() {
                let mut t = Tile::filled([0; 4]);
                compose_list(&pair, c, &mut t, &NoOverride);
                if !t.is_empty() {
                    g.set(c, Some(Arc::new(t)));
                }
            }
        }
        self.commit("Merge down", |doc| {
            let frames = doc.state.frames;
            doc.take(id);
            if let Some(l) = doc.find_mut(lower_id) {
                l.tiles = Arc::new(g);
                if !l.is_paintable() {
                    l.content = Content::Raster;
                    l.cels = vec![None; frames];
                }
            }
            doc.state.active = lower_id;
        });
        true
    }

    /// Moves a layer to a new place in the tree (index counted after the
    /// layer is removed from its old place). Refuses to put a group inside
    /// itself.
    pub fn move_layer(&mut self, id: LayerId, to: Place) {
        let Some(from) = self.doc.place_of(id) else {
            return;
        };
        if let Some(p) = to.parent
            && (p == id || self.doc.ancestors(p).contains(&id))
        {
            return;
        }
        if from == to {
            return;
        }
        self.commit("Reorder layers", |doc| {
            if let Some((l, _)) = doc.take(id) {
                doc.put(l, to);
                doc.state.active = id;
            }
        });
    }

    /// Wraps a layer in a new group.
    pub fn group_layer(&mut self, id: LayerId) {
        let Some(place) = self.doc.place_of(id) else {
            return;
        };
        self.commit("Group layer", |doc| {
            let mut g = doc.new_group(doc.next_name("Group"));
            let gid = g.id;
            if let Some((l, _)) = doc.take(id) {
                g.children_mut().unwrap().push(l);
            }
            doc.put(g, place);
            doc.state.active = gid;
        });
    }

    /// Replaces a group by its children.
    pub fn ungroup(&mut self, id: LayerId) {
        let Some(place) = self.doc.place_of(id) else {
            return;
        };
        if !self.doc.find(id).is_some_and(|l| l.is_group()) {
            return;
        }
        self.commit("Ungroup", |doc| {
            let Some((mut g, _)) = doc.take(id) else {
                return;
            };
            let (op, vis) = (g.opacity, g.visible);
            let children = std::mem::take(g.children_mut().unwrap());
            let top = children.last().map(|c| c.id);
            for (i, mut c) in children.into_iter().enumerate() {
                c.opacity *= op;
                c.visible &= vis;
                doc.put(
                    c,
                    Place {
                        parent: place.parent,
                        index: place.index + i,
                    },
                );
            }
            if let Some(t) = top {
                doc.state.active = t;
            }
        });
    }

    pub fn toggle_clip(&mut self, id: LayerId) {
        let Some(l) = self.doc.find(id) else { return };
        let v = !l.clip;
        self.commit(if v { "Clip to below" } else { "Unclip" }, |doc| {
            if let Some(l) = doc.find_mut(id) {
                l.clip = v;
            }
        });
    }

    /// Turns a text, image or group layer into plain pixels.
    pub fn rasterize(&mut self, id: LayerId) {
        let Some(l) = self.doc.find(id) else { return };
        if l.is_paintable() {
            return;
        }
        let g = self.layer_pixels(l);
        self.commit("Rasterize", |doc| {
            let frames = doc.state.frames;
            if let Some(l) = doc.find_mut(id) {
                l.tiles = Arc::new(g);
                l.content = Content::Raster;
                l.cels = vec![None; frames];
            }
        });
    }

    /// Changes properties of one layer as an undo step.
    pub fn set_layer(&mut self, id: LayerId, label: &str, f: impl FnOnce(&mut Layer)) {
        self.commit(label, |doc| {
            if let Some(l) = doc.find_mut(id) {
                f(l);
            }
        });
    }

    /// Like [`set_layer`](Self::set_layer), merging repeated calls (sliders).
    pub fn set_layer_coalesced(&mut self, id: LayerId, label: &str, f: impl FnOnce(&mut Layer)) {
        self.commit_coalesced(label, |doc| {
            if let Some(l) = doc.find_mut(id) {
                f(l);
            }
        });
    }

    /// Opens or closes a group in the layer panel (not an undo step).
    pub fn set_expanded(&mut self, id: LayerId, open: bool) {
        if let Some(l) = self.doc.find_mut(id)
            && let Content::Group { expanded, .. } = &mut l.content
        {
            *expanded = open;
        }
    }

    /// Merges every visible layer into one.
    pub fn flatten(&mut self) {
        let (w, h) = (self.doc.width, self.doc.height);
        let mut g = TileGrid::new(w, h);
        let layers = self.doc.layers().to_vec();
        let mut rect: Option<TileRect> = None;
        self.doc.walk(|d| {
            if let Some(b) = d.tiles.bounds() {
                rect = Some(rect.map_or(b, |r| r.union(b)));
            }
        });
        if let Some(r) = rect {
            for c in r.iter() {
                let mut t = Tile::filled([0; 4]);
                compose_list(&layers, c, &mut t, &NoOverride);
                if !t.is_empty() {
                    g.set(c, Some(Arc::new(t)));
                }
            }
        }
        self.commit("Flatten image", |doc| {
            let mut l = doc.new_layer("Background");
            l.tiles = Arc::new(g);
            doc.state.active = l.id;
            doc.state.layers = vec![l];
        });
    }

    pub fn clear_layer(&mut self) {
        let _ = self.clear_selected();
    }

    pub fn set_paper(&mut self, paper: Option<[u8; 3]>) {
        if self.doc.state.paper != paper {
            self.commit_coalesced("Paper colour", |doc| doc.state.paper = paper);
        }
    }

    // ---- objects --------------------------------------------------------------

    /// Adds a text layer above the active one.
    pub fn add_text(&mut self, mut obj: TextObject) -> LayerId {
        let (w, h) = (self.doc.width, self.doc.height);
        let tiles = obj.render(w, h);
        let name: String = obj
            .text
            .lines()
            .next()
            .unwrap_or("Text")
            .chars()
            .take(32)
            .collect();
        let mut l = self.doc.new_layer(if name.trim().is_empty() {
            "Text".into()
        } else {
            name
        });
        l.tiles = Arc::new(tiles);
        l.content = Content::Text(Arc::new(obj));
        l.cels.clear();
        self.insert_layer("Add text", l);
        self.doc.state.active
    }

    /// Replaces a text layer's text and settings.
    pub fn set_text(&mut self, id: LayerId, mut obj: TextObject) {
        let (w, h) = (self.doc.width, self.doc.height);
        let tiles = obj.render(w, h);
        self.commit_coalesced("Edit text", |doc| {
            if let Some(l) = doc.find_mut(id) {
                let auto_name = match &l.content {
                    Content::Text(old) => {
                        old.text
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(32)
                            .collect::<String>()
                            == l.name
                    }
                    _ => false,
                };
                if auto_name {
                    let n: String = obj
                        .text
                        .lines()
                        .next()
                        .unwrap_or("Text")
                        .chars()
                        .take(32)
                        .collect();
                    l.name = if n.trim().is_empty() {
                        "Text".into()
                    } else {
                        n
                    };
                }
                l.tiles = Arc::new(tiles);
                l.content = Content::Text(Arc::new(obj));
            }
        });
    }

    /// Places an image as a transformable object, fitted inside the canvas.
    pub fn place_image(&mut self, name: &str, src: Source, at: Option<(f32, f32)>) -> LayerId {
        let (w, h) = (self.doc.width as f32, self.doc.height as f32);
        let (iw, ih) = (src.width as f32, src.height as f32);
        let s = (w / iw).min(h / ih).min(1.0);
        let (cx, cy) = at.unwrap_or((w * 0.5, h * 0.5));
        let m =
            Affine::translate(cx - iw * s * 0.5, cy - ih * s * 0.5).compose(Affine::scale(s, s));
        let obj = ImageObject {
            source: Arc::new(src),
            transform: m,
            smooth: true,
        };
        let tiles = obj.render(self.doc.width, self.doc.height);
        let mut l = self.doc.new_layer(name);
        l.tiles = Arc::new(tiles);
        l.content = Content::Image(Arc::new(obj));
        l.cels.clear();
        self.insert_layer("Place image", l);
        self.doc.state.active
    }

    pub fn set_image_smooth(&mut self, id: LayerId, smooth: bool) {
        let (w, h) = (self.doc.width, self.doc.height);
        self.set_layer(id, "Image sampling", |l| {
            if let Content::Image(o) = &l.content {
                let mut o = (**o).clone();
                o.smooth = smooth;
                l.tiles = Arc::new(o.render(w, h));
                l.content = Content::Image(Arc::new(o));
            }
        });
    }

    // ---- clipboard ----------------------------------------------------------

    /// Copies the selected pixels (or the whole layer) of the active layer,
    /// or of the visible image when `merged`.
    pub fn copy(&mut self, merged: bool) -> Option<Copied> {
        let (w, h) = (self.doc.width, self.doc.height);
        let grid = if merged {
            self.projection.update_all(&self.doc, &NoOverride);
            let mut g = TileGrid::new(w, h);
            let (cols, rows) = self.doc.grid_dims();
            for y in 0..rows {
                for x in 0..cols {
                    let c = TileCoord::new(x, y);
                    if let Some(t) = self.projection.tile(0, c) {
                        g.set(c, Some(Arc::new(t.clone())));
                    }
                }
            }
            g
        } else {
            self.layer_pixels(self.doc.active_layer())
        };
        let grid = match self.selection() {
            Some(s) => split_by_selection(&grid, s).0,
            None => grid,
        };
        let (x0, y0, x1, y1) = content_bounds(&grid)?;
        let (cw, ch) = (x1 - x0, y1 - y0);
        let mut rgba = vec![0u8; (cw * ch * 4) as usize];
        for y in 0..ch {
            for x in 0..cw {
                let p = crate::color::unpremultiply(grid.pixel((x0 + x) as i64, (y0 + y) as i64));
                let o = ((y * cw + x) * 4) as usize;
                rgba[o..o + 4].copy_from_slice(&p);
            }
        }
        Some(Copied {
            x: x0 as i64,
            y: y0 as i64,
            width: cw,
            height: ch,
            rgba,
        })
    }

    /// Copies, then deletes the selected pixels.
    pub fn cut(&mut self) -> Option<Copied> {
        let c = self.copy(false)?;
        let _ = self.clear_selected();
        Some(c)
    }

    /// Adds pixels as a new layer at a canvas position.
    pub fn paste(&mut self, name: &str, c: &Copied) {
        let (w, h) = (self.doc.width, self.doc.height);
        let g = crate::io::grid_from_rgba_at(w, h, &c.rgba, c.x, c.y, c.width, c.height);
        let mut l = self.doc.new_layer(name);
        l.tiles = Arc::new(g);
        self.insert_layer("Paste", l);
    }

    /// Copies the selection (or layer) to a new layer ("layer via copy").
    pub fn layer_via_copy(&mut self) {
        if let Some(c) = self.copy(false) {
            let name = format!("{} copy", self.doc.active_layer().name);
            self.paste(&name, &c);
        }
    }

    // ---- canvas ---------------------------------------------------------------

    /// Remaps every layer, the selection and the canvas size through `m`.
    fn remap(&mut self, label: &str, nw: u32, nh: u32, m: Affine, smooth: bool) {
        let integer = m.is_translation() && m.m.iter().all(|v| (v - v.round()).abs() < 1e-4);
        let grid = |g: &TileGrid| -> TileGrid {
            if integer {
                g.reframed(nw, nh, m.m[4].round() as i64, m.m[5].round() as i64)
            } else {
                let (w, h) = (g.cols() * TILE as u32, g.rows() * TILE as u32);
                Source::new(g.clone(), w, h).render(&m, smooth, nw, nh)
            }
        };
        self.commit(label, |doc| {
            doc.walk_mut(|l| match l.kind() {
                Kind::Raster => {
                    l.tiles = Arc::new(grid(&l.tiles));
                    for c in l.cels.iter_mut().flatten() {
                        *c = Arc::new(grid(c));
                    }
                }
                Kind::Text | Kind::Image => transform_object(l, &m, nw, nh),
                Kind::Group => {}
            });
            if let Some(s) = doc.state.selection.take() {
                let g = grid(&s.to_grid());
                let ns = Selection::from_alpha(nw, nh, &g);
                doc.state.selection = (!ns.is_empty()).then(|| Arc::new(ns));
            }
            doc.set_size(nw, nh);
        });
    }

    /// Scales the whole image.
    pub fn resize_image(&mut self, nw: u32, nh: u32, smooth: bool) {
        let (w, h) = (self.doc.width as f32, self.doc.height as f32);
        if (nw, nh) == (self.doc.width, self.doc.height) {
            return;
        }
        self.remap(
            "Resize image",
            nw,
            nh,
            Affine::scale(nw as f32 / w, nh as f32 / h),
            smooth,
        );
    }

    /// Changes the canvas size, placing the old canvas at `(dx, dy)`.
    pub fn resize_canvas(&mut self, nw: u32, nh: u32, dx: i64, dy: i64) {
        if (nw, nh, dx, dy) == (self.doc.width, self.doc.height, 0, 0) {
            return;
        }
        self.remap(
            "Canvas size",
            nw,
            nh,
            Affine::translate(dx as f32, dy as f32),
            false,
        );
    }

    /// Crops to a rectangle.
    pub fn crop(&mut self, x0: u32, y0: u32, x1: u32, y1: u32) {
        let (x1, y1) = (x1.min(self.doc.width), y1.min(self.doc.height));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        self.remap(
            "Crop",
            x1 - x0,
            y1 - y0,
            Affine::translate(-(x0 as f32), -(y0 as f32)),
            false,
        );
    }

    pub fn crop_to_selection(&mut self) -> bool {
        let Some((x0, y0, x1, y1)) = self.selection().and_then(|s| s.bounds()) else {
            return false;
        };
        self.crop(x0, y0, x1, y1);
        true
    }

    /// Rotates the canvas by quarter turns clockwise.
    pub fn rotate_canvas(&mut self, quarters: i32) {
        let (w, h) = (self.doc.width as f32, self.doc.height as f32);
        let q = quarters.rem_euclid(4);
        let half = std::f32::consts::FRAC_PI_2;
        let (m, nw, nh) = match q {
            1 => (Affine::translate(h, 0.0).compose(exact_rot(1)), h, w),
            2 => (Affine::translate(w, h).compose(exact_rot(2)), w, h),
            3 => (Affine::translate(0.0, w).compose(exact_rot(3)), h, w),
            _ => return,
        };
        let _ = half;
        self.remap("Rotate canvas", nw as u32, nh as u32, m, false);
    }

    pub fn flip_canvas(&mut self, horizontal: bool) {
        let (w, h) = (self.doc.width as f32, self.doc.height as f32);
        let m = if horizontal {
            Affine::translate(w, 0.0).compose(Affine::scale(-1.0, 1.0))
        } else {
            Affine::translate(0.0, h).compose(Affine::scale(1.0, -1.0))
        };
        self.remap("Flip canvas", self.doc.width, self.doc.height, m, false);
    }

    // ---- animation ------------------------------------------------------------

    /// Shows another frame (not an undo step).
    pub fn set_frame(&mut self, f: usize) {
        if f >= self.doc.state.frames || f == self.doc.state.frame || self.is_busy() {
            return;
        }
        let before = self.doc.state.clone();
        self.doc.set_frame(f);
        self.invalidate_between(&before);
        if self.onion.is_some() {
            self.invalidate_all();
        }
    }

    /// Inserts a frame after the current one, blank or a copy of it.
    pub fn add_frame(&mut self, duplicate: bool) {
        self.cancel_all();
        let f = self.doc.state.frame;
        self.commit(
            if duplicate {
                "Duplicate frame"
            } else {
                "New frame"
            },
            |doc| {
                let frames = doc.state.frames;
                doc.walk_mut(|l| {
                    if !l.is_paintable() {
                        return;
                    }
                    l.cels.resize(frames, None);
                    let copy = duplicate.then(|| l.tiles.clone()).filter(|t| !t.is_empty());
                    l.cels.insert(f + 1, copy);
                });
                doc.state.frames += 1;
                doc.set_frame(f + 1);
            },
        );
    }

    pub fn delete_frame(&mut self) {
        if self.doc.state.frames <= 1 {
            return;
        }
        self.cancel_all();
        let f = self.doc.state.frame;
        self.commit("Delete frame", |doc| {
            let frames = doc.state.frames;
            let next = if f + 1 < frames { f + 1 } else { f - 1 };
            doc.set_frame(next);
            doc.walk_mut(|l| {
                if l.is_paintable() {
                    l.cels.resize(frames, None);
                    l.cels.remove(f);
                }
            });
            doc.state.frames -= 1;
            // Frame indices above `f` shifted down by one.
            doc.state.frame = if next > f { f } else { next };
        });
    }

    /// Swaps the current frame with a neighbour (`dir` = -1 or 1).
    pub fn move_frame(&mut self, dir: i32) {
        let f = self.doc.state.frame;
        let to = f as i64 + dir as i64;
        if to < 0 || to >= self.doc.state.frames as i64 {
            return;
        }
        let to = to as usize;
        self.cancel_all();
        self.commit("Move frame", |doc| {
            let frames = doc.state.frames;
            let empty = doc.empty_grid();
            doc.walk_mut(|l| {
                if !l.is_paintable() {
                    return;
                }
                l.cels.resize(frames, None);
                let cur = std::mem::replace(&mut l.tiles, empty.clone());
                l.cels[f] = (!cur.is_empty()).then_some(cur);
                l.cels.swap(f, to);
                l.tiles = l.cels[to].take().unwrap_or_else(|| empty.clone());
            });
            doc.state.frame = to;
        });
    }

    pub fn set_fps(&mut self, fps: u32) {
        let fps = fps.clamp(1, 60);
        if fps != self.doc.state.fps {
            self.commit_coalesced("Frame rate", |doc| doc.state.fps = fps);
        }
    }

    pub fn set_onion(&mut self, o: Option<Onion>) {
        if self.onion != o {
            self.onion = o;
            self.invalidate_all();
        }
    }

    /// Flattens one frame to straight RGBA.
    pub fn render_frame(&self, f: usize) -> Vec<u8> {
        let mut d = self.doc.clone();
        d.set_frame(f);
        crate::render::flatten(&d)
    }

    // ---- sampling -----------------------------------------------------------

    /// Straight-alpha colour of the visible composite at a canvas pixel.
    pub fn sample(&self, x: f32, y: f32) -> Option<[u8; 4]> {
        if x < 0.0 || y < 0.0 || x >= self.doc.width as f32 || y >= self.doc.height as f32 {
            return None;
        }
        Some(crate::color::unpremultiply(
            self.projection.pixel(x as u32, y as u32),
        ))
    }
}

/// An exact rotation by quarter turns (no floating-point drift).
fn exact_rot(q: i32) -> Affine {
    match q.rem_euclid(4) {
        1 => Affine {
            m: [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
        },
        2 => Affine {
            m: [-1.0, 0.0, 0.0, -1.0, 0.0, 0.0],
        },
        3 => Affine {
            m: [0.0, -1.0, 1.0, 0.0, 0.0, 0.0],
        },
        _ => Affine::IDENTITY,
    }
}

fn transform_grid(g: &TileGrid, m: &Affine, smooth: bool, w: u32, h: u32) -> TileGrid {
    if m.is_translation() && (!smooth || m.m.iter().all(|v| (v - v.round()).abs() < 1e-4)) {
        return g.translated(m.m[4].round() as i64, m.m[5].round() as i64);
    }
    Source::new(g.clone(), w, h).render(m, smooth, w, h)
}

/// Applies a canvas-space transform to a text or image layer.
fn transform_object(l: &mut Layer, m: &Affine, w: u32, h: u32) {
    match &l.content {
        Content::Image(o) => {
            let mut o = (**o).clone();
            o.transform = m.compose(o.transform);
            l.tiles = Arc::new(o.render(w, h));
            l.content = Content::Image(Arc::new(o));
        }
        Content::Text(o) => {
            let mut o = (**o).clone();
            o.transform = m.compose(o.transform);
            l.tiles = Arc::new(o.render(w, h));
            l.content = Content::Text(Arc::new(o));
        }
        _ => {}
    }
}

/// Splits pixels into the selected part and the rest.
fn split_by_selection(g: &TileGrid, s: &Selection) -> (TileGrid, Option<TileGrid>) {
    let mut inside = TileGrid::new(g.cols() * TILE as u32, g.rows() * TILE as u32);
    let mut outside = g.clone();
    for (c, t) in g.iter() {
        let Some(st) = s.tile(c) else { continue };
        let mut a = (**t).clone();
        let mut b = (**t).clone();
        for i in 0..TILE_PX {
            let v = st.v[i] as u32;
            for ch in 0..4 {
                a.px[i][ch] = crate::blend::div255(t.px[i][ch] as u32 * v) as u8;
                b.px[i][ch] = crate::blend::div255(t.px[i][ch] as u32 * (255 - v)) as u8;
            }
        }
        inside.set(c, (!a.is_empty()).then(|| Arc::new(a)));
        outside.set(c, (!b.is_empty()).then(|| Arc::new(b)));
    }
    (inside, Some(outside))
}

/// A full-coverage mask over every tile the selection touches.
fn full_mask(sel: &Selection) -> Mask {
    let (w, h) = sel.size();
    let mut m = Mask::new(w, h);
    if let Some(r) = sel.tile_bounds() {
        m.set_from(r, |c, t: &mut MaskTile| {
            if sel.tile(c).is_some() {
                t.v.fill(65535);
                true
            } else {
                false
            }
        });
    }
    m
}

fn empty_state() -> DocState {
    DocState {
        layers: vec![],
        active: 0,
        paper: None,
        selection: None,
        frames: 1,
        frame: 0,
        fps: 12,
        size: (1, 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paint(c: [u8; 3]) -> Paint {
        Paint {
            color: c,
            mode: PaintMode::Paint,
            alpha_lock: false,
        }
    }

    fn brush() -> BrushParams {
        BrushParams {
            size: 20.0,
            hardness: 1.0,
            pressure_size: false,
            smoothing: 0.0,
            ..Default::default()
        }
    }

    fn layer_id(ed: &Editor, i: usize) -> LayerId {
        ed.doc.layers()[i].id
    }

    #[test]
    fn stroke_undo_redo() {
        let mut ed = Editor::new(Document::new(512, 512, Some([255; 3])));
        ed.update_projection(None, usize::MAX);
        ed.begin_stroke(brush(), paint([255, 0, 0]), 50.0, 50.0, 1.0)
            .unwrap();
        ed.stroke_to(200.0, 50.0, 1.0);
        ed.update_projection(None, usize::MAX);
        assert_eq!(ed.sample(120.0, 50.0), Some([255, 0, 0, 255]));
        ed.end_stroke();
        assert!(ed.is_modified());
        ed.update_projection(None, usize::MAX);
        assert_eq!(ed.sample(120.0, 50.0), Some([255, 0, 0, 255]));
        assert!(ed.undo());
        assert!(!ed.is_modified());
        ed.update_projection(None, usize::MAX);
        assert_eq!(ed.sample(120.0, 50.0), Some([255, 255, 255, 255]));
        assert!(ed.redo());
        ed.update_projection(None, usize::MAX);
        assert_eq!(ed.sample(120.0, 50.0), Some([255, 0, 0, 255]));
    }

    #[test]
    fn hidden_layer_refuses() {
        let mut ed = Editor::new(Document::new(64, 64, None));
        ed.doc.state.layers[0].visible = false;
        assert_eq!(
            ed.begin_stroke(brush(), paint([0; 3]), 1.0, 1.0, 1.0),
            Err(Refusal::LayerHidden)
        );
    }

    #[test]
    fn layer_ops_and_merge() {
        let mut ed = Editor::new(Document::new(256, 256, Some([255; 3])));
        ed.begin_stroke(brush(), paint([0, 0, 255]), 10.0, 10.0, 1.0)
            .unwrap();
        ed.end_stroke();
        ed.add_layer();
        assert_eq!(ed.doc.layers().len(), 2);
        assert_eq!(ed.doc.active_id(), layer_id(&ed, 1));
        let a = ed.doc.active_id();
        ed.set_layer(a, "Opacity", |l| l.opacity = 0.5);
        ed.begin_stroke(brush(), paint([255, 0, 0]), 10.0, 10.0, 1.0)
            .unwrap();
        ed.end_stroke();
        ed.update_projection(None, usize::MAX);
        let before = ed.sample(10.0, 10.0).unwrap();
        assert!(ed.merge_down(a));
        assert_eq!(ed.doc.layers().len(), 1);
        ed.update_projection(None, usize::MAX);
        let after = ed.sample(10.0, 10.0).unwrap();
        for ch in 0..4 {
            assert!(
                (before[ch] as i32 - after[ch] as i32).abs() <= 1,
                "{before:?} {after:?}"
            );
        }
        ed.undo();
        assert_eq!(ed.doc.layers().len(), 2);
        ed.duplicate_layer(ed.doc.active_id());
        assert_eq!(ed.doc.layers()[2].name, "Layer 2 copy");
        let top = layer_id(&ed, 2);
        ed.move_layer(
            top,
            Place {
                parent: None,
                index: 0,
            },
        );
        assert_eq!(ed.doc.active_id(), layer_id(&ed, 0));
        assert!(ed.delete_layer(ed.doc.active_id()));
        assert_eq!(ed.doc.layers().len(), 2);
    }

    #[test]
    fn coalesced_edits_are_one_step() {
        let mut ed = Editor::new(Document::new(64, 64, None));
        let id = ed.doc.active_id();
        for i in 0..10 {
            ed.set_layer_coalesced(id, "Layer opacity", |l| l.opacity = i as f32 / 10.0);
        }
        ed.end_gesture();
        assert_eq!(ed.history.len().0, 1);
        ed.undo();
        assert_eq!(ed.doc.active_layer().opacity, 1.0);
    }

    #[test]
    fn move_layer_content() {
        let mut ed = Editor::new(Document::new(256, 256, None));
        ed.begin_stroke(brush(), paint([0, 255, 0]), 30.0, 30.0, 1.0)
            .unwrap();
        ed.end_stroke();
        ed.begin_transform(false).unwrap();
        ed.set_transform(Affine::translate(100.0, 7.0), false);
        ed.update_projection(None, usize::MAX);
        assert_eq!(ed.sample(130.0, 37.0), Some([0, 255, 0, 255]));
        assert_eq!(ed.sample(30.0, 30.0), Some([0; 4]));
        ed.commit_transform();
        ed.update_projection(None, usize::MAX);
        assert_eq!(ed.sample(130.0, 37.0), Some([0, 255, 0, 255]));
        assert_eq!(ed.doc.active_layer().tiles.pixel(130, 37), [0, 255, 0, 255]);
    }

    #[test]
    fn fill_sample_all() {
        let mut ed = Editor::new(Document::new(128, 128, Some([255; 3])));
        let bottom = ed.doc.active_id();
        // Line art on the top layer: a vertical wall at x=64.
        ed.add_layer();
        ed.line(brush(), paint([0; 3]), (64.0, -10.0), (64.0, 140.0))
            .unwrap();
        ed.select_layer(bottom);
        let p = FillParams {
            tolerance: 16,
            grow: 0,
            opacity: 1.0,
        };
        ed.fill(10, 10, &p, true, paint([255, 0, 0])).unwrap();
        let l = &ed.doc.layers()[0].tiles;
        assert_eq!(l.pixel(10, 100), [255, 0, 0, 255]);
        assert_eq!(l.pixel(120, 100), [0; 4]);
    }

    #[test]
    fn erase_and_alpha_lock() {
        let mut ed = Editor::new(Document::new(128, 128, None));
        let id = ed.doc.active_id();
        ed.begin_stroke(brush(), paint([0, 0, 0]), 20.0, 20.0, 1.0)
            .unwrap();
        ed.end_stroke();
        ed.set_layer(id, "Lock", |l| l.alpha_lock = true);
        ed.line(
            BrushParams {
                size: 80.0,
                ..brush()
            },
            paint([255; 3]),
            (20.0, 20.0),
            (25.0, 20.0),
        )
        .unwrap();
        let g = &ed.doc.active_layer().tiles;
        assert_eq!(g.pixel(20, 20), [255, 255, 255, 255]);
        assert_eq!(g.pixel(50, 20), [0; 4]);
        ed.set_layer(id, "Unlock", |l| l.alpha_lock = false);
        let erase = Paint {
            mode: PaintMode::Erase,
            ..paint([0; 3])
        };
        ed.line(
            BrushParams {
                size: 80.0,
                ..brush()
            },
            erase,
            (20.0, 20.0),
            (21.0, 20.0),
        )
        .unwrap();
        assert!(ed.doc.active_layer().tiles.is_empty());
    }

    #[test]
    fn groups_composite_and_ungroup() {
        let mut ed = Editor::new(Document::new(128, 128, Some([255; 3])));
        let a = ed.doc.active_id();
        ed.line(brush(), paint([0, 0, 0]), (10.0, 10.0), (100.0, 10.0))
            .unwrap();
        ed.group_layer(a);
        let g = ed.doc.active_id();
        assert!(ed.doc.find(g).unwrap().is_group());
        assert_eq!(ed.doc.place_of(a).unwrap().parent, Some(g));
        ed.set_layer(g, "Opacity", |l| l.opacity = 0.5);
        ed.update_projection(None, usize::MAX);
        let p = ed.sample(50.0, 10.0).unwrap();
        assert!((p[0] as i32 - 128).abs() <= 1, "{p:?}");
        // Hiding the group hides the child; painting into it is refused.
        ed.set_layer(g, "Hide", |l| l.visible = false);
        ed.select_layer(a);
        assert_eq!(
            ed.begin_stroke(brush(), paint([0; 3]), 1.0, 1.0, 1.0),
            Err(Refusal::LayerHidden)
        );
        ed.set_layer(g, "Show", |l| l.visible = true);
        ed.ungroup(g);
        assert_eq!(ed.doc.layers().len(), 1);
        assert!((ed.doc.layers()[0].opacity - 0.5).abs() < 1e-6);
        ed.update_projection(None, usize::MAX);
        assert_eq!(ed.sample(50.0, 10.0).unwrap(), p);
    }

    #[test]
    fn clipping_paints_inside_base() {
        let mut ed = Editor::new(Document::new(128, 128, None));
        // Base: a short stroke. Clipped layer: paint everywhere.
        ed.line(brush(), paint([0, 0, 255]), (20.0, 20.0), (40.0, 20.0))
            .unwrap();
        ed.add_layer();
        let top = ed.doc.active_id();
        ed.fill_selection([255, 0, 0]).unwrap();
        ed.toggle_clip(top);
        ed.update_projection(None, usize::MAX);
        assert_eq!(ed.sample(30.0, 20.0), Some([255, 0, 0, 255]));
        assert_eq!(ed.sample(100.0, 100.0), Some([0; 4]));
        // Merging bakes the clip.
        assert!(ed.merge_down(top));
        assert_eq!(ed.doc.layers()[0].tiles.pixel(100, 100), [0; 4]);
        assert_eq!(ed.doc.layers()[0].tiles.pixel(30, 20), [255, 0, 0, 255]);
    }

    #[test]
    fn selection_limits_painting_and_moves() {
        let mut ed = Editor::new(Document::new(256, 256, None));
        ed.select_shape(
            Selection::rect(256, 256, 0.0, 0.0, 128.0, 256.0),
            SelectOp::Replace,
        );
        ed.line(
            BrushParams {
                size: 30.0,
                ..brush()
            },
            paint([0, 0, 0]),
            (60.0, 100.0),
            (200.0, 100.0),
        )
        .unwrap();
        let g = ed.doc.active_layer().tiles.clone();
        assert_eq!(g.pixel(100, 100), [0, 0, 0, 255]);
        assert_eq!(g.pixel(150, 100), [0; 4]);
        // Floating the selected pixels moves them and the selection.
        ed.begin_transform(false).unwrap();
        ed.set_transform(Affine::translate(0.0, 50.0), false);
        ed.commit_transform();
        let g = &ed.doc.active_layer().tiles;
        assert_eq!(g.pixel(100, 150), [0, 0, 0, 255]);
        assert_eq!(g.pixel(100, 100), [0; 4]);
        assert_eq!(ed.selection().unwrap().value(10, 255), 255);
        assert_eq!(ed.selection().unwrap().value(10, 10), 0);
        ed.deselect();
        assert!(ed.selection().is_none());
        ed.undo();
        assert!(ed.selection().is_some());
    }

    #[test]
    fn copy_paste_and_crop() {
        let mut ed = Editor::new(Document::new(200, 100, None));
        ed.line(brush(), paint([9, 9, 9]), (50.0, 50.0), (60.0, 50.0))
            .unwrap();
        let c = ed.copy(false).unwrap();
        assert!(c.width >= 20 && c.height >= 20);
        ed.paste("Pasted", &c);
        assert_eq!(ed.doc.layers().len(), 2);
        assert_eq!(ed.doc.active_layer().tiles.pixel(55, 50), [9, 9, 9, 255]);
        ed.crop(40, 30, 100, 90);
        assert_eq!((ed.doc.width, ed.doc.height), (60, 60));
        assert_eq!(ed.doc.active_layer().tiles.pixel(15, 20), [9, 9, 9, 255]);
        ed.update_projection(None, usize::MAX);
        ed.undo();
        assert_eq!((ed.doc.width, ed.doc.height), (200, 100));
        assert_eq!(ed.projection.size(), (200, 100));
        ed.rotate_canvas(1);
        assert_eq!((ed.doc.width, ed.doc.height), (100, 200));
        // (55, 50) → (100 - 1 - 50, 55) = (49, 55)
        assert_eq!(ed.doc.layers()[0].tiles.pixel(49, 55), [9, 9, 9, 255]);
    }

    #[test]
    fn animation_frames() {
        let mut ed = Editor::new(Document::new(128, 128, None));
        ed.line(brush(), paint([1, 2, 3]), (10.0, 10.0), (20.0, 10.0))
            .unwrap();
        ed.add_frame(false);
        assert_eq!((ed.doc.state.frames, ed.doc.state.frame), (2, 1));
        assert!(ed.doc.active_layer().tiles.is_empty());
        ed.line(brush(), paint([4, 5, 6]), (100.0, 100.0), (110.0, 100.0))
            .unwrap();
        ed.set_frame(0);
        assert_eq!(ed.doc.active_layer().tiles.pixel(15, 10), [1, 2, 3, 255]);
        assert_eq!(ed.doc.active_layer().tiles.pixel(105, 100), [0; 4]);
        let f1 = ed.render_frame(1);
        assert_eq!(&f1[(100 * 128 + 105) * 4..][..4], &[4, 5, 6, 255]);
        ed.add_frame(true);
        assert_eq!(ed.doc.state.frames, 3);
        assert_eq!(ed.doc.active_layer().tiles.pixel(15, 10), [1, 2, 3, 255]);
        ed.delete_frame();
        assert_eq!(ed.doc.state.frames, 2);
        ed.set_frame(1);
        assert_eq!(ed.doc.active_layer().tiles.pixel(105, 100), [4, 5, 6, 255]);
        ed.undo();
        assert_eq!(ed.doc.state.frames, 3);
    }

    #[test]
    fn filter_preview_and_commit() {
        let mut ed = Editor::new(Document::new(128, 128, None));
        ed.fill_selection([200, 100, 0]).unwrap();
        ed.begin_filter(Arc::new(crate::adjust::Adjustment::Invert))
            .unwrap();
        ed.update_projection(None, usize::MAX);
        assert_eq!(ed.sample(5.0, 5.0), Some([55, 155, 255, 255]));
        ed.cancel_filter();
        ed.update_projection(None, usize::MAX);
        assert_eq!(ed.sample(5.0, 5.0), Some([200, 100, 0, 255]));
        ed.select_shape(
            Selection::rect(128, 128, 0.0, 0.0, 10.0, 10.0),
            SelectOp::Replace,
        );
        ed.apply_filter("Invert", Arc::new(crate::adjust::Adjustment::Invert))
            .unwrap();
        let g = &ed.doc.active_layer().tiles;
        assert_eq!(g.pixel(5, 5), [55, 155, 255, 255]);
        assert_eq!(g.pixel(50, 50), [200, 100, 0, 255]);
    }

    #[test]
    fn image_object_is_lossless() {
        let mut ed = Editor::new(Document::new(256, 256, None));
        let rgba: Vec<u8> = (0..64 * 64)
            .flat_map(|i| [(i % 64) as u8 * 4, 0, 0, 255])
            .collect();
        let id = ed.place_image(
            "img",
            Source::from_rgba(64, 64, &rgba),
            Some((128.0, 128.0)),
        );
        // Shrink a lot, then grow back: still sharp because the original is kept.
        for s in [0.1, 10.0] {
            ed.begin_transform(true).unwrap();
            ed.set_transform(
                Affine::translate(128.0, 128.0)
                    .compose(Affine::scale(s, s))
                    .compose(Affine::translate(-128.0, -128.0)),
                true,
            );
            ed.commit_transform();
        }
        let l = ed.doc.find(id).unwrap();
        assert_eq!(l.kind(), Kind::Image);
        // Column 40 of the source sits at x = 96 + 40.
        let p = l.tiles.pixel(96 + 40, 128);
        assert!((p[0] as i32 - 160).abs() <= 4, "{p:?}");
        ed.rasterize(id);
        assert_eq!(ed.doc.find(id).unwrap().kind(), Kind::Raster);
    }
}
