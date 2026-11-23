//! The editing API: every user-visible operation, with undo and display
//! invalidation handled in one place. The GUI is a thin shell around this,
//! and everything here runs (and is tested) headless.

use std::sync::Arc;

use crate::blend::blend_tile;
use crate::brush::{BrushParams, Dab, Mask, Paint, PaintMode, StrokeEngine, apply_mask_tile};
use crate::document::{DocState, Document, Layer};
use crate::fill::{FillParams, flood_fill};
use crate::history::{Changes, History, diff_states, snapshot_cost};
use crate::render::{Override, Projection};
use crate::tile::{TileCoord, TileGrid, TileRect};

struct ActiveStroke {
    engine: StrokeEngine,
    mask: Mask,
    paint: Paint,
    layer: usize,
}

struct ActiveMove {
    layer: usize,
    dx: i64,
    dy: i64,
    bounds: TileRect,
}

/// Why an edit could not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    LayerHidden,
    LayerEmpty,
    Busy,
}

pub struct Editor {
    pub doc: Document,
    pub history: History,
    pub projection: Projection,
    state_id: u64,
    next_state_id: u64,
    saved_id: u64,
    stroke: Option<ActiveStroke>,
    moving: Option<ActiveMove>,
    coalesce: Option<String>,
    last_point: Option<(f32, f32)>,
    dabs: Vec<Dab>,
    dirty: Vec<TileCoord>,
}

