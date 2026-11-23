//! The brush engine.
//!
//! A stroke is rendered in two stages, the model Photoshop popularised:
//!
//! 1. Round dabs are stamped into a 16-bit *coverage mask*. `flow` controls
//!    how much each dab adds; `opacity` caps the total, so going back over the
//!    same spot within one stroke never exceeds the chosen opacity.
//! 2. The mask is composited onto the layer with the brush colour, live for
//!    display and for real when the stroke ends.
//!
//! The 16-bit mask means low-flow airbrushing builds up smoothly instead of
//! banding or stalling the way repeated 8-bit dab blending does, and the
//! layer itself is touched exactly once per stroke, which also makes the
//! stroke a single, cheap undo step.

use rayon::prelude::*;

use crate::tile::{TILE, TILE_PX, Tile, TileCoord, TileRect, tiles_for};

/// Coverage values for one tile, `0..=65535`.
#[derive(Clone)]
pub struct MaskTile {
    pub v: [u16; TILE_PX],
}

/// A sparse 16-bit coverage mask the size of the canvas.
pub struct Mask {
    width: u32,
    height: u32,
    cols: u32,
    rows: u32,
    tiles: Vec<Option<Box<MaskTile>>>,
    touched: Vec<TileCoord>,
}

impl Mask {
    pub fn new(width: u32, height: u32) -> Self {
        let (cols, rows) = (tiles_for(width), tiles_for(height));
        Mask {
            width,
            height,
            cols,
            rows,
            tiles: (0..cols * rows).map(|_| None).collect(),
            touched: vec![],
        }
    }

    #[inline]
    pub fn get(&self, c: TileCoord) -> Option<&MaskTile> {
        self.tiles[(c.y * self.cols + c.x) as usize].as_deref()
    }

    /// Tiles that contain any coverage, in the order they were first touched.
    pub fn touched(&self) -> &[TileCoord] {
        &self.touched
    }

    pub fn is_empty(&self) -> bool {
        self.touched.is_empty()
    }

    pub fn tile_mut(&mut self, c: TileCoord) -> &mut MaskTile {
        let i = (c.y * self.cols + c.x) as usize;
        if self.tiles[i].is_none() {
            self.tiles[i] = Some(Box::new(MaskTile { v: [0; TILE_PX] }));
            self.touched.push(c);
        }
        self.tiles[i].as_mut().unwrap()
    }

    /// Reads the coverage of one pixel.
    pub fn value(&self, x: u32, y: u32) -> u16 {
        let t = TILE as u32;
        self.get(TileCoord::new(x / t, y / t))
            .map_or(0, |m| m.v[((y % t) * t + x % t) as usize])
    }

    /// Stamps one dab, appending the tiles it touched to `dirty`.
    pub fn stamp(&mut self, dab: &Dab, dirty: &mut Vec<TileCoord>) {
        if dab.pixel {
            self.stamp_pixel(dab, dirty);
            return;
        }
        let r = dab.radius.max(0.5);
        let (w, h) = (self.width as f32, self.height as f32);
        let x0 = (dab.x - r - 1.0).floor().max(0.0);
        let y0 = (dab.y - r - 1.0).floor().max(0.0);
        let x1 = (dab.x + r + 1.0).ceil().min(w);
        let y1 = (dab.y + r + 1.0).ceil().min(h);
        if x0 >= x1 || y0 >= y1 || dab.flow <= 0.0 || dab.cap <= 0.0 {
            return;
        }
        let px = [x0 as u32, y0 as u32, x1 as u32, y1 as u32];
        let rect = TileRect::from_px(
            px[0] as i64,
            px[1] as i64,
            px[2] as i64,
            px[3] as i64,
            self.cols,
            self.rows,
        );
        for c in rect.iter() {
            self.tile_mut(c);
            dirty.push(c);
        }
        let shape = DabShape::new(dab);
        let cols = self.cols as usize;
        let (x0, x1) = (rect.x0 as usize, rect.x1 as usize);
        // Borrow just the rows of the grid the dab covers.
        let rows = &mut self.tiles[rect.y0 as usize * cols..rect.y1 as usize * cols];
        let mut jobs: Vec<(TileCoord, &mut MaskTile)> = rows
            .chunks_mut(cols)
            .enumerate()
            .flat_map(|(ry, row)| {
                row[x0..x1].iter_mut().enumerate().map(move |(rx, t)| {
                    (
                        TileCoord::new((x0 + rx) as u32, rect.y0 + ry as u32),
                        &mut **t.as_mut().unwrap(),
                    )
                })
            })
            .collect();
        // Big dabs are split across cores; small ones aren't worth the overhead.
        if jobs.len() > 4 && r > 160.0 {
            jobs.par_iter_mut()
                .for_each(|(c, t)| shape.stamp_tile(*c, px, t));
        } else {
            jobs.iter_mut()
                .for_each(|(c, t)| shape.stamp_tile(*c, px, t));
        }
    }

