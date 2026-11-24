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

use std::borrow::Cow;
use std::collections::HashMap;

use rayon::prelude::*;

use crate::blend::{BlendMode, blend_tile_at};
use crate::document::{Document, Layer, LayerId};
use crate::tile::{TILE, Tile, TileCoord, TileRect, tiles_for};

/// A temporary modification of the document that is shown but not yet
/// committed: a stroke in progress, a layer being moved or transformed, an
/// adjustment being previewed, onion skins.
pub trait Ov: Sync {
    /// The layers being modified, if any. Everything below the lowest of
    /// their root-level ancestors is cached while the override lasts. `tile`
    /// and `under` are only consulted for these layers.
    fn layers(&self) -> &[LayerId];
    /// Replacement pixels for one tile of `l`: `None` if `l` isn't
    /// overridden, `Some(None)` if the tile becomes empty.
    fn tile(&self, l: &Layer, c: TileCoord) -> Option<Option<Tile>>;
    /// Draws anything that should appear directly beneath `l` (onion skins).
    fn under(&self, _l: &Layer, _c: TileCoord, _out: &mut Tile) {}
}

/// No modification.
pub struct NoOverride;

impl Ov for NoOverride {
    fn layers(&self) -> &[LayerId] {
        &[]
    }
    fn tile(&self, _: &Layer, _: TileCoord) -> Option<Option<Tile>> {
        None
    }
}

/// The pixels a layer contributes to one tile, before its own opacity and
/// blend mode: its tiles, or for a group its children composited in
/// isolation.
pub fn layer_content<'a>(l: &'a Layer, c: TileCoord, ov: &dyn Ov) -> Option<Cow<'a, Tile>> {
    content(l, c, ov, ov.layers())
}

fn content<'a>(
    l: &'a Layer,
    c: TileCoord,
    ov: &dyn Ov,
    target: &[LayerId],
) -> Option<Cow<'a, Tile>> {
    if target.contains(&l.id)
        && let Some(t) = ov.tile(l, c)
    {
        return t.map(Cow::Owned);
    }
    match &l.content {
        crate::document::Content::Group { children, .. } => {
            if !children.iter().any(|ch| ch.is_shown()) {
                return None;
            }
            let mut t = Tile::filled([0; 4]);
            list(children, c, &mut t, ov, target);
            if t.is_empty() {
                None
            } else {
                Some(Cow::Owned(t))
            }
        }
        _ => l.tiles.get(c).map(|t| Cow::Borrowed(&**t)),
    }
}

/// Composites a list of sibling layers (bottom first) onto `out`.
///
/// A run of clipped layers paints inside the alpha of the unclipped layer
/// beneath it (its base), as if the base were opaque; the base's alpha,
/// opacity and blend mode then apply to the result, as in Photoshop.
pub fn compose_list(layers: &[Layer], c: TileCoord, out: &mut Tile, ov: &dyn Ov) {
    list(layers, c, out, ov, ov.layers());
}

fn list(layers: &[Layer], c: TileCoord, out: &mut Tile, ov: &dyn Ov, target: &[LayerId]) {
    let n = layers.len();
    let mut i = 0;
    while i < n {
        let base = &layers[i];
        let mut j = i + 1;
        while j < n && layers[j].clip {
            j += 1;
        }
        if base.is_shown() {
            if target.contains(&base.id) {
                ov.under(base, c, out);
            }
            let clips = &layers[i + 1..j];
            let clipped = clips.iter().any(|l| l.is_shown());
            if !clipped && pass_through(base, c, out, ov, target) {
                i = j;
                continue;
            }
            if let Some(t) = content(base, c, ov, target) {
                if clipped {
                    let mut t = t.into_owned();
                    apply_clips(&mut t, clips, c, ov, target);
                    blend_tile_at(out, &t, base.opacity_u8(), base.blend, c);
                } else {
                    blend_tile_at(out, &t, base.opacity_u8(), base.blend, c);
                }
            }
        }
        i = j;
    }
}

