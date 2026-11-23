//! Selections.
//!
//! A selection is a sparse 8-bit coverage mask over the canvas, stored in the
//! same 64×64 tiles as layers so it costs nothing where nothing is selected.
//! Partial coverage gives soft (antialiased or feathered) edges. It lives in
//! the undoable document state, so selecting is undoable like any edit, as
//! in Photoshop.

use std::sync::Arc;

use rayon::prelude::*;

use crate::brush::Mask;
use crate::tile::{TILE, TILE_PX, TileCoord, TileRect, tiles_for};

#[derive(Clone)]
pub struct SelTile {
    pub v: [u8; TILE_PX],
}

impl SelTile {
    fn full() -> Self {
        SelTile { v: [255; TILE_PX] }
    }
    fn is_full(&self) -> bool {
        self.v.iter().all(|v| *v == 255)
    }
    fn is_empty(&self) -> bool {
        self.v.iter().all(|v| *v == 0)
    }
}

/// How a new shape combines with the existing selection.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SelectOp {
    #[default]
    Replace,
    Add,
    Subtract,
    Intersect,
}

#[derive(Clone)]
pub struct Selection {
    width: u32,
    height: u32,
    cols: u32,
    rows: u32,
    tiles: Vec<Option<Arc<SelTile>>>,
}

impl Selection {
    pub fn empty(width: u32, height: u32) -> Self {
        let (cols, rows) = (tiles_for(width), tiles_for(height));
        Selection {
            width,
            height,
            cols,
            rows,
            tiles: vec![None; (cols * rows) as usize],
        }
    }

    pub fn all(width: u32, height: u32) -> Self {
        let mut s = Selection::empty(width, height);
        let full = Arc::new(SelTile::full());
        for t in &mut s.tiles {
            *t = Some(full.clone());
        }
        s.clip_to_canvas();
        s
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    #[inline]
    pub fn tile(&self, c: TileCoord) -> Option<&SelTile> {
        if c.x >= self.cols || c.y >= self.rows {
            return None;
        }
        self.tiles[(c.y * self.cols + c.x) as usize].as_deref()
    }

    /// Coverage at a pixel, 0..=255.
    pub fn value(&self, x: i64, y: i64) -> u8 {
        if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 {
            return 0;
        }
        let t = TILE as i64;
        self.tile(TileCoord::new((x / t) as u32, (y / t) as u32))
            .map_or(0, |s| s.v[((y % t) * t + x % t) as usize])
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.iter().all(|t| t.is_none())
    }

    /// Tiles that hold any coverage.
    pub fn coords(&self) -> impl Iterator<Item = TileCoord> + '_ {
        let cols = self.cols;
        self.tiles
            .iter()
            .enumerate()
            .filter(|(_, t)| t.is_some())
            .map(move |(i, _)| TileCoord::new(i as u32 % cols, i as u32 / cols))
    }

    /// Pixel bounds `(x0, y0, x1, y1)` of the selected area (exclusive end).
    pub fn bounds(&self) -> Option<(u32, u32, u32, u32)> {
        let mut b: Option<(u32, u32, u32, u32)> = None;
        for c in self.coords() {
            let t = self.tile(c).unwrap();
            let (ox, oy) = (c.x * TILE as u32, c.y * TILE as u32);
            // Exact bounds within the tile.
            let (mut x0, mut y0, mut x1, mut y1) = (TILE, TILE, 0, 0);
            for y in 0..TILE {
                for x in 0..TILE {
                    if t.v[y * TILE + x] != 0 {
                        x0 = x0.min(x);
                        x1 = x1.max(x + 1);
                        y0 = y0.min(y);
                        y1 = y1.max(y + 1);
                    }
                }
            }
            if x1 == 0 {
                continue;
            }
            let r = (
                ox + x0 as u32,
                oy + y0 as u32,
                ox + x1 as u32,
                oy + y1 as u32,
            );
            b = Some(match b {
                None => r,
                Some(b) => (b.0.min(r.0), b.1.min(r.1), b.2.max(r.2), b.3.max(r.3)),
            });
        }
        b
    }

    /// Tile rectangle covering the selection.
    pub fn tile_bounds(&self) -> Option<TileRect> {
        let mut r: Option<TileRect> = None;
        for c in self.coords() {
            let t = TileRect {
                x0: c.x,
                y0: c.y,
                x1: c.x + 1,
                y1: c.y + 1,
            };
            r = Some(r.map_or(t, |r| r.union(t)));
        }
        r
    }