    /// Stamps an aliased square of whole pixels centred on the dab.
    fn stamp_pixel(&mut self, dab: &Dab, dirty: &mut Vec<TileCoord>) {
        let side = (dab.radius * 2.0).round().max(1.0) as i64;
        let x0 = (dab.x - side as f32 * 0.5 + 0.5).floor() as i64;
        let y0 = (dab.y - side as f32 * 0.5 + 0.5).floor() as i64;
        let cap = (dab.cap.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
        let (w, h) = (self.width as i64, self.height as i64);
        for y in y0.max(0)..(y0 + side).min(h) {
            for x in x0.max(0)..(x0 + side).min(w) {
                let c = TileCoord::new((x / TILE as i64) as u32, (y / TILE as i64) as u32);
                let t = self.tile_mut(c);
                let v = &mut t.v[(y as usize % TILE) * TILE + x as usize % TILE];
                *v = (*v).max(cap);
                if dirty.last() != Some(&c) {
                    dirty.push(c);
                }
            }
        }
    }

    /// Multiplies coverage by a selection, so edits only land inside it.
    pub fn restrict(&mut self, sel: &crate::selection::Selection) {
        for i in 0..self.tiles.len() {
            let c = TileCoord::new(i as u32 % self.cols, i as u32 / self.cols);
            let Some(t) = &mut self.tiles[i] else {
                continue;
            };
            match sel.tile(c) {
                None => t.v.fill(0),
                Some(s) => {
                    for (v, s) in t.v.iter_mut().zip(s.v.iter()) {
                        *v = ((*v as u32 * *s as u32 + 127) / 255) as u16;
                    }
                }
            }
        }
        let tiles = &self.tiles;
        let cols = self.cols;
        self.touched.retain(|c| {
            tiles[(c.y * cols + c.x) as usize]
                .as_ref()
                .is_some_and(|t| t.v.iter().any(|v| *v != 0))
        });
    }

    /// Fills every tile in `rect` that `f` reports as covered.
    pub fn set_from(
        &mut self,
        rect: TileRect,
        mut f: impl FnMut(TileCoord, &mut MaskTile) -> bool,
    ) {
        for c in rect.iter() {
            let mut t = Box::new(MaskTile { v: [0; TILE_PX] });
            if f(c, &mut t) {
                let i = (c.y * self.cols + c.x) as usize;
                self.tiles[i] = Some(t);
                self.touched.push(c);
            }
        }
    }
}

/// A single round brush imprint.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Dab {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    /// 0 = soft airbrush falloff, 1 = crisp edge.
    pub hardness: f32,
    /// Fraction of the remaining headroom each dab fills, 0..=1.
    pub flow: f32,
    /// Maximum coverage for this dab, 0..=1.
    pub cap: f32,
    /// Aliased square covering whole pixels (pixel-art mode).
    pub pixel: bool,
}

struct DabShape {
    x: f32,
    y: f32,
    r: f32,
    inner: f32,
    inv_soft: f32,
    flow: f32,
    cap: u32,
}