/// Composites a Pass Through group's children straight onto `out`, with
/// the group's opacity fading between before and after. Returns false
/// (doing nothing) if `l` isn't such a group or is being replaced by an
/// override, so the caller composites it in isolation instead. A group
/// with layers clipped to it is always isolated, since they clip to its
/// combined content.
fn pass_through(l: &Layer, c: TileCoord, out: &mut Tile, ov: &dyn Ov, target: &[LayerId]) -> bool {
    let crate::document::Content::Group { children, .. } = &l.content else {
        return false;
    };
    if l.blend != BlendMode::PassThrough || (target.contains(&l.id) && ov.tile(l, c).is_some()) {
        return false;
    }
    let op = l.opacity_u8();
    if op == 0 {
        return true;
    }
    if op == 255 {
        list(children, c, out, ov, target);
        return true;
    }
    let mut after = out.clone();
    list(children, c, &mut after, ov, target);
    let (o, inv) = (op as u32, 255 - op as u32);
    for (d, a) in out.px.iter_mut().zip(after.px.iter()) {
        for ch in 0..4 {
            d[ch] = crate::blend::div255(a[ch] as u32 * o + d[ch] as u32 * inv) as u8;
        }
    }
    true
}

fn apply_clips(base: &mut Tile, clips: &[Layer], c: TileCoord, ov: &dyn Ov, target: &[LayerId]) {
    // Remember the base's alpha and make it opaque.
    let mut alpha = [0u8; crate::tile::TILE_PX];
    for (a, p) in alpha.iter_mut().zip(base.px.iter_mut()) {
        *a = p[3];
        *p = match p[3] {
            0 => [0; 4],
            255 => *p,
            _ => {
                let u = crate::color::unpremultiply(*p);
                [u[0], u[1], u[2], 255]
            }
        };
    }
    for l in clips {
        if !l.is_shown() {
            continue;
        }
        if target.contains(&l.id) {
            ov.under(l, c, base);
        }
        if let Some(t) = content(l, c, ov, target) {
            blend_tile_at(base, &t, l.opacity_u8(), l.blend, c);
        }
    }
    for (a, p) in alpha.iter().zip(base.px.iter_mut()) {
        *p = match *a {
            0 => [0; 4],
            255 => *p,
            a => crate::color::premultiply([p[0], p[1], p[2], a]),
        };
    }
}