    fn clip_to_canvas(&mut self) {
        // Zero coverage past the right and bottom edges of the canvas.
        let (w, h) = (self.width as usize, self.height as usize);
        for i in 0..self.tiles.len() {
            let c = TileCoord::new(i as u32 % self.cols, i as u32 / self.cols);
            let (ox, oy) = (c.x as usize * TILE, c.y as usize * TILE);
            if ox + TILE <= w && oy + TILE <= h {
                continue;
            }
            if let Some(t) = &mut self.tiles[i] {
                let t = Arc::make_mut(t);
                for y in 0..TILE {
                    for x in 0..TILE {
                        if ox + x >= w || oy + y >= h {
                            t.v[y * TILE + x] = 0;
                        }
                    }
                }
                if t.is_empty() {
                    self.tiles[i] = None;
                }
            }
        }
    }

    /// Builds a selection from a per-row coverage rasteriser. `row(y, buf)`
    /// fills coverage for canvas row `y` (buffer is canvas-wide, zeroed).
    fn from_rows(
        width: u32,
        height: u32,
        y0: u32,
        y1: u32,
        row: impl Fn(u32, &mut [u8]) + Sync,
    ) -> Self {
        let mut s = Selection::empty(width, height);
        let (cols, w) = (s.cols, width as usize);
        let full = Arc::new(SelTile::full());
        let ty0 = y0 / TILE as u32;
        let ty1 = tiles_for(y1.min(height));
        let bands: Vec<(u32, Vec<Option<Arc<SelTile>>>)> = (ty0..ty1)
            .into_par_iter()
            .map(|ty| {
                let mut buf = vec![0u8; w * TILE];
                for ly in 0..TILE {
                    let y = ty as usize * TILE + ly;
                    if y >= height as usize || (y as u32) < y0 || (y as u32) >= y1 {
                        continue;
                    }
                    row(y as u32, &mut buf[ly * w..(ly + 1) * w]);
                }
                let mut out = Vec::with_capacity(cols as usize);
                for tx in 0..cols as usize {
                    let mut t = SelTile { v: [0; TILE_PX] };
                    let x0 = tx * TILE;
                    let n = TILE.min(w - x0);
                    for ly in 0..TILE {
                        t.v[ly * TILE..ly * TILE + n]
                            .copy_from_slice(&buf[ly * w + x0..ly * w + x0 + n]);
                    }
                    out.push(if t.is_empty() {
                        None
                    } else if n == TILE && t.is_full() {
                        Some(full.clone())
                    } else {
                        Some(Arc::new(t))
                    });
                }
                (ty, out)
            })
            .collect();
        for (ty, row) in bands {
            for (tx, t) in row.into_iter().enumerate() {
                s.tiles[(ty * cols) as usize + tx] = t;
            }
        }
        s
    }

    /// A rectangle snapped to whole pixels.
    pub fn rect(width: u32, height: u32, x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        let (ax, bx) = (
            x0.min(x1).round().max(0.0) as u32,
            x0.max(x1).round().min(width as f32) as u32,
        );
        let (ay, by) = (
            y0.min(y1).round().max(0.0) as u32,
            y0.max(y1).round().min(height as f32) as u32,
        );
        if ax >= bx || ay >= by {
            return Selection::empty(width, height);
        }
        Selection::from_rows(width, height, ay, by, |_, buf| {
            buf[ax as usize..bx as usize].fill(255);
        })
    }

    /// An ellipse inscribed in the given box.
    pub fn ellipse(
        width: u32,
        height: u32,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        antialias: bool,
    ) -> Self {
        let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
        let (rx, ry) = (((x1 - x0) * 0.5).abs(), ((y1 - y0) * 0.5).abs());
        if rx < 0.5 || ry < 0.5 {
            return Selection::empty(width, height);
        }
        let n = ((rx.max(ry) * 0.75) as usize).clamp(24, 720);
        let pts: Vec<(f32, f32)> = (0..n)
            .map(|i| {
                let a = i as f32 / n as f32 * std::f32::consts::TAU;
                (cx + a.cos() * rx, cy + a.sin() * ry)
            })
            .collect();
        Selection::polygon(width, height, &pts, antialias)
    }