impl DabShape {
    fn new(d: &Dab) -> Self {
        let r = d.radius.max(0.5);
        let inner = r * d.hardness.clamp(0.0, 1.0);
        DabShape {
            x: d.x,
            y: d.y,
            r,
            inner,
            inv_soft: if r - inner > 1e-4 {
                1.0 / (r - inner)
            } else {
                0.0
            },
            flow: d.flow.clamp(0.0, 1.0),
            cap: (d.cap.clamp(0.0, 1.0) * 65535.0 + 0.5) as u32,
        }
    }

    fn stamp_tile(&self, c: TileCoord, px: [u32; 4], m: &mut MaskTile) {
        let t = TILE as u32;
        let (bx, by) = (c.x * t, c.y * t);
        let lx0 = px[0].saturating_sub(bx).min(t) as usize;
        let ly0 = px[1].saturating_sub(by).min(t) as usize;
        let lx1 = (px[2].saturating_sub(bx)).min(t) as usize;
        let ly1 = (px[3].saturating_sub(by)).min(t) as usize;
        let r2 = (self.r + 0.5) * (self.r + 0.5);
        for ly in ly0..ly1 {
            let dy = (by as usize + ly) as f32 + 0.5 - self.y;
            if dy * dy >= r2 {
                continue;
            }
            // Only visit the span of this row that lies inside the circle.
            let half = (r2 - dy * dy).sqrt();
            let sx0 = ((self.x - half - 0.5).floor() as i64 - bx as i64)
                .clamp(lx0 as i64, lx1 as i64) as usize;
            let sx1 = ((self.x + half + 0.5).ceil() as i64 - bx as i64)
                .clamp(lx0 as i64, lx1 as i64) as usize;
            let row = &mut m.v[ly * TILE..ly * TILE + TILE];
            // Branch-free so the compiler can vectorise it.
            let cap = self.cap as f32;
            let base = bx as f32 + 0.5 - self.x;
            for (lx, v) in row.iter_mut().enumerate().take(sx1).skip(sx0) {
                let dx = base + lx as f32;
                let d = (dx * dx + dy * dy).sqrt();
                let edge = (self.r - d + 0.5).clamp(0.0, 1.0);
                let s = (1.0 - (d - self.inner) * self.inv_soft).clamp(0.0, 1.0);
                let f = edge.min(s * s * (3.0 - 2.0 * s)) * self.flow;
                let cur = *v as f32;
                *v = (cur + (cap - cur).max(0.0) * f + 0.5).min(cap.max(cur)) as u16;
            }
        }
    }
}

/// What a mask does when applied to a layer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PaintMode {
    Paint,
    Erase,
}

/// Colour and mode used to apply a mask.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Paint {
    pub color: [u8; 3],
    pub mode: PaintMode,
    /// Only change colour where the layer already has paint.
    pub alpha_lock: bool,
}

#[inline(always)]
fn div65535(x: u32) -> u32 {
    let x = x + 32768;
    (x + (x >> 16)) >> 16
}

/// Applies `mask` with `paint` onto `dst` in place.
pub fn apply_mask_tile(dst: &mut Tile, mask: &MaskTile, paint: &Paint) {
    let [r, g, b] = paint.color;
    for (p, &m) in dst.px.iter_mut().zip(mask.v.iter()) {
        if m == 0 {
            continue;
        }
        let m = m as u32;
        let inv = 65535 - m;
        match (paint.mode, paint.alpha_lock) {
            (PaintMode::Erase, false) => {
                for c in p.iter_mut() {
                    *c = div65535(*c as u32 * inv) as u8;
                }
            }
            (PaintMode::Paint, false) => {
                let src = [r, g, b, 255];
                for i in 0..4 {
                    p[i] = div65535(src[i] as u32 * m + p[i] as u32 * inv) as u8;
                }
            }
            (mode, true) => {
                // Alpha is preserved; colour moves toward the brush colour (or
                // toward black-premultiplied "nothing" for the eraser, which
                // here means toward transparent colour at the same alpha would
                // be meaningless, so erasing under alpha lock does nothing).
                if mode == PaintMode::Erase || p[3] == 0 {
                    continue;
                }
                let a = p[3] as u32;
                let src = [r, g, b].map(|c| crate::blend::div255(c as u32 * a));
                for i in 0..3 {
                    p[i] = div65535(src[i] * m + p[i] as u32 * inv) as u8;
                }
            }
        }
    }
}