/// Index of the root-level layer whose clipping stack contains `id`
/// (the stack's base).
fn root_unit_start(doc: &Document, id: LayerId) -> Option<usize> {
    let root = doc.ancestors(id).first().copied().unwrap_or(id);
    let layers = doc.layers();
    let mut i = layers.iter().position(|l| l.id == root)?;
    while i > 0 && layers[i].clip {
        i -= 1;
    }
    Some(i)
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
    below_key: Option<(Vec<LayerId>, usize)>,
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
            below_key: None,
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
        self.below_key = None;
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
        ov: &dyn Ov,
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
    pub fn update_all(&mut self, doc: &Document, ov: &dyn Ov) {
        self.update(doc, ov, None, usize::MAX);
    }

    fn compose(&mut self, doc: &Document, ov: &dyn Ov, todo: &[TileCoord]) {
        let layers = doc.layers();
        let ids = ov.layers();
        let start = ids.iter().filter_map(|id| root_unit_start(doc, *id)).min();
        // Fill the below-the-edit cache for tiles that lack it.
        if let Some(start) = start {
            if self
                .below_key
                .as_ref()
                .is_none_or(|(k, s)| k.as_slice() != ids || *s != start)
            {
                self.below.clear();
                self.below_key = Some((ids.to_vec(), start));
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
                    compose_list(&layers[..start], c, &mut t, &NoOverride);
                    (c, t)
                })
                .collect();
            self.below.extend(built);
        }

        let below = &self.below;
        let results: Vec<(TileCoord, Option<Box<Tile>>)> = todo
            .par_iter()
            .map(|&c| {
                let (mut out, from) = match start {
                    Some(s) => (below[&c].clone(), s),
                    None => (Box::new(Tile::filled(doc.paper_px())), 0),
                };
                compose_list(&layers[from..], c, &mut out, ov);
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

    #[test]
    fn stroke_preview_matches_commit() {
        use crate::brush::{BrushParams, Paint, PaintMode};
        let mut doc = Document::new(300, 200, Some([255, 255, 255]));
        let top = doc.new_layer("top");
        doc.state.layers.push(top);
        doc.state.layers[1].opacity = 0.5;
        let bottom = doc.state.layers[0].id;
        let mut ed = crate::editor::Editor::new(doc);
        ed.select_layer(bottom);
        ed.update_projection(None, usize::MAX);
        assert_eq!(ed.projection.pixel(10, 10), [255, 255, 255, 255]);
        let p = Paint {
            color: [0, 0, 0],
            mode: PaintMode::Paint,
            alpha_lock: false,
        };
        let b = BrushParams {
            size: 80.0,
            hardness: 0.5,
            pressure_size: false,
            smoothing: 0.0,
            ..Default::default()
        };
        ed.begin_stroke(b, p, 100.0, 100.0, 1.0).unwrap();
        ed.update_projection(None, usize::MAX);
        let preview: Vec<_> = (0..200).map(|x| ed.projection.pixel(x, 100)).collect();
        ed.end_stroke();
        ed.invalidate_all();
        ed.update_projection(None, usize::MAX);
        let committed: Vec<_> = (0..200).map(|x| ed.projection.pixel(x, 100)).collect();
        assert_eq!(preview, committed);
        assert_eq!(ed.projection.pixel(100, 100), [0, 0, 0, 255]);
        assert!(!ed.projection.has_pending());
    }

    #[test]
    fn pass_through_groups_blend_children_with_what_is_below() {
        use crate::blend::BlendMode;
        use crate::document::Content;
        let mut doc = Document::new(64, 64, Some([200, 200, 200]));
        let mut child = doc.new_layer("multiply");
        child.blend = BlendMode::Multiply;
        *child.tiles_mut().get_mut_or_insert(TileCoord::new(0, 0)) =
            Tile::filled([100, 100, 100, 255]);
        let mut group = doc.new_layer("group");
        group.content = Content::Group {
            children: vec![child],
            expanded: true,
        };
        doc.state.layers = vec![group];
        let px = |doc: &Document| {
            let mut p = Projection::new(64, 64);
            p.update_all(doc, &NoOverride);
            p.pixel(5, 5)
        };
        // Isolated: the multiply has nothing under it inside the group.
        assert_eq!(px(&doc)[0], 100);
        doc.state.layers[0].blend = BlendMode::PassThrough;
        // Pass through: it multiplies the paper.
        assert_eq!(px(&doc)[0], 78);
        doc.state.layers[0].opacity = 0.5;
        let half = px(&doc)[0];
        assert!((138..=140).contains(&half), "{half}");
    }

    #[test]
    fn pyramid_averages() {
        let mut doc = Document::new(256, 256, None);
        let g = doc.state.layers[0].tiles_mut();
        *g.get_mut_or_insert(TileCoord::new(0, 0)) = Tile::filled([200, 100, 0, 200]);
        let mut proj = Projection::new(256, 256);
        proj.update_all(&doc, &NoOverride);
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
        assert!(proj.update(&doc, &NoOverride, Some(vis), 0));
        assert_eq!(proj.pixel(10, 10), [1, 2, 3, 255]);
        assert_eq!(proj.pixel(1000, 1000), [0; 4]);
        proj.update_all(&doc, &NoOverride);
        assert_eq!(proj.pixel(1000, 1000), [1, 2, 3, 255]);
    }
}
