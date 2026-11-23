//! The projection: a cached composite of the document plus a mip pyramid.
//!
//! Only tiles marked dirty are recomposited, tiles in the viewport go first,
//! and work is spread over all cores. While a stroke or move is in progress
//! the composite of everything *below* the active layer is cached per tile,
//! so painting on layer 1 of 300 costs about the same as painting on a
//! one-layer document.
//!
//! Each pyramid level halves the previous one. The display uses the level
//! nearest the zoom, so a zoomed-out view of a huge canvas never touches the
//! GPU with more than a screenful of pixels, and its downsampling is a correct
//! box filter rather than GPU aliasing.

use std::collections::HashMap;

use rayon::prelude::*;

use crate::blend::blend_tile;
use crate::brush::{Mask, Paint, apply_mask_tile};
use crate::document::Document;
use crate::tile::{TILE, Tile, TileCoord, TileRect, tiles_for};

/// A temporary modification of one layer that is shown but not yet committed.
#[derive(Copy, Clone)]
pub enum Override<'a> {
    None,
    Stroke {
        layer: usize,
        mask: &'a Mask,
        paint: Paint,
    },
    Offset {
        layer: usize,
        dx: i64,
        dy: i64,
    },
}

impl Override<'_> {
    fn layer(&self) -> Option<usize> {
        match *self {
            Override::None => None,
            Override::Stroke { layer, .. } | Override::Offset { layer, .. } => Some(layer),
        }
    }
}

struct Level {
    cols: u32,
    rows: u32,
    tiles: Vec<Option<Box<Tile>>>,
    dirty: Vec<bool>,
}

impl Level {
    fn new(cols: u32, rows: u32) -> Self {
        let n = (cols * rows) as usize;
        Level {
            cols,
            rows,
            tiles: (0..n).map(|_| None).collect(),
            dirty: vec![true; n],
        }
    }
    fn idx(&self, c: TileCoord) -> usize {
        (c.y * self.cols + c.x) as usize
    }
}

pub struct Projection {
    width: u32,
    height: u32,
    levels: Vec<Level>,
    below: HashMap<TileCoord, Box<Tile>>,
    below_layer: Option<usize>,
    changed: Vec<(u8, TileCoord)>,
}