    /// A closed polygon (even-odd rule), optionally antialiased.
    pub fn polygon(width: u32, height: u32, pts: &[(f32, f32)], antialias: bool) -> Self {
        if pts.len() < 3 {
            return Selection::empty(width, height);
        }
        let ymin = pts
            .iter()
            .map(|p| p.1)
            .fold(f32::MAX, f32::min)
            .floor()
            .max(0.0) as u32;
        let ymax = (pts
            .iter()
            .map(|p| p.1)
            .fold(f32::MIN, f32::max)
            .ceil()
            .max(0.0) as u32)
            .min(height);
        let edges: Vec<((f32, f32), (f32, f32))> = (0..pts.len())
            .map(|i| (pts[i], pts[(i + 1) % pts.len()]))
            .filter(|(a, b)| a.1 != b.1)
            .collect();
        const SUB: usize = 5;
        let w = width as f32;
        Selection::from_rows(width, height, ymin, ymax, |y, buf| {
            let mut acc = vec![0f32; buf.len() + 1];
            let mut xs: Vec<f32> = Vec::with_capacity(16);
            let subs: &[f32] = if antialias {
                &[0.1, 0.3, 0.5, 0.7, 0.9]
            } else {
                &[0.5]
            };
            let wsub = 1.0 / subs.len() as f32;
            for &s in subs.iter().take(SUB) {
                let sy = y as f32 + s;
                xs.clear();
                for &((ax, ay), (bx, by)) in &edges {
                    if (sy >= ay) != (sy >= by) {
                        xs.push(ax + (sy - ay) / (by - ay) * (bx - ax));
                    }
                }
                xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
                for pair in xs.as_chunks::<2>().0 {
                    let (mut l, mut r) = (pair[0].clamp(0.0, w), pair[1].clamp(0.0, w));
                    if !antialias {
                        // Pixel centres inside the span are fully covered.
                        l = (l - 0.5).ceil() + 0.0;
                        r = (r - 0.5).ceil();
                        if r <= l {
                            continue;
                        }
                        acc[l as usize] += wsub;
                        acc[r as usize] -= wsub;
                        continue;
                    }
                    if r <= l {
                        continue;
                    }
                    // Exact horizontal coverage with fractional ends, as a
                    // difference array plus corrections at the end pixels.
                    let (li, ri) = (l.floor() as usize, r.floor() as usize);
                    if li == ri {
                        let v = (r - l) * wsub;
                        acc[li] += v;
                        acc[li + 1] -= v;
                    } else {
                        let lv = (1.0 - (l - li as f32)) * wsub;
                        acc[li] += lv;
                        acc[li + 1] -= lv;
                        acc[li + 1] += wsub;
                        acc[ri] -= wsub;
                        let rv = (r - ri as f32) * wsub;
                        if ri < buf.len() {
                            acc[ri] += rv;
                            acc[ri + 1] -= rv;
                        }
                    }
                }
            }
            let mut run = 0.0;
            for (x, b) in buf.iter_mut().enumerate() {
                run += acc[x];
                *b = (run.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        })
    }

    /// Converts a 16-bit coverage mask (e.g. from the magic wand).
    pub fn from_mask(width: u32, height: u32, m: &Mask) -> Self {
        let mut s = Selection::empty(width, height);
        for &c in m.touched() {
            let mt = m.get(c).unwrap();
            let mut t = SelTile { v: [0; TILE_PX] };
            for (d, v) in t.v.iter_mut().zip(mt.v.iter()) {
                *d = (*v >> 8) as u8;
            }
            if !t.is_empty() {
                s.tiles[(c.y * s.cols + c.x) as usize] = Some(Arc::new(t));
            }
        }
        s
    }

    /// Selects by the alpha channel of a layer's pixels.
    pub fn from_alpha(width: u32, height: u32, g: &crate::tile::TileGrid) -> Self {
        let mut s = Selection::empty(width, height);
        for (c, t) in g.iter() {
            let mut st = SelTile { v: [0; TILE_PX] };
            for (d, p) in st.v.iter_mut().zip(t.px.iter()) {
                *d = p[3];
            }
            if !st.is_empty() {
                s.tiles[(c.y * s.cols + c.x) as usize] = Some(Arc::new(st));
            }
        }
        s
    }

    /// The selection as premultiplied white pixels with coverage as alpha,
    /// so it can be resampled like an image.
    pub fn to_grid(&self) -> crate::tile::TileGrid {
        let mut g = crate::tile::TileGrid::new(self.width, self.height);
        for c in self.coords().collect::<Vec<_>>() {
            let st = self.tile(c).unwrap();
            let t = g.get_mut_or_insert(c);
            for (p, v) in t.px.iter_mut().zip(st.v.iter()) {
                *p = [*v; 4];
            }
        }
        g
    }

    /// Combines `other` into `self` with `op`.
    pub fn combine(&self, other: &Selection, op: SelectOp) -> Selection {
        if op == SelectOp::Replace {
            return other.clone();
        }
        let mut out = self.clone();
        for i in 0..self.tiles.len() {
            let (a, b) = (&self.tiles[i], &other.tiles[i]);
            out.tiles[i] = match (op, a, b) {
                (SelectOp::Add, None, b) => b.clone(),
                (SelectOp::Add, a, None) => a.clone(),
                (SelectOp::Subtract, a, None) => a.clone(),
                (SelectOp::Subtract | SelectOp::Intersect, None, _) => None,
                (SelectOp::Intersect, _, None) => None,
                (_, Some(a), Some(b)) => {
                    let mut t = SelTile { v: [0; TILE_PX] };
                    for k in 0..TILE_PX {
                        let (x, y) = (a.v[k] as u32, b.v[k] as u32);
                        t.v[k] = match op {
                            SelectOp::Add => x.max(y),
                            SelectOp::Subtract => crate::blend::div255(x * (255 - y)),
                            _ => crate::blend::div255(x * y),
                        } as u8;
                    }
                    if t.is_empty() {
                        None
                    } else {
                        Some(Arc::new(t))
                    }
                }
                _ => unreachable!(),
            };
        }
        out
    }

    pub fn inverted(&self) -> Selection {
        let mut out = self.clone();
        let full = Arc::new(SelTile::full());
        for (o, t) in out.tiles.iter_mut().zip(&self.tiles) {
            *o = match t {
                None => Some(full.clone()),
                Some(t) => {
                    let mut n = SelTile { v: [0; TILE_PX] };
                    for k in 0..TILE_PX {
                        n.v[k] = 255 - t.v[k];
                    }
                    if n.is_empty() {
                        None
                    } else {
                        Some(Arc::new(n))
                    }
                }
            };
        }
        out.clip_to_canvas();
        out
    }

    /// Runs `f` over a dense copy of the selection's neighbourhood (bounds
    /// expanded by `pad`) and stores the result back.
    fn dense_op(&self, pad: u32, f: impl FnOnce(&mut [f32], usize, usize)) -> Selection {
        let Some((x0, y0, x1, y1)) = self.bounds() else {
            return self.clone();
        };
        let x0 = x0.saturating_sub(pad);
        let y0 = y0.saturating_sub(pad);
        let x1 = (x1 + pad).min(self.width);
        let y1 = (y1 + pad).min(self.height);
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let mut buf = vec![0f32; w * h];
        buf.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                *v = self.value(x0 as i64 + x as i64, y0 as i64 + y as i64) as f32;
            }
        });
        f(&mut buf, w, h);
        Selection::from_rows(self.width, self.height, y0, y1, |y, row| {
            let r = &buf[(y - y0) as usize * w..(y - y0 + 1) as usize * w];
            for (x, v) in r.iter().enumerate() {
                row[x0 as usize + x] = (v.clamp(0.0, 255.0) + 0.5) as u8;
            }
        })
    }

    /// Softens the edge by roughly `radius` pixels (three box blurs, which
    /// approximate a Gaussian).
    pub fn feathered(&self, radius: f32) -> Selection {
        let r = radius.max(0.0);
        if r < 0.5 {
            return self.clone();
        }
        let br = ((r / 3f32.sqrt()).round() as usize).max(1);
        self.dense_op((r * 2.0).ceil() as u32 + 2, |buf, w, h| {
            for _ in 0..3 {
                box_blur(buf, w, h, br);
            }
        })
    }

    /// Grows (positive) or shrinks (negative) the selection by whole pixels.
    pub fn grown(&self, by: i32) -> Selection {
        if by == 0 {
            return self.clone();
        }
        let r = by.unsigned_abs() as usize;
        self.dense_op(r as u32 + 1, |buf, w, h| {
            let grow = by > 0;
            morph(buf, w, h, r, grow);
        })
    }

    /// Edges between selected and unselected pixels (coverage ≥ 50%), as
    /// canvas-space line segments `[x0, y0, x1, y1]` merged into long runs.
    pub fn outline(&self) -> Vec<[f32; 4]> {
        let on = |x: i64, y: i64| self.value(x, y) >= 128;
        // Which tiles need scanning: partial ones, and full ones bordering
        // something that isn't full.
        let state = |c: Option<TileCoord>| -> u8 {
            match c.and_then(|c| self.tile(c)) {
                None => 0,
                Some(t) if t.is_full() => 2,
                Some(_) => 1,
            }
        };
        let mut segs: Vec<[f32; 4]> = vec![];
        let tiles: Vec<TileCoord> = self
            .coords()
            .filter(|&c| {
                let s = state(Some(c));
                if s == 1 {
                    return true;
                }
                let n = |dx: i64, dy: i64| {
                    let (x, y) = (c.x as i64 + dx, c.y as i64 + dy);
                    if x < 0 || y < 0 || x >= self.cols as i64 || y >= self.rows as i64 {
                        None
                    } else {
                        Some(TileCoord::new(x as u32, y as u32))
                    }
                };
                [n(-1, 0), n(1, 0), n(0, -1), n(0, 1)]
                    .iter()
                    .any(|t| state(*t) != 2)
                    || c.x + 1 == self.cols
                    || c.y + 1 == self.rows
            })
            .collect();
        let per: Vec<Vec<[f32; 4]>> = tiles
            .par_iter()
            .map(|&c| {
                let mut out = vec![];
                let (ox, oy) = (c.x as i64 * TILE as i64, c.y as i64 * TILE as i64);
                // Horizontal edges on the top side of each pixel row, plus
                // the bottom side of the tile's last row.
                for ly in 0..=TILE as i64 {
                    let y = oy + ly;
                    if ly == TILE as i64 && y < self.height as i64 {
                        // The next tile row handles it, unless it is unscanned.
                        let below = TileCoord::new(c.x, c.y + 1);
                        if tiles_contains(&tiles, below) {
                            continue;
                        }
                    }
                    let mut start: Option<i64> = None;
                    for lx in 0..=TILE as i64 {
                        let x = ox + lx;
                        let edge = lx < TILE as i64 && on(x, y) != on(x, y - 1);
                        match (edge, start) {
                            (true, None) => start = Some(x),
                            (false, Some(s)) => {
                                out.push([s as f32, y as f32, x as f32, y as f32]);
                                start = None;
                            }
                            _ => {}
                        }
                    }
                }
                for lx in 0..=TILE as i64 {
                    let x = ox + lx;
                    if lx == TILE as i64 && x < self.width as i64 {
                        let right = TileCoord::new(c.x + 1, c.y);
                        if tiles_contains(&tiles, right) {
                            continue;
                        }
                    }
                    let mut start: Option<i64> = None;
                    for ly in 0..=TILE as i64 {
                        let y = oy + ly;
                        let edge = ly < TILE as i64 && on(x, y) != on(x - 1, y);
                        match (edge, start) {
                            (true, None) => start = Some(y),
                            (false, Some(s)) => {
                                out.push([x as f32, s as f32, x as f32, y as f32]);
                                start = None;
                            }
                            _ => {}
                        }
                    }
                }
                out
            })
            .collect();
        for p in per {
            segs.extend(p);
        }
        // Edges of tiles that are empty but border a scanned full tile are
        // found from the full tile's side, since `on` reads across tiles.
        segs
    }
}