/// Like [`apply_mask_tile`], with coverage scaled by a selection tile.
pub fn apply_mask_tile_sel(
    dst: &mut Tile,
    mask: &MaskTile,
    sel: Option<&crate::selection::SelTile>,
    paint: &Paint,
) {
    match sel {
        None => apply_mask_tile(dst, mask, paint),
        Some(s) => {
            let mut m = mask.clone();
            for (v, s) in m.v.iter_mut().zip(s.v.iter()) {
                *v = ((*v as u32 * *s as u32 + 127) / 255) as u16;
            }
            apply_mask_tile(dst, &m, paint);
        }
    }
}

/// User-facing brush settings.
#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(default)
)]
pub struct BrushParams {
    /// Diameter in canvas pixels.
    pub size: f32,
    pub hardness: f32,
    pub opacity: f32,
    pub flow: f32,
    /// Distance between dabs as a fraction of the diameter.
    pub spacing: f32,
    pub pressure_size: bool,
    pub pressure_opacity: bool,
    /// Size at zero pressure, as a fraction of `size`.
    pub min_size: f32,
    /// Stabiliser strength, 0..=1.
    pub smoothing: f32,
    /// Pixel-art mode: aliased square dabs on whole pixels, no gaps.
    pub pixel: bool,
    /// In pixel mode, removes the doubled corner pixels of 1px lines.
    pub pixel_perfect: bool,
}