impl Projection {
    pub fn new(width: u32, height: u32) -> Self {
        let mut levels = vec![];
        let mut l = 0;
        loop {
            let (w, h) = (width.div_ceil(1 << l), height.div_ceil(1 << l));
            levels.push(Level::new(tiles_for(w), tiles_for(h)));
            if w <= TILE as u32 && h <= TILE as u32 {
                break;
            }
            l += 1;
        }
        Projection {
            width,
            height,
            levels,
            below: HashMap::new(),
            below_layer: None,
            changed: vec![],
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn level_count(&self) -> usize {
        self.levels.len()
    }

    /// Grid size of a pyramid level.
    pub fn level_dims(&self, level: usize) -> (u32, u32) {
        (self.levels[level].cols, self.levels[level].rows)
    }

    /// A composited tile, or `None` if it is fully transparent.
    pub fn tile(&self, level: usize, c: TileCoord) -> Option<&Tile> {
        let l = &self.levels[level];
        if c.x >= l.cols || c.y >= l.rows {
            return None;
        }
        l.tiles[l.idx(c)].as_deref()
    }

    /// Reads one composited pixel (premultiplied).
    pub fn pixel(&self, x: u32, y: u32) -> crate::tile::Px {
        let t = TILE as u32;
        self.tile(0, TileCoord::new(x / t, y / t))
            .map_or([0; 4], |tile| tile.get((x % t) as usize, (y % t) as usize))
    }

    /// Takes the list of tiles whose pixels changed since the last call.
    pub fn take_changed(&mut self) -> Vec<(u8, TileCoord)> {
        std::mem::take(&mut self.changed)
    }

    pub fn invalidate(&mut self, c: TileCoord) {
        let l = &mut self.levels[0];
        if c.x < l.cols && c.y < l.rows {
            let i = l.idx(c);
            l.dirty[i] = true;
        }
    }

    pub fn invalidate_many(&mut self, cs: impl IntoIterator<Item = TileCoord>) {
        for c in cs {
            self.invalidate(c);
        }
    }

    pub fn invalidate_rect(&mut self, r: TileRect) {
        self.invalidate_many(r.iter());
    }

    pub fn invalidate_all(&mut self) {
        self.levels[0].dirty.fill(true);
        self.below.clear();
    }

    /// Drops the below-active cache. Call when a stroke or move ends, or
    /// whenever layers beneath the active one may have changed.
    pub fn reset_below_cache(&mut self) {
        self.below.clear();
        self.below_layer = None;
    }

    /// Whether any composite work is still pending.
    pub fn has_pending(&self) -> bool {
        self.levels.iter().any(|l| l.dirty.iter().any(|d| *d))
    }

    /// Recomposites dirty tiles. Every dirty tile inside `visible` (level-0
    /// tile coordinates) is processed, plus at most `budget` others. Returns
    /// true if work remains.
    pub fn update(
        &mut self,
        doc: &Document,
        ov: Override,
        visible: Option<TileRect>,
        budget: usize,
    ) -> bool {
        let l0 = &self.levels[0];
        let mut todo: Vec<TileCoord> = vec![];
        if let Some(v) = visible {
            todo.extend(
                v.iter()
                    .filter(|c| c.x < l0.cols && c.y < l0.rows && l0.dirty[l0.idx(*c)]),
            );
        }
        let mut extra = 0;
        for (i, d) in l0.dirty.iter().enumerate() {
            if extra >= budget {
                break;
            }
            let c = TileCoord::new(i as u32 % l0.cols, i as u32 / l0.cols);
            if *d && !visible.is_some_and(|v| v.contains(c)) {
                todo.push(c);
                extra += 1;
            }
        }
        if !todo.is_empty() {
            self.compose(doc, ov, &todo);
        }
        self.update_pyramid();
        self.levels[0].dirty.iter().any(|d| *d)
    }

    /// Processes all pending work.
    pub fn update_all(&mut self, doc: &Document, ov: Override) {
        self.update(doc, ov, None, usize::MAX);
    }

    fn compose(&mut self, doc: &Document, ov: Override, todo: &[TileCoord]) {
        let layers = doc.layers();
        // Fill the below-active cache for tiles that lack it.
        if let Some(active) = ov.layer() {
            if self.below_layer != Some(active) {
                self.below.clear();
                self.below_layer = Some(active);
            }
            let missing: Vec<TileCoord> = todo
                .iter()
                .copied()
                .filter(|c| !self.below.contains_key(c))
                .collect();
            let built: Vec<(TileCoord, Box<Tile>)> = missing
                .par_iter()
                .map(|&c| {
                    let mut t = Box::new(Tile::filled(doc.paper_px()));
                    for l in &layers[..active] {
                        if l.is_shown()
                            && let Some(src) = l.tiles.get(c)
                        {
                            blend_tile(&mut t, src, l.opacity_u8(), l.blend);
                        }
                    }
                    (c, t)
                })
                .collect();
            self.below.extend(built);
        }

        let below = &self.below;
        let results: Vec<(TileCoord, Option<Box<Tile>>)> = todo
            .par_iter()
            .map(|&c| {
                let (mut out, start) = match ov.layer() {
                    Some(a) => (below[&c].clone(), a),
                    None => (Box::new(Tile::filled(doc.paper_px())), 0),
                };
                for (i, l) in layers.iter().enumerate().skip(start) {
                    if !l.is_shown() {
                        continue;
                    }
                    let (op, mode) = (l.opacity_u8(), l.blend);
                    match ov {
                        Override::Stroke { layer, mask, paint } if layer == i => {
                            if let Some(m) = mask.get(c) {
                                let mut t = l
                                    .tiles
                                    .get(c)
                                    .map_or_else(|| Tile::filled([0; 4]), |t| (**t).clone());
                                apply_mask_tile(&mut t, m, &paint);
                                blend_tile(&mut out, &t, op, mode);
                                continue;
                            }
                        }
                        Override::Offset { layer, dx, dy } if layer == i => {
                            if let Some(t) = l.tiles.shifted_tile(c, dx, dy) {
                                blend_tile(&mut out, &t, op, mode);
                            }
                            continue;
                        }
                        _ => {}
                    }
                    if let Some(t) = l.tiles.get(c) {
                        blend_tile(&mut out, t, op, mode);
                    }
                }
                let out = if doc.state.paper.is_none() && out.is_empty() {
                    None
                } else {
                    Some(out)
                };
                (c, out)
            })
            .collect();

        for (c, t) in results {
            let l0 = &mut self.levels[0];
            let i = l0.idx(c);
            l0.tiles[i] = t;
            l0.dirty[i] = false;
            self.changed.push((0, c));
            if self.levels.len() > 1 {
                let l1 = &mut self.levels[1];
                let p = TileCoord::new(c.x / 2, c.y / 2);
                let j = l1.idx(p);
                l1.dirty[j] = true;
            }
        }
    }

    fn update_pyramid(&mut self) {
        for lv in 1..self.levels.len() {
            let (lo, hi) = self.levels.split_at_mut(lv);
            let child = &lo[lv - 1];
            let cur = &mut hi[0];
            let todo: Vec<TileCoord> = cur
                .dirty
                .iter()
                .enumerate()
                .filter(|(_, d)| **d)
                .map(|(i, _)| TileCoord::new(i as u32 % cur.cols, i as u32 / cur.cols))
                .collect();
            if todo.is_empty() {
                continue;
            }
            // A parent can only be finished once none of its children are pending.
            let child_dirty = |c: TileCoord, dx: u32, dy: u32| {
                let (x, y) = (c.x * 2 + dx, c.y * 2 + dy);
                x < child.cols && y < child.rows && child.dirty[child.idx(TileCoord::new(x, y))]
            };
            let ready: Vec<TileCoord> = todo
                .into_iter()
                .filter(|&c| !(0..2).any(|dy| (0..2).any(|dx| child_dirty(c, dx, dy))))
                .collect();
            let results: Vec<(TileCoord, Option<Box<Tile>>)> = ready
                .par_iter()
                .map(|&c| (c, downsample(child, c)))
                .collect();
            for (c, t) in results {
                let i = cur.idx(c);
                cur.tiles[i] = t;
                cur.dirty[i] = false;
                self.changed.push((lv as u8, c));
            }
            // Mark the parents of everything just processed.
            if let Some(next) = self.levels.get_mut(lv + 1) {
                for c in &ready {
                    let j = next.idx(TileCoord::new(c.x / 2, c.y / 2));
                    next.dirty[j] = true;
                }
            }
        }
    }
}

/// Box-filters four child tiles into one parent tile.
fn downsample(child: &Level, c: TileCoord) -> Option<Box<Tile>> {
    let get = |dx: u32, dy: u32| {
        let (x, y) = (c.x * 2 + dx, c.y * 2 + dy);
        if x < child.cols && y < child.rows {
            child.tiles[child.idx(TileCoord::new(x, y))].as_deref()
        } else {
            None
        }
    };
    let kids = [get(0, 0), get(1, 0), get(0, 1), get(1, 1)];
    if kids.iter().all(|k| k.is_none()) {
        return None;
    }
    let mut out = Tile::new_boxed();
    const H: usize = TILE / 2;
    for (q, kid) in kids.iter().enumerate() {
        let Some(k) = kid else { continue };
        let (ox, oy) = ((q % 2) * H, (q / 2) * H);
        for y in 0..H {
            let r0 = &k.px[(2 * y) * TILE..(2 * y + 1) * TILE];
            let r1 = &k.px[(2 * y + 1) * TILE..(2 * y + 2) * TILE];
            let dst = &mut out.px[(oy + y) * TILE + ox..(oy + y) * TILE + ox + H];
            for (x, d) in dst.iter_mut().enumerate() {
                let (a, b, cc, e) = (r0[2 * x], r0[2 * x + 1], r1[2 * x], r1[2 * x + 1]);
                for ch in 0..4 {
                    d[ch] = ((a[ch] as u32 + b[ch] as u32 + cc[ch] as u32 + e[ch] as u32 + 2) >> 2)
                        as u8;
                }
            }
        }
    }
    Some(out)
}

/// Flattens the document into straight-alpha RGBA rows.
pub fn flatten(doc: &Document) -> Vec<u8> {
    let (w, h) = (doc.width as usize, doc.height as usize);
    let (cols, rows) = doc.grid_dims();
    let mut out = vec![0u8; w * h * 4];
    out.par_chunks_mut(w * 4 * TILE)
        .enumerate()
        .for_each(|(ty, band)| {
            let mut tile = Tile::filled([0; 4]);
            for tx in 0..cols {
                let _ = rows;
                doc.compose_tile(TileCoord::new(tx, ty as u32), &mut tile);
                let x0 = tx as usize * TILE;
                let cw = TILE.min(w - x0);
                for y in 0..band.len() / (w * 4) {
                    for x in 0..cw {
                        let p = crate::color::unpremultiply(tile.px[y * TILE + x]);
                        let o = (y * w + x0 + x) * 4;
                        band[o..o + 4].copy_from_slice(&p);
                    }
                }
            }
        });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::{Dab, PaintMode};

    #[test]
    fn stroke_preview_matches_commit() {
        let mut doc = Document::new(300, 200, Some([255, 255, 255]));
        let top = doc.new_layer("top");
        doc.state.layers.push(top);
        doc.state.layers[1].opacity = 0.5;
        let mut proj = Projection::new(300, 200);
        proj.update_all(&doc, Override::None);
        assert_eq!(proj.pixel(10, 10), [255, 255, 255, 255]);

        let mut mask = Mask::new(300, 200);
        let mut dirty = vec![];
        mask.stamp(
            &Dab {
                x: 100.0,
                y: 100.0,
                radius: 40.0,
                hardness: 0.5,
                flow: 1.0,
                cap: 1.0,
            },
            &mut dirty,
        );
        let paint = Paint {
            color: [0, 0, 0],
            mode: PaintMode::Paint,
            alpha_lock: false,
        };
        proj.invalidate_many(dirty.iter().copied());
        proj.update_all(
            &doc,
            Override::Stroke {
                layer: 0,
                mask: &mask,
                paint,
            },
        );
        let preview: Vec<_> = (0..200).map(|x| proj.pixel(x, 100)).collect();

        // Commit and recomposite without override.
        let grid = doc.state.layers[0].tiles_mut();
        for &c in mask.touched() {
            apply_mask_tile(grid.get_mut_or_insert(c), mask.get(c).unwrap(), &paint);
        }
        proj.reset_below_cache();
        proj.invalidate_all();
        proj.update_all(&doc, Override::None);
        let committed: Vec<_> = (0..200).map(|x| proj.pixel(x, 100)).collect();
        assert_eq!(preview, committed);
        assert_eq!(proj.pixel(100, 100), [0, 0, 0, 255]);
        assert!(!proj.has_pending());
    }

    #[test]
    fn pyramid_averages() {
        let mut doc = Document::new(256, 256, None);
        let g = doc.state.layers[0].tiles_mut();
        *g.get_mut_or_insert(TileCoord::new(0, 0)) = Tile::filled([200, 100, 0, 200]);
        let mut proj = Projection::new(256, 256);
        proj.update_all(&doc, Override::None);
        assert_eq!(proj.level_count(), 3);
        // Level 1 tile 0 holds the 64px tile in its top-left quadrant.
        let t = proj.tile(1, TileCoord::new(0, 0)).unwrap();
        assert_eq!(t.get(5, 5), [200, 100, 0, 200]);
        assert_eq!(t.get(40, 5), [0; 4]);
        let t2 = proj.tile(2, TileCoord::new(0, 0)).unwrap();
        assert_eq!(t2.get(3, 3), [200, 100, 0, 200]);
    }

    #[test]
    fn visible_first_budget() {
        let doc = Document::new(1024, 1024, Some([1, 2, 3]));
        let mut proj = Projection::new(1024, 1024);
        let vis = TileRect {
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 2,
        };
        assert!(proj.update(&doc, Override::None, Some(vis), 0));
        assert_eq!(proj.pixel(10, 10), [1, 2, 3, 255]);
        assert_eq!(proj.pixel(1000, 1000), [0; 4]);
        proj.update_all(&doc, Override::None);
        assert_eq!(proj.pixel(1000, 1000), [1, 2, 3, 255]);
    }
}