fn tiles_contains(sorted_rowmajor: &[TileCoord], c: TileCoord) -> bool {
    sorted_rowmajor
        .binary_search_by(|t| (t.y, t.x).cmp(&(c.y, c.x)))
        .is_ok()
}

fn box_blur(buf: &mut [f32], w: usize, h: usize, r: usize) {
    let tmp: Vec<f32> = buf.to_vec();
    let inv = 1.0 / (2 * r + 1) as f32;
    // Horizontal.
    let mut mid = vec![0f32; w * h];
    mid.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let row = &tmp[y * w..(y + 1) * w];
        let get = |x: i64| row[x.clamp(0, w as i64 - 1) as usize];
        let mut acc: f32 = (-(r as i64)..=r as i64).map(get).sum();
        for (x, o) in out.iter_mut().enumerate() {
            *o = acc * inv;
            acc += get(x as i64 + r as i64 + 1) - get(x as i64 - r as i64);
        }
    });
    // Vertical, column by column in parallel bands.
    let cols: Vec<Vec<f32>> = (0..w)
        .into_par_iter()
        .map(|x| {
            let get = |y: i64| mid[y.clamp(0, h as i64 - 1) as usize * w + x];
            let mut acc: f32 = (-(r as i64)..=r as i64).map(get).sum();
            let mut col = vec![0f32; h];
            for (y, o) in col.iter_mut().enumerate() {
                *o = acc * inv;
                acc += get(y as i64 + r as i64 + 1) - get(y as i64 - r as i64);
            }
            col
        })
        .collect();
    for (x, col) in cols.iter().enumerate() {
        for (y, v) in col.iter().enumerate() {
            buf[y * w + x] = *v;
        }
    }
}