impl Default for BrushParams {
    fn default() -> Self {
        BrushParams {
            size: 12.0,
            hardness: 0.85,
            opacity: 1.0,
            flow: 1.0,
            spacing: 0.08,
            pressure_size: true,
            pressure_opacity: false,
            min_size: 0.15,
            smoothing: 0.3,
            pixel: false,
            pixel_perfect: true,
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct Pt {
    x: f32,
    y: f32,
    p: f32,
}

/// Turns a stream of pointer samples into evenly spaced dabs.
pub struct StrokeEngine {
    params: BrushParams,
    smoothed: Option<Pt>,
    last_raw: Option<Pt>,
    carry: f32,
    /// Pixel mode: last emitted pixel and pixels held back for the
    /// pixel-perfect corner test.
    last_px: Option<(i64, i64)>,
    held: Vec<Dab>,
}

impl StrokeEngine {
    pub fn new(params: BrushParams) -> Self {
        StrokeEngine {
            params,
            smoothed: None,
            last_raw: None,
            carry: 0.0,
            last_px: None,
            held: vec![],
        }
    }

    pub fn params(&self) -> &BrushParams {
        &self.params
    }

    fn dab_at(&self, q: Pt) -> Dab {
        let p = &self.params;
        let pr = q.p.clamp(0.0, 1.0);
        let scale = if p.pressure_size {
            p.min_size + (1.0 - p.min_size) * pr
        } else {
            1.0
        };
        let cap = if p.pressure_opacity {
            p.opacity * pr
        } else {
            p.opacity
        };
        Dab {
            x: q.x,
            y: q.y,
            radius: (p.size * 0.5 * scale).max(0.5),
            hardness: p.hardness,
            flow: if p.pixel { 1.0 } else { p.flow },
            cap,
            pixel: p.pixel,
        }
    }

    /// Emits a pixel-mode dab, skipping repeats and (for 1px brushes with
    /// pixel-perfect on) the middle pixel of L-shaped corners.
    fn emit(&mut self, d: Dab, out: &mut Vec<Dab>) {
        if !d.pixel {
            out.push(d);
            return;
        }
        let key = (d.x.floor() as i64, d.y.floor() as i64);
        if self.last_px == Some(key) {
            return;
        }
        self.last_px = Some(key);
        let perfect = self.params.pixel_perfect && (d.radius * 2.0).round() <= 1.0;
        if !perfect {
            out.push(d);
            return;
        }
        self.held.push(d);
        if self.held.len() == 3 {
            let p = |d: &Dab| (d.x.floor() as i64, d.y.floor() as i64);
            let (a, b, c) = (p(&self.held[0]), p(&self.held[1]), p(&self.held[2]));
            let corner = (a.0 == b.0 || a.1 == b.1)
                && (b.0 == c.0 || b.1 == c.1)
                && (a.0 - c.0).abs() == 1
                && (a.1 - c.1).abs() == 1;
            if corner {
                self.held.remove(1);
            }
        }
        while self.held.len() > 2 {
            out.push(self.held.remove(0));
        }
    }

    fn spacing_at(&self, q: Pt) -> f32 {
        if self.params.pixel {
            return 0.5;
        }
        (self.dab_at(q).radius * 2.0 * self.params.spacing.max(0.01)).max(0.5)
    }

    fn segment(&mut self, a: Pt, b: Pt, out: &mut Vec<Dab>) {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = (dx * dx + dy * dy).sqrt();
        if len <= 1e-6 {
            return;
        }
        let mut t = 0.0f32; // distance travelled along this segment
        loop {
            let here = lerp_pt(a, b, t / len);
            let need = self.spacing_at(here) - self.carry;
            if t + need > len {
                self.carry += len - t;
                return;
            }
            t += need;
            self.carry = 0.0;
            let d = self.dab_at(lerp_pt(a, b, t / len));
            self.emit(d, out);
        }
    }

    /// Adds a pointer sample (canvas coordinates, pressure 0..=1).
    pub fn add_point(&mut self, x: f32, y: f32, pressure: f32, out: &mut Vec<Dab>) {
        let raw = Pt { x, y, p: pressure };
        self.last_raw = Some(raw);
        let Some(prev) = self.smoothed else {
            self.smoothed = Some(raw);
            let d = self.dab_at(raw);
            self.emit(d, out);
            return;
        };
        // Exponential smoothing with a strength curve that stays responsive
        // at low settings and becomes a strong "lazy" stabiliser near 1.
        let s = self.params.smoothing.clamp(0.0, 1.0);
        let w = 1.0 - s.powf(0.6) * 0.94;
        let next = Pt {
            x: prev.x + (raw.x - prev.x) * w,
            y: prev.y + (raw.y - prev.y) * w,
            p: prev.p + (raw.p - prev.p) * w.max(0.5),
        };
        self.segment(prev, next, out);
        self.smoothed = Some(next);
    }

    /// Ends the stroke, letting the stabilised path catch up to the pen.
    pub fn finish(&mut self, out: &mut Vec<Dab>) {
        if let (Some(s), Some(r)) = (self.smoothed, self.last_raw) {
            self.segment(s, Pt { p: s.p, ..r }, out);
            self.smoothed = Some(r);
        }
        out.append(&mut self.held);
    }

    /// Last position of the stroke, after smoothing.
    pub fn position(&self) -> Option<(f32, f32)> {
        self.smoothed.map(|p| (p.x, p.y))
    }
}

fn lerp_pt(a: Pt, b: Pt, t: f32) -> Pt {
    Pt {
        x: a.x + (b.x - a.x) * t,
        y: a.y + (b.y - a.y) * t,
        p: a.p + (b.p - a.p) * t,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spacing_is_even() {
        let params = BrushParams {
            size: 10.0,
            spacing: 0.5,
            smoothing: 0.0,
            pressure_size: false,
            ..Default::default()
        };
        let mut e = StrokeEngine::new(params);
        let mut dabs = vec![];
        e.add_point(0.0, 0.0, 1.0, &mut dabs);
        for i in 1..=10 {
            e.add_point(i as f32 * 3.3, 0.0, 1.0, &mut dabs);
        }
        e.finish(&mut dabs);
        assert_eq!(dabs.len(), 7); // 0,5,10,...,30
        for w in dabs.windows(2) {
            assert!((w[1].x - w[0].x - 5.0).abs() < 1e-3);
        }
    }

    #[test]
    fn opacity_caps_buildup() {
        let mut m = Mask::new(64, 64);
        let mut dirty = vec![];
        let dab = Dab {
            x: 32.0,
            y: 32.0,
            radius: 10.0,
            hardness: 1.0,
            flow: 0.5,
            cap: 0.5,
            pixel: false,
        };
        for _ in 0..50 {
            m.stamp(&dab, &mut dirty);
        }
        let v = m.value(32, 32);
        assert!((v as i32 - 32768).abs() < 4, "{v}");
        assert_eq!(m.value(0, 0), 0);
    }

    #[test]
    fn low_flow_reaches_full() {
        let mut m = Mask::new(64, 64);
        let mut dirty = vec![];
        let dab = Dab {
            x: 32.0,
            y: 32.0,
            radius: 10.0,
            hardness: 1.0,
            flow: 0.02,
            cap: 1.0,
            pixel: false,
        };
        for _ in 0..800 {
            m.stamp(&dab, &mut dirty);
        }
        assert!(m.value(32, 32) > 65000);
    }

    #[test]
    fn pixel_perfect_line() {
        let params = BrushParams {
            size: 1.0,
            pixel: true,
            pixel_perfect: true,
            smoothing: 0.0,
            pressure_size: false,
            ..Default::default()
        };
        let mut e = StrokeEngine::new(params);
        let mut dabs = vec![];
        // A staircase drawn as right-then-down moves.
        let pts = [(0.5, 0.5), (1.5, 0.5), (1.5, 1.5), (2.5, 1.5), (2.5, 2.5)];
        for (x, y) in pts {
            e.add_point(x, y, 1.0, &mut dabs);
        }
        e.finish(&mut dabs);
        let px: Vec<(i64, i64)> = dabs
            .iter()
            .map(|d| (d.x.floor() as i64, d.y.floor() as i64))
            .collect();
        assert_eq!(px.first(), Some(&(0, 0)));
        assert_eq!(px.last(), Some(&(2, 2)));
        // Corners removed: the result is a clean diagonal.
        assert!(!px.contains(&(1, 0)), "{px:?}");
        let mut m = Mask::new(8, 8);
        let mut dirty = vec![];
        for d in &dabs {
            m.stamp(d, &mut dirty);
        }
        assert_eq!(m.value(1, 1), 65535);
        assert_eq!(m.value(1, 0), 0);
    }

    #[test]
    fn apply_paint_and_erase() {
        let mut t = Tile::filled([0; 4]);
        let mask = MaskTile {
            v: [65535; TILE_PX],
        };
        apply_mask_tile(
            &mut t,
            &mask,
            &Paint {
                color: [10, 20, 30],
                mode: PaintMode::Paint,
                alpha_lock: false,
            },
        );
        assert_eq!(t.px[0], [10, 20, 30, 255]);
        let half = MaskTile {
            v: [32768; TILE_PX],
        };
        apply_mask_tile(
            &mut t,
            &half,
            &Paint {
                color: [0; 3],
                mode: PaintMode::Erase,
                alpha_lock: false,
            },
        );
        assert_eq!(t.px[0][3], 127);
    }

    #[test]
    fn big_dab_parallel_matches_serial_shape() {
        let mut m = Mask::new(1024, 1024);
        let mut dirty = vec![];
        m.stamp(
            &Dab {
                x: 500.0,
                y: 500.0,
                radius: 300.0,
                hardness: 0.5,
                flow: 1.0,
                cap: 1.0,
                pixel: false,
            },
            &mut dirty,
        );
        assert_eq!(m.value(500, 500), 65535);
        assert_eq!(m.value(500, 199), 0);
        // Symmetric falloff.
        let a = m.value(500 + 200, 500);
        let b = m.value(500 - 201, 500);
        assert!((a as i32 - b as i32).abs() < 200, "{a} {b}");
    }
}