impl Editor {
    pub fn new(doc: Document) -> Self {
        let projection = Projection::new(doc.width, doc.height);
        Editor {
            doc,
            history: History::default(),
            projection,
            state_id: 0,
            next_state_id: 1,
            saved_id: 0,
            stroke: None,
            moving: None,
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
        self.stroke.is_some() || self.moving.is_some()
    }

    /// Recomposites the display. `visible` is in level-0 tile coordinates.
    /// Returns true if more work is pending.
    pub fn update_projection(&mut self, visible: Option<TileRect>, budget: usize) -> bool {
        let ov = match (&self.stroke, &self.moving) {
            (Some(s), _) => Override::Stroke {
                layer: s.layer,
                mask: &s.mask,
                paint: s.paint,
            },
            (_, Some(m)) => Override::Offset {
                layer: m.layer,
                dx: m.dx,
                dy: m.dy,
            },
            _ => Override::None,
        };
        self.projection.update(&self.doc, ov, visible, budget)
    }

    /// Applies `f` as one undoable step.
    pub fn commit(&mut self, label: &str, f: impl FnOnce(&mut Document)) {
        self.coalesce = None;
        self.commit_inner(label, f);
    }

    fn commit_inner(&mut self, label: &str, f: impl FnOnce(&mut Document)) {
        let before = self.doc.state.clone();
        f(&mut self.doc);
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

    fn invalidate_between(&mut self, other: &DocState) {
        self.projection.reset_below_cache();
        match diff_states(other, &self.doc.state) {
            Changes::All => self.projection.invalidate_all(),
            Changes::Tiles(t) => self.projection.invalidate_many(t),
        }
    }

    pub fn undo(&mut self) -> bool {
        self.cancel_stroke();
        self.cancel_move();
        self.coalesce = None;
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
        self.cancel_stroke();
        self.cancel_move();
        self.coalesce = None;
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

    fn check_paintable(&self) -> Result<(), Refusal> {
        if self.is_busy() {
            return Err(Refusal::Busy);
        }
        if !self.doc.active_layer().visible {
            return Err(Refusal::LayerHidden);
        }
        Ok(())
    }

    pub fn begin_stroke(
        &mut self,
        params: BrushParams,
        paint: Paint,
        x: f32,
        y: f32,
        pressure: f32,
    ) -> Result<(), Refusal> {
        self.check_paintable()?;
        let paint = Paint {
            alpha_lock: self.doc.active_layer().alpha_lock,
            ..paint
        };
        self.stroke = Some(ActiveStroke {
            engine: StrokeEngine::new(params),
            mask: Mask::new(self.doc.width, self.doc.height),
            paint,
            layer: self.doc.active(),
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
        self.apply_mask(label, s.layer, &s.mask, s.paint);
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

    fn apply_mask(&mut self, label: &str, layer: usize, mask: &Mask, paint: Paint) {
        if mask.is_empty() {
            self.projection.reset_below_cache();
            return;
        }
        let touched = mask.touched().to_vec();
        self.commit(label, |doc| {
            let grid = doc.state.layers[layer].tiles_mut();
            for &c in &touched {
                let m = mask.get(c).unwrap();
                if (paint.mode == PaintMode::Erase || paint.alpha_lock) && grid.get(c).is_none() {
                    continue;
                }
                apply_mask_tile(grid.get_mut_or_insert(c), m, &paint);
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
        self.check_paintable()?;
        let paint = Paint {
            alpha_lock: self.doc.active_layer().alpha_lock,
            ..paint
        };
        let (w, h) = (self.doc.width, self.doc.height);
        let mask = if sample_all {
            self.projection.update_all(&self.doc, Override::None);
            let proj = &self.projection;
            flood_fill(w, h, x, y, |x, y| proj.pixel(x, y), p)
        } else {
            let grid = &self.doc.active_layer().tiles;
            flood_fill(w, h, x, y, |x, y| grid.pixel(x as i64, y as i64), p)
        };
        if let Some(mask) = mask {
            self.apply_mask("Fill", self.doc.active(), &mask, paint);
        }
        Ok(())
    }

    // ---- moving -------------------------------------------------------------

    pub fn begin_move(&mut self) -> Result<(), Refusal> {
        self.check_paintable()?;
        let Some(bounds) = self.doc.active_layer().tiles.bounds() else {
            return Err(Refusal::LayerEmpty);
        };
        self.moving = Some(ActiveMove {
            layer: self.doc.active(),
            dx: 0,
            dy: 0,
            bounds,
        });
        Ok(())
    }

    fn move_rect(&self, m: &ActiveMove) -> TileRect {
        let t = crate::tile::TILE as i64;
        let (cols, rows) = self.doc.grid_dims();
        TileRect::from_px(
            m.bounds.x0 as i64 * t + m.dx,
            m.bounds.y0 as i64 * t + m.dy,
            m.bounds.x1 as i64 * t + m.dx,
            m.bounds.y1 as i64 * t + m.dy,
            cols,
            rows,
        )
    }

    pub fn move_to(&mut self, dx: i64, dy: i64) {
        let Some(m) = &self.moving else { return };
        let old = self.move_rect(m);
        let m = self.moving.as_mut().unwrap();
        m.dx = dx;
        m.dy = dy;
        let m = self.moving.as_ref().unwrap();
        let new = self.move_rect(m);
        self.projection.invalidate_rect(old);
        self.projection.invalidate_rect(new);
    }

    pub fn move_offset(&self) -> Option<(i64, i64)> {
        self.moving.as_ref().map(|m| (m.dx, m.dy))
    }

    pub fn end_move(&mut self) {
        let Some(m) = self.moving.take() else { return };
        self.projection.reset_below_cache();
        if m.dx == 0 && m.dy == 0 {
            return;
        }
        let (layer, dx, dy) = (m.layer, m.dx, m.dy);
        self.commit("Move layer", |doc| {
            let l = &mut doc.state.layers[layer];
            l.tiles = Arc::new(l.tiles.translated(dx, dy));
        });
    }

    pub fn cancel_move(&mut self) {
        if let Some(m) = self.moving.take() {
            let r = self.move_rect(&m);
            self.projection.reset_below_cache();
            self.projection.invalidate_rect(r);
            self.projection.invalidate_rect(m.bounds);
        }
    }

    // ---- layers -------------------------------------------------------------

    pub fn select_layer(&mut self, i: usize) {
        if i < self.doc.layers().len() && !self.is_busy() {
            self.doc.state.active = i;
        }
    }

    pub fn add_layer(&mut self) {
        self.commit("New layer", |doc| {
            let l = doc.new_layer(doc.next_layer_name());
            let at = doc.state.active + 1;
            doc.state.layers.insert(at, l);
            doc.state.active = at;
        });
    }

    /// Inserts an already built layer above the active one.
    pub fn insert_layer(&mut self, label: &str, mut layer: Layer) {
        self.commit(label, |doc| {
            layer.id = doc.fresh_id();
            let at = (doc.state.active + 1).min(doc.state.layers.len());
            doc.state.layers.insert(at, layer);
            doc.state.active = at;
        });
    }

    pub fn delete_layer(&mut self) -> bool {
        if self.doc.layers().len() <= 1 {
            return false;
        }
        self.commit("Delete layer", |doc| {
            let a = doc.state.active;
            doc.state.layers.remove(a);
            doc.state.active = a.saturating_sub(1).min(doc.state.layers.len() - 1);
        });
        true
    }

    pub fn duplicate_layer(&mut self) {
        self.commit("Duplicate layer", |doc| {
            let a = doc.state.active;
            let mut l = doc.state.layers[a].clone();
            l.id = doc.fresh_id();
            l.name = format!("{} copy", l.name);
            doc.state.layers.insert(a + 1, l);
            doc.state.active = a + 1;
        });
    }

    pub fn can_merge_down(&self) -> bool {
        let a = self.doc.active();
        a > 0 && self.doc.layers()[a].visible && self.doc.layers()[a - 1].visible
    }

    pub fn merge_down(&mut self) -> bool {
        if !self.can_merge_down() {
            return false;
        }
        self.commit("Merge down", |doc| {
            let a = doc.state.active;
            let upper = doc.state.layers.remove(a);
            let lower = &mut doc.state.layers[a - 1];
            let (op, mode) = (upper.opacity_u8(), upper.blend);
            let grid = lower.tiles_mut();
            for (c, t) in upper.tiles.iter() {
                if grid.get(c).is_none() && op == 255 && mode == crate::blend::BlendMode::Normal {
                    grid.set(c, Some(t.clone()));
                } else {
                    blend_tile(grid.get_mut_or_insert(c), t, op, mode);
                }
            }
            doc.state.active = a - 1;
        });
        true
    }

    /// Moves the layer at `from` to index `to`, keeping it selected.
    pub fn move_layer(&mut self, from: usize, to: usize) {
        let n = self.doc.layers().len();
        if from == to || from >= n || to >= n {
            return;
        }
        self.commit("Reorder layers", |doc| {
            let l = doc.state.layers.remove(from);
            doc.state.layers.insert(to, l);
            doc.state.active = to;
        });
    }

    pub fn clear_layer(&mut self) {
        if self.doc.active_layer().tiles.is_empty() {
            return;
        }
        self.commit("Clear layer", |doc| {
            let (w, h) = (doc.width, doc.height);
            doc.active_layer_mut().tiles = Arc::new(TileGrid::new(w, h));
        });
    }

    pub fn set_paper(&mut self, paper: Option<[u8; 3]>) {
        if self.doc.state.paper != paper {
            self.commit_coalesced("Paper colour", |doc| doc.state.paper = paper);
        }
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

fn empty_state() -> DocState {
    DocState {
        layers: vec![],
        active: 0,
        paper: None,
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
        assert_eq!(ed.doc.active(), 1);
        ed.commit("Opacity", |d| d.active_layer_mut().opacity = 0.5);
        ed.begin_stroke(brush(), paint([255, 0, 0]), 10.0, 10.0, 1.0)
            .unwrap();
        ed.end_stroke();
        ed.update_projection(None, usize::MAX);
        let before = ed.sample(10.0, 10.0).unwrap();
        assert!(ed.merge_down());
        assert_eq!(ed.doc.layers().len(), 1);
        ed.update_projection(None, usize::MAX);
        assert_eq!(ed.sample(10.0, 10.0).unwrap(), before);
        ed.undo();
        assert_eq!(ed.doc.layers().len(), 2);
        ed.duplicate_layer();
        assert_eq!(ed.doc.layers()[2].name, "Layer 2 copy");
        ed.move_layer(2, 0);
        assert_eq!(ed.doc.active(), 0);
        assert!(ed.delete_layer());
        assert_eq!(ed.doc.layers().len(), 2);
    }

    #[test]
    fn coalesced_edits_are_one_step() {
        let mut ed = Editor::new(Document::new(64, 64, None));
        for i in 0..10 {
            ed.commit_coalesced("Layer opacity", |d| {
                d.active_layer_mut().opacity = i as f32 / 10.0
            });
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
        ed.begin_move().unwrap();
        ed.move_to(100, 7);
        ed.update_projection(None, usize::MAX);
        assert_eq!(ed.sample(130.0, 37.0), Some([0, 255, 0, 255]));
        assert_eq!(ed.sample(30.0, 30.0), Some([0; 4]));
        ed.end_move();
        ed.update_projection(None, usize::MAX);
        assert_eq!(ed.sample(130.0, 37.0), Some([0, 255, 0, 255]));
        assert_eq!(ed.doc.active_layer().tiles.pixel(130, 37), [0, 255, 0, 255]);
    }

    #[test]
    fn fill_sample_all() {
        let mut ed = Editor::new(Document::new(128, 128, Some([255; 3])));
        // Line art on the top layer: a vertical wall at x=64.
        ed.add_layer();
        ed.line(brush(), paint([0; 3]), (64.0, -10.0), (64.0, 140.0))
            .unwrap();
        ed.select_layer(0);
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
        ed.begin_stroke(brush(), paint([0, 0, 0]), 20.0, 20.0, 1.0)
            .unwrap();
        ed.end_stroke();
        ed.commit("Lock", |d| d.active_layer_mut().alpha_lock = true);
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
        ed.commit("Unlock", |d| d.active_layer_mut().alpha_lock = false);
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
}
