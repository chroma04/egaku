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

use std::sync::Arc;

use rayon::prelude::*;

use crate::tile::{TILE, TILE_PX, Tile, TileCoord, TileRect, tiles_for};
use crate::tip::TipImage;

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
        let shape = DabShape::new(dab);
        let r = shape.extent;
        let (w, h) = (self.width as f32, self.height as f32);
        let x0 = (shape.x - r - 1.0).floor().max(0.0);
        let y0 = (shape.y - r - 1.0).floor().max(0.0);
        let x1 = (shape.x + r + 1.0).ceil().min(w);
        let y1 = (shape.y + r + 1.0).ceil().min(h);
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

/// The outline of a brush tip.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TipShape {
    /// A circle, or an ellipse with roundness below 1.
    #[default]
    Round,
    /// A square, or a rectangle with roundness below 1.
    Square,
    /// A registered image (see [`crate::tip`]), by id. Falls back to round
    /// if the image isn't registered.
    Image(u64),
}

/// A single brush imprint.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Dab {
    pub x: f32,
    pub y: f32,
    /// Half the tip's long axis.
    pub radius: f32,
    /// 0 = soft airbrush falloff, 1 = crisp edge.
    pub hardness: f32,
    /// Fraction of the remaining headroom each dab fills, 0..=1.
    pub flow: f32,
    /// Maximum coverage for this dab, 0..=1.
    pub cap: f32,
    /// Aliased: every pixel is either inside the tip or not, and the tip is
    /// centred on the pixel grid (pixel-art mode). Falloff, grain and
    /// image tips still give partial coverage inside it.
    pub pixel: bool,
    pub tip: TipShape,
    /// Short axis over long axis, 0..=1 (1 = round).
    pub roundness: f32,
    /// Rotation of the long axis, radians.
    pub angle: f32,
    /// How much the paper grain shows through, 0..=1.
    pub grain: f32,
    /// Size of the grain's features in canvas pixels.
    pub grain_scale: f32,
}

impl Default for Dab {
    fn default() -> Self {
        Dab {
            x: 0.0,
            y: 0.0,
            radius: 1.0,
            hardness: 1.0,
            flow: 1.0,
            cap: 1.0,
            pixel: false,
            tip: TipShape::Round,
            roundness: 1.0,
            angle: 0.0,
            grain: 0.0,
            grain_scale: 6.0,
        }
    }
}

/// Where an aliased dab of `radius` centred near `(x, y)` actually sits:
/// on a pixel centre when its width in pixels is odd, on a pixel corner
/// when even, so the stamped shape is symmetric.
pub fn snap_center(x: f32, y: f32, radius: f32) -> (f32, f32) {
    let side = (radius * 2.0).round().max(1.0) as i64;
    if side % 2 == 1 {
        (x.floor() + 0.5, y.floor() + 0.5)
    } else {
        (x.round(), y.round())
    }
}

impl Dab {
    /// The pixels this dab covers, as `(x0, y0, width, height, covered)`
    /// with rows of `width` flags. For outlining aliased brushes.
    pub fn footprint(&self) -> (i64, i64, u32, u32, Vec<bool>) {
        let shape = DabShape::new(self);
        let e = shape.extent + 1.0;
        let x0 = (shape.x - e).floor() as i64;
        let y0 = (shape.y - e).floor() as i64;
        let w = ((shape.x + e).ceil() as i64 - x0).max(1) as u32;
        let h = ((shape.y + e).ceil() as i64 - y0).max(1) as u32;
        let mut m = Mask::new(w, h);
        let d = Dab {
            x: self.x - x0 as f32,
            y: self.y - y0 as f32,
            flow: 1.0,
            cap: 1.0,
            grain: 0.0,
            ..*self
        };
        let mut dirty = vec![];
        m.stamp(&d, &mut dirty);
        let cov = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| m.value(x, y) > 0)
            .collect();
        (x0, y0, w, h, cov)
    }
}