/// Circular max (grow) or min (shrink) filter.
fn morph(buf: &mut [f32], w: usize, h: usize, r: usize, grow: bool) {
    let src = buf.to_vec();
    let ri = r as i64;
    let offs: Vec<(i64, i64)> = (-ri..=ri)
        .flat_map(|dy| (-ri..=ri).map(move |dx| (dx, dy)))
        .filter(|(dx, dy)| dx * dx + dy * dy <= ri * ri + ri)
        .collect();
    buf.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut v: f32 = if grow { 0.0 } else { 255.0 };
            for &(dx, dy) in &offs {
                let (sx, sy) = (x as i64 + dx, y as i64 + dy);
                let s = if sx < 0 || sy < 0 || sx >= w as i64 || sy >= h as i64 {
                    0.0
                } else {
                    src[sy as usize * w + sx as usize]
                };
                v = if grow { v.max(s) } else { v.min(s) };
            }
            *o = v;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_and_ops() {
        let a = Selection::rect(200, 100, 10.0, 10.0, 50.0, 40.0);
        assert_eq!(a.value(10, 10), 255);
        assert_eq!(a.value(49, 39), 255);
        assert_eq!(a.value(50, 39), 0);
        assert_eq!(a.bounds(), Some((10, 10, 50, 40)));
        let b = Selection::rect(200, 100, 30.0, 0.0, 120.0, 100.0);
        let u = a.combine(&b, SelectOp::Add);
        assert_eq!(u.value(100, 90), 255);
        let s = a.combine(&b, SelectOp::Subtract);
        assert_eq!(s.value(35, 20), 0);
        assert_eq!(s.value(15, 20), 255);
        let i = a.combine(&b, SelectOp::Intersect);
        assert_eq!(i.bounds(), Some((30, 10, 50, 40)));
        let inv = a.inverted();
        assert_eq!(inv.value(0, 0), 255);
        assert_eq!(inv.value(20, 20), 0);
        assert_eq!(inv.value(199, 99), 255);
    }

    #[test]
    fn polygon_coverage() {
        // A triangle; its interior is full and edges are partial.
        let s = Selection::polygon(100, 100, &[(10.0, 10.0), (90.0, 10.0), (10.0, 90.0)], true);
        assert_eq!(s.value(20, 20), 255);
        assert_eq!(s.value(80, 80), 0);
        let e = s.value(50, 49);
        assert!(e > 0 && e < 255, "{e}");
        let hard = Selection::polygon(100, 100, &[(10.0, 10.0), (90.0, 10.0), (10.0, 90.0)], false);
        assert!([0, 255].contains(&hard.value(50, 49)));
        let el = Selection::ellipse(100, 100, 0.0, 0.0, 100.0, 100.0, true);
        assert_eq!(el.value(50, 50), 255);
        assert_eq!(el.value(2, 2), 0);
    }

    #[test]
    fn outline_of_rect() {
        let a = Selection::rect(300, 300, 10.0, 20.0, 150.0, 90.0);
        let segs = a.outline();
        let len: f32 = segs
            .iter()
            .map(|s| (s[2] - s[0]).abs() + (s[3] - s[1]).abs())
            .sum();
        assert_eq!(len, 2.0 * (140.0 + 70.0));
    }

    #[test]
    fn feather_and_grow() {
        let a = Selection::rect(200, 200, 50.0, 50.0, 150.0, 150.0);
        let f = a.feathered(8.0);
        assert!(f.value(100, 100) > 250);
        let edge = f.value(50, 100);
        assert!(edge > 60 && edge < 200, "{edge}");
        let g = a.grown(5);
        assert_eq!(g.value(46, 100), 255);
        assert_eq!(g.value(40, 100), 0);
        let s = a.grown(-5);
        assert_eq!(s.value(52, 100), 0);
        assert_eq!(s.value(60, 100), 255);
    }
}