/// Smooth value noise in 0..=1, fixed to canvas coordinates so the grain
/// stays put from stroke to stroke, like the tooth of real paper.
#[inline]
fn grain_at(x: f32, y: f32, scale: f32) -> f32 {
    #[inline]
    fn hash(x: i32, y: i32) -> f32 {
        let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841);
        h ^= h >> 13;
        h = h.wrapping_mul(0x85eb_ca6b);
        h ^= h >> 16;
        (h & 0xffff) as f32 / 65535.0
    }
    #[inline]
    fn octave(x: f32, y: f32) -> f32 {
        let (xi, yi) = (x.floor(), y.floor());
        let (fx, fy) = (x - xi, y - yi);
        let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
        let (xi, yi) = (xi as i32, yi as i32);
        let a = hash(xi, yi) + (hash(xi + 1, yi) - hash(xi, yi)) * sx;
        let b = hash(xi, yi + 1) + (hash(xi + 1, yi + 1) - hash(xi, yi + 1)) * sx;
        a + (b - a) * sy
    }
    let s = 1.0 / scale.max(0.5);
    let n = octave(x * s, y * s) * 0.65 + octave(x * s * 2.7 + 17.0, y * s * 2.7 + 5.0) * 0.35;
    // Stretch the middle so the texture has real peaks and valleys.
    ((n - 0.5) * 1.8 + 0.5).clamp(0.0, 1.0)
}

enum Kind {
    Round,
    Square,
    Image(Arc<TipImage>),
}

struct DabShape {
    x: f32,
    y: f32,
    r: f32,
    /// Half-size of the box that holds the whole tip.
    extent: f32,
    inner: f32,
    inv_soft: f32,
    flow: f32,
    cap: u32,
    aliased: bool,
    kind: Kind,
    /// Cos/sin of the angle and 1 / roundness.
    frame: (f32, f32, f32),
    grain: f32,
    grain_scale: f32,
}

impl DabShape {
    fn new(d: &Dab) -> Self {
        let r = d.radius.max(0.5);
        let (x, y) = if d.pixel {
            snap_center(d.x, d.y, r)
        } else {
            (d.x, d.y)
        };
        let inner = r * d.hardness.clamp(0.0, 1.0);
        let round = d.roundness.clamp(0.02, 1.0);
        let kind = match d.tip {
            TipShape::Round => Kind::Round,
            TipShape::Square => Kind::Square,
            TipShape::Image(id) => crate::tip::get(id).map_or(Kind::Round, Kind::Image),
        };
        let extent = match kind {
            Kind::Round => r,
            _ => r * std::f32::consts::SQRT_2,
        };
        DabShape {
            x,
            y,
            r,
            extent,
            inner,
            inv_soft: if r - inner > 1e-4 {
                1.0 / (r - inner)
            } else {
                0.0
            },
            flow: d.flow.clamp(0.0, 1.0),
            cap: (d.cap.clamp(0.0, 1.0) * 65535.0 + 0.5) as u32,
            aliased: d.pixel,
            kind,
            frame: (d.angle.cos(), d.angle.sin(), 1.0 / round),
            grain: d.grain.clamp(0.0, 1.0),
            grain_scale: d.grain_scale,
        }
    }

    fn stamp_tile(&self, c: TileCoord, px: [u32; 4], m: &mut MaskTile) {
        let simple = matches!(self.kind, Kind::Round)
            && !self.aliased
            && self.frame.2 <= 1.001
            && self.grain <= 0.0;
        if !simple {
            self.stamp_tile_general(c, px, m);
            return;
        }
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

    /// How much of the pixel centred at `(dx, dy)` from the dab's centre the
    /// tip covers, before flow and grain: 0..=1.
    #[inline]
    fn coverage(&self, dx: f32, dy: f32) -> f32 {
        let (ca, sa, k) = self.frame;
        // Into the tip's frame, short axis stretched to the long one.
        let u = dx * ca + dy * sa;
        let w = (-dx * sa + dy * ca) * k;
        let falloff = |d: f32| {
            let s = (1.0 - (d - self.inner) * self.inv_soft).clamp(0.0, 1.0);
            s * s * (3.0 - 2.0 * s)
        };
        match &self.kind {
            Kind::Round => {
                let d = (u * u + w * w).sqrt();
                let edge = if self.aliased {
                    if d < self.r { 1.0 } else { 0.0 }
                } else {
                    // Distance to the edge in screen pixels, for antialiasing.
                    let g = if d > 1e-4 {
                        ((u / d).powi(2) + (w * k / d).powi(2)).sqrt().max(1e-3)
                    } else {
                        1.0
                    };
                    ((self.r - d) / g + 0.5).clamp(0.0, 1.0)
                };
                if edge <= 0.0 {
                    0.0
                } else {
                    edge.min(falloff(d))
                }
            }
            Kind::Square => {
                let (au, aw) = (u.abs(), w.abs());
                let d = au.max(aw);
                let edge = if self.aliased {
                    if d < self.r { 1.0 } else { 0.0 }
                } else {
                    // Each pair of sides is a straight edge; `w` is stretched
                    // by `k`, so its distance shrinks back by it.
                    let eu = (self.r - au + 0.5).clamp(0.0, 1.0);
                    let ew = ((self.r - aw) / k + 0.5).clamp(0.0, 1.0);
                    eu.min(ew)
                };
                if edge <= 0.0 {
                    0.0
                } else {
                    edge.min(falloff(d))
                }
            }
            Kind::Image(img) => {
                // The image's longer side spans the tip's diameter.
                let (iw, ih) = (img.width() as f32, img.height() as f32);
                let step = iw.max(ih) / (2.0 * self.r);
                img.sample(
                    u * step + iw * 0.5,
                    w * step + ih * 0.5,
                    step * k.sqrt(),
                    !self.aliased,
                )
            }
        }
    }

    /// Shaped, aliased, image and/or textured tips.
    fn stamp_tile_general(&self, c: TileCoord, px: [u32; 4], m: &mut MaskTile) {
        let t = TILE as u32;
        let (bx, by) = (c.x * t, c.y * t);
        let lx0 = px[0].saturating_sub(bx).min(t) as usize;
        let ly0 = px[1].saturating_sub(by).min(t) as usize;
        let lx1 = (px[2].saturating_sub(bx)).min(t) as usize;
        let ly1 = (px[3].saturating_sub(by)).min(t) as usize;
        for ly in ly0..ly1 {
            let py = (by as usize + ly) as f32 + 0.5;
            let dy = py - self.y;
            let row = &mut m.v[ly * TILE..ly * TILE + TILE];
            for (lx, v) in row.iter_mut().enumerate().take(lx1).skip(lx0) {
                let pxx = (bx as usize + lx) as f32 + 0.5;
                let cov = self.coverage(pxx - self.x, dy);
                if cov <= 0.0 {
                    continue;
                }
                let f = cov * self.flow;
                let mut cap = self.cap as f32;
                if self.grain > 0.0 {
                    // The grain lowers how much paint a spot can ever hold, so
                    // going over it again doesn't fill the valleys.
                    let n = grain_at(pxx, py, self.grain_scale);
                    cap *= 1.0 - self.grain * (1.0 - n);
                }
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
    /// Pixel-art mode: no antialiasing. Each pixel is either inside the tip
    /// or not, dabs sit on the pixel grid and lines have no gaps; every
    /// other setting still applies.
    pub pixel: bool,
    /// In pixel mode, removes the doubled corner pixels of 1px lines.
    pub pixel_perfect: bool,
    /// The tip's outline.
    pub tip: TipShape,
    /// Tip shape: short axis over long axis (1 = round).
    pub roundness: f32,
    /// Tip angle in degrees (relative to the stroke when following it).
    pub angle: f32,
    /// Turn the tip with the direction of the stroke (a calligraphy nib).
    pub follow_direction: bool,
    /// Random variation per dab, each 0..=1.
    pub size_jitter: f32,
    pub angle_jitter: f32,
    pub opacity_jitter: f32,
    /// Random offset of each dab, as a fraction of the diameter.
    pub scatter: f32,
    /// Paper texture showing through, 0..=1, and its feature size in px.
    pub grain: f32,
    pub grain_scale: f32,
    /// Pen pressure response: four points `(input, output)` with rising
    /// input, joined by a smooth monotone curve.
    pub pressure_curve: [[f32; 2]; 4],
    /// Pressure also scales flow (soft build-up).
    pub pressure_flow: bool,
    /// How much moving fast thins the line, 0..=1. Gives a mouse some of
    /// the life of a pen.
    pub speed_size: f32,
    /// Length of the stroke's tapered start, in canvas pixels.
    pub taper: f32,
}

/// A straight-line pressure curve.
pub const LINEAR_CURVE: [[f32; 2]; 4] = [
    [0.0, 0.0],
    [1.0 / 3.0, 1.0 / 3.0],
    [2.0 / 3.0, 2.0 / 3.0],
    [1.0, 1.0],
];

/// Evaluates a pressure curve: flat outside its first and last points and a
/// monotone cubic (Fritsch–Carlson) in between, so it never overshoots.
pub fn eval_curve(pts: &[[f32; 2]; 4], x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= pts[0][0] {
        return pts[0][1];
    }
    if x >= pts[3][0] {
        return pts[3][1];
    }
    // Secant slopes, then tangents limited to keep monotone segments.
    let mut d = [0f32; 3];
    for i in 0..3 {
        let dx = (pts[i + 1][0] - pts[i][0]).max(1e-5);
        d[i] = (pts[i + 1][1] - pts[i][1]) / dx;
    }
    let mut m = [d[0], 0.0, 0.0, d[2]];
    for i in 1..3 {
        m[i] = if d[i - 1] * d[i] <= 0.0 {
            0.0
        } else {
            (d[i - 1] + d[i]) * 0.5
        };
    }
    for i in 0..3 {
        if d[i].abs() < 1e-6 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
        } else {
            let (a, b) = (m[i] / d[i], m[i + 1] / d[i]);
            let h = a * a + b * b;
            if h > 9.0 {
                let t = 3.0 / h.sqrt();
                m[i] = t * a * d[i];
                m[i + 1] = t * b * d[i];
            }
        }
    }
    let i = (0..3).find(|&i| x <= pts[i + 1][0]).unwrap_or(2);
    let (x0, x1) = (pts[i][0], pts[i + 1][0]);
    let h = (x1 - x0).max(1e-5);
    let t = (x - x0) / h;
    let (t2, t3) = (t * t, t * t * t);
    let y = (2.0 * t3 - 3.0 * t2 + 1.0) * pts[i][1]
        + (t3 - 2.0 * t2 + t) * h * m[i]
        + (-2.0 * t3 + 3.0 * t2) * pts[i + 1][1]
        + (t3 - t2) * h * m[i + 1];
    y.clamp(0.0, 1.0)
}

/// A pressure curve fitted to how someone actually presses: the light end
/// of their range maps to 0, the heavy end to 1 and their typical pressure
/// to the middle. `samples` are raw pressures from a test stroke.
pub fn fit_curve(samples: &[f32]) -> Option<[[f32; 2]; 4]> {
    let mut v: Vec<f32> = samples
        .iter()
        .copied()
        .filter(|p| p.is_finite() && *p > 0.0)
        .collect();
    if v.len() < 8 {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |q: f32| v[((v.len() - 1) as f32 * q).round() as usize];
    let lo = (at(0.05) * 0.8).clamp(0.0, 0.6);
    let hi = at(0.95).clamp(lo + 0.15, 1.0);
    let mid = at(0.5).clamp(lo + 0.05, hi - 0.05);
    let q3 = (mid + hi) * 0.5;
    Some([[lo, 0.0], [mid, 0.5], [q3, 0.78], [hi, 1.0]])
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
            tip: TipShape::Round,
            roundness: 1.0,
            angle: 0.0,
            follow_direction: false,
            size_jitter: 0.0,
            angle_jitter: 0.0,
            opacity_jitter: 0.0,
            scatter: 0.0,
            grain: 0.0,
            grain_scale: 6.0,
            pressure_curve: LINEAR_CURVE,
            pressure_flow: false,
            speed_size: 0.0,
            taper: 0.0,
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct Pt {
    x: f32,
    y: f32,
    p: f32,
    /// Size factor from speed, 0..=1.
    v: f32,
}

/// Turns a stream of pointer samples into evenly spaced dabs.
pub struct StrokeEngine {
    params: BrushParams,
    smoothed: Option<Pt>,
    last_raw: Option<Pt>,
    carry: f32,
    /// Distance travelled along the (smoothed) stroke.
    travelled: f32,
    /// Smoothed pointer speed, in diameters per sample.
    speed: f32,
    rng: u32,
    /// Pixel mode: last emitted pixel and pixels held back for the
    /// pixel-perfect corner test.
    last_px: Option<[i64; 5]>,
    held: Vec<Dab>,
}

impl StrokeEngine {
    pub fn new(params: BrushParams) -> Self {
        StrokeEngine {
            params,
            smoothed: None,
            last_raw: None,
            carry: 0.0,
            travelled: 0.0,
            speed: 0.0,
            rng: 0x9e37_79b9,
            last_px: None,
            held: vec![],
        }
    }

    pub fn params(&self) -> &BrushParams {
        &self.params
    }

    /// A random number in -1..=1.
    fn rand(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x >> 8) as f32 / (1u32 << 23) as f32 - 1.0
    }

    /// The size multiplier from pressure, speed and the entry taper, before
    /// any randomness (also used for spacing).
    fn size_scale(&self, q: Pt, dist: f32) -> f32 {
        let p = &self.params;
        let pr = eval_curve(&p.pressure_curve, q.p);
        let mut scale = if p.pressure_size {
            p.min_size + (1.0 - p.min_size) * pr
        } else {
            1.0
        };
        scale *= q.v;
        if p.taper > 0.5 {
            let t = (dist / p.taper).clamp(0.0, 1.0);
            scale *= 0.08 + 0.92 * t * (2.0 - t);
        }
        scale
    }

    fn dab_at(&mut self, q: Pt, dist: f32, heading: f32) -> Dab {
        let p = self.params;
        let pr = eval_curve(&p.pressure_curve, q.p);
        let mut scale = self.size_scale(q, dist);
        let mut cap = if p.pressure_opacity {
            p.opacity * pr
        } else {
            p.opacity
        };
        let mut flow = if p.pressure_flow {
            p.flow * (0.1 + 0.9 * pr)
        } else {
            p.flow
        };
        let (mut x, mut y) = (q.x, q.y);
        let mut angle = p.angle.to_radians();
        if p.follow_direction {
            angle += heading;
        }
        if p.size_jitter > 0.0 {
            scale *= 1.0 - p.size_jitter * 0.5 * (self.rand() + 1.0) * 0.9;
        }
        if p.angle_jitter > 0.0 {
            angle += self.rand() * p.angle_jitter * std::f32::consts::PI;
        }
        if p.opacity_jitter > 0.0 {
            let j = 1.0 - p.opacity_jitter * 0.5 * (self.rand() + 1.0);
            cap *= j;
            flow *= j;
        }
        if p.scatter > 0.0 {
            let r = p.size * scale * p.scatter;
            x += self.rand() * r;
            y += self.rand() * r;
        }
        Dab {
            x,
            y,
            radius: (p.size * 0.5 * scale).max(0.5),
            hardness: p.hardness,
            flow,
            cap,
            pixel: p.pixel,
            tip: p.tip,
            roundness: p.roundness,
            angle,
            grain: p.grain,
            grain_scale: p.grain_scale,
        }
    }

    /// Emits a pixel-mode dab, skipping repeats of the same footprint and
    /// (for 1px brushes with pixel-perfect on) the middle pixel of L-shaped
    /// corners.
    fn emit(&mut self, mut d: Dab, out: &mut Vec<Dab>) {
        if !d.pixel {
            out.push(d);
            return;
        }
        (d.x, d.y) = snap_center(d.x, d.y, d.radius.max(0.5));
        let q = |v: f32, k: f32| (v * k).round() as i64;
        let key = [
            q(d.x, 2.0),
            q(d.y, 2.0),
            q(d.radius, 8.0),
            q(d.angle, 128.0),
            q(d.cap, 256.0),
        ];
        if self.last_px == Some(key) {
            return;
        }
        self.last_px = Some(key);
        let perfect = self.params.pixel_perfect
            && (d.radius * 2.0).round() <= 1.0
            && self.params.scatter <= 0.0;
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

    fn spacing_at(&self, q: Pt, dist: f32) -> f32 {
        let d = self.params.size * self.size_scale(q, dist).max(0.0);
        let step = (d * self.params.spacing.max(0.01)).max(0.5);
        // Aliased lines stay gap-free, unless the spacing is set wide on
        // purpose (stamps along a path).
        if self.params.pixel && self.params.spacing < 0.3 {
            step.min(1.0)
        } else {
            step
        }
    }

    fn segment(&mut self, a: Pt, b: Pt, out: &mut Vec<Dab>) {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = (dx * dx + dy * dy).sqrt();
        if len <= 1e-6 {
            return;
        }
        let heading = dy.atan2(dx);
        let mut t = 0.0f32; // distance travelled along this segment
        let start = self.travelled;
        loop {
            let here = lerp_pt(a, b, t / len);
            let need = self.spacing_at(here, start + t) - self.carry;
            if t + need > len {
                self.carry += len - t;
                self.travelled = start + len;
                return;
            }
            t += need;
            self.carry = 0.0;
            let d = self.dab_at(lerp_pt(a, b, t / len), start + t, heading);
            self.emit(d, out);
        }
    }

    /// Adds a pointer sample (canvas coordinates, pressure 0..=1).
    pub fn add_point(&mut self, x: f32, y: f32, pressure: f32, out: &mut Vec<Dab>) {
        let mut raw = Pt {
            x,
            y,
            p: pressure,
            v: 1.0,
        };
        if let Some(last) = self.last_raw {
            let d = ((x - last.x).powi(2) + (y - last.y).powi(2)).sqrt();
            let v = d / self.params.size.max(1.0);
            self.speed = self.speed * 0.75 + v * 0.25;
        }
        let k = self.params.speed_size.clamp(0.0, 1.0);
        if k > 0.0 {
            raw.v = 1.0 - k * 0.85 * (1.0 - (-self.speed * 1.5).exp());
        }
        self.last_raw = Some(raw);
        let Some(prev) = self.smoothed else {
            // Seed the jitter from where the stroke starts, so every stroke
            // differs but a replay is identical.
            self.rng = (x.to_bits() ^ y.to_bits().rotate_left(16)) | 1;
            self.smoothed = Some(raw);
            let d = self.dab_at(raw, 0.0, 0.0);
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
            v: prev.v + (raw.v - prev.v) * w.max(0.5),
        };
        self.segment(prev, next, out);
        self.smoothed = Some(next);
    }

    /// Ends the stroke, letting the stabilised path catch up to the pen.
    pub fn finish(&mut self, out: &mut Vec<Dab>) {
        if let (Some(s), Some(r)) = (self.smoothed, self.last_raw) {
            self.segment(
                s,
                Pt {
                    p: s.p,
                    v: s.v,
                    ..r
                },
                out,
            );
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
        v: a.v + (b.v - a.v) * t,
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
            ..Default::default()
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
            ..Default::default()
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
                ..Default::default()
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

    #[test]
    fn pressure_curve_is_monotone_and_linear_by_default() {
        for i in 0..=20 {
            let x = i as f32 / 20.0;
            assert!((eval_curve(&LINEAR_CURVE, x) - x).abs() < 1e-4);
        }
        let c = [[0.1, 0.0], [0.3, 0.6], [0.5, 0.65], [0.8, 1.0]];
        let mut last = -1.0;
        for i in 0..=100 {
            let y = eval_curve(&c, i as f32 / 100.0);
            assert!(y >= last - 1e-6, "not monotone at {i}");
            last = y;
        }
        assert_eq!(eval_curve(&c, 0.05), 0.0);
        assert_eq!(eval_curve(&c, 0.9), 1.0);
        // A light-handed artist: pressures mostly around 0.2–0.4.
        let samples: Vec<f32> = (0..100).map(|i| 0.2 + 0.2 * (i as f32 / 99.0)).collect();
        let f = fit_curve(&samples).unwrap();
        assert!(eval_curve(&f, 0.3) > 0.4 && eval_curve(&f, 0.3) < 0.6);
        assert!(eval_curve(&f, 0.4) > 0.95);
    }

    #[test]
    fn elliptical_tip_and_grain() {
        let mut m = Mask::new(128, 128);
        let mut dirty = vec![];
        let dab = Dab {
            x: 64.0,
            y: 64.0,
            radius: 30.0,
            roundness: 0.25,
            angle: 0.0,
            ..Default::default()
        };
        m.stamp(&dab, &mut dirty);
        // Long along x, short along y.
        assert_eq!(m.value(90, 64), 65535);
        assert_eq!(m.value(64, 76), 0);
        assert_eq!(m.value(64, 68), 65535);
        // Rotated a quarter turn, it stands upright.
        let mut m = Mask::new(128, 128);
        m.stamp(
            &Dab {
                angle: std::f32::consts::FRAC_PI_2,
                ..dab
            },
            &mut dirty,
        );
        assert_eq!(m.value(64, 90), 65535);
        assert_eq!(m.value(76, 64), 0);
        // Grain: repeated dabs still leave texture behind.
        let mut m = Mask::new(128, 128);
        let g = Dab {
            roundness: 1.0,
            grain: 1.0,
            grain_scale: 4.0,
            ..dab
        };
        for _ in 0..30 {
            m.stamp(&g, &mut dirty);
        }
        let vals: Vec<u16> = (50..78).map(|x| m.value(x, 64)).collect();
        let (lo, hi) = (*vals.iter().min().unwrap(), *vals.iter().max().unwrap());
        assert!(hi - lo > 20000, "{lo} {hi}");
    }

    #[test]
    fn taper_and_speed_thin_the_stroke() {
        let base = BrushParams {
            size: 20.0,
            pressure_size: false,
            smoothing: 0.0,
            spacing: 0.1,
            ..Default::default()
        };
        let run = |p: BrushParams| {
            let mut e = StrokeEngine::new(p);
            let mut dabs = vec![];
            for i in 0..=40 {
                e.add_point(i as f32 * 5.0, 0.0, 1.0, &mut dabs);
            }
            e.finish(&mut dabs);
            dabs
        };
        let plain = run(base);
        assert!(plain.iter().all(|d| (d.radius - 10.0).abs() < 1e-3));
        let tapered = run(BrushParams {
            taper: 60.0,
            ..base
        });
        assert!(tapered[0].radius < 2.0);
        assert!((tapered.last().unwrap().radius - 10.0).abs() < 1e-3);
        let fast = run(BrushParams {
            speed_size: 1.0,
            ..base
        });
        assert!(fast.last().unwrap().radius < 8.0);
        // Following the stroke turns the tip with it.
        let mut e = StrokeEngine::new(BrushParams {
            follow_direction: true,
            roundness: 0.3,
            ..base
        });
        let mut dabs = vec![];
        e.add_point(0.0, 0.0, 1.0, &mut dabs);
        e.add_point(0.0, 50.0, 1.0, &mut dabs);
        assert!((dabs.last().unwrap().angle - std::f32::consts::FRAC_PI_2).abs() < 1e-3);
    }

    fn footprint_rows(d: Dab) -> Vec<String> {
        let (_, _, w, h, cov) = d.footprint();
        (0..h as usize)
            .map(|y| {
                (0..w as usize)
                    .map(|x| if cov[y * w as usize + x] { '#' } else { '.' })
                    .collect::<String>()
            })
            .filter(|r| r.contains('#'))
            .map(|r| r.trim_matches('.').to_string())
            .collect()
    }

    #[test]
    fn aliased_round_sizes() {
        let at = |size: f32| {
            footprint_rows(Dab {
                x: 20.3,
                y: 20.7,
                radius: size / 2.0,
                pixel: true,
                ..Default::default()
            })
        };
        assert_eq!(at(1.0), ["#"]);
        assert_eq!(at(2.0), ["##", "##"]);
        // Decimal sizes pick between the shapes a whole size can't.
        assert_eq!(at(3.0), ["###", "###", "###"]);
        assert_eq!(at(2.8), ["#", "###", "#"]);
        assert_eq!(at(4.0), ["##", "####", "####", "##"]);
        // Aliased coverage is all or nothing at the edge.
        let mut m = Mask::new(16, 16);
        let mut dirty = vec![];
        m.stamp(
            &Dab {
                x: 8.0,
                y: 8.0,
                radius: 4.0,
                pixel: true,
                ..Default::default()
            },
            &mut dirty,
        );
        for y in 0..16 {
            for x in 0..16 {
                assert!(matches!(m.value(x, y), 0 | 65535));
            }
        }
    }

    #[test]
    fn square_tip_with_roundness_and_angle() {
        let sq = Dab {
            x: 10.0,
            y: 10.0,
            radius: 2.0,
            pixel: true,
            tip: TipShape::Square,
            ..Default::default()
        };
        assert_eq!(footprint_rows(sq), ["####"; 4]);
        // Half as tall.
        assert_eq!(
            footprint_rows(Dab {
                roundness: 0.5,
                ..sq
            }),
            ["####"; 2]
        );
        // Turned 45°: a diamond.
        let d = footprint_rows(Dab {
            radius: 3.0,
            angle: std::f32::consts::FRAC_PI_4,
            ..sq
        });
        assert!(d.first().unwrap().len() < d[d.len() / 2].len(), "{d:?}");
        // Antialiased squares fill their corners, unlike circles.
        let mut m = Mask::new(64, 64);
        let mut dirty = vec![];
        m.stamp(
            &Dab {
                x: 32.0,
                y: 32.0,
                radius: 10.0,
                tip: TipShape::Square,
                ..Default::default()
            },
            &mut dirty,
        );
        assert_eq!(m.value(23, 23), 65535);
        assert_eq!(m.value(32, 43), 0);
    }

    #[test]
    fn image_tip_stamps_its_shape() {
        // A ring: covered border, empty middle.
        let n = 32u32;
        let data = (0..n * n)
            .map(|i| {
                let (x, y) = ((i % n) as f32 - 15.5, (i / n) as f32 - 15.5);
                let d = (x * x + y * y).sqrt();
                if (10.0..15.0).contains(&d) { 255 } else { 0 }
            })
            .collect();
        let id = crate::tip::register(crate::tip::TipImage::new(n, n, data));
        for pixel in [false, true] {
            let mut m = Mask::new(64, 64);
            let mut dirty = vec![];
            m.stamp(
                &Dab {
                    x: 32.0,
                    y: 32.0,
                    radius: 16.0,
                    pixel,
                    tip: TipShape::Image(id),
                    ..Default::default()
                },
                &mut dirty,
            );
            assert_eq!(m.value(32, 32), 0, "{pixel}");
            assert!(m.value(32 + 12, 32) > 60000, "{pixel}");
            assert_eq!(m.value(32 + 18, 32), 0, "{pixel}");
        }
    }

    #[test]
    fn pixel_mode_keeps_brush_features() {
        let base = BrushParams {
            size: 6.0,
            pixel: true,
            pressure_size: false,
            smoothing: 0.0,
            ..Default::default()
        };
        let run = |p: BrushParams| {
            let mut e = StrokeEngine::new(p);
            let mut dabs = vec![];
            for i in 0..=30 {
                e.add_point(i as f32 * 3.0, 10.0, 1.0, &mut dabs);
            }
            e.finish(&mut dabs);
            dabs
        };
        let plain = run(base);
        assert!(plain.iter().all(|d| d.flow == 1.0 && d.roundness == 1.0));
        // Dabs land on the pixel grid and repeats are dropped.
        assert!(plain.iter().all(|d| d.x == d.x.round()));
        assert!(plain.windows(2).all(|w| w[0].x != w[1].x));
        let p = run(BrushParams {
            flow: 0.5,
            roundness: 0.4,
            taper: 30.0,
            size_jitter: 0.5,
            tip: TipShape::Square,
            ..base
        });
        assert!(p.iter().all(|d| d.flow == 0.5 && d.roundness == 0.4));
        assert!(p.iter().all(|d| d.tip == TipShape::Square));
        assert!(p[0].radius < 1.0);
    }
}
