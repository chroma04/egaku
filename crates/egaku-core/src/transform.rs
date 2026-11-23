//! Affine resampling.
//!
//! Everything that moves pixels by more than whole-pixel offsets goes
//! through here: free transform, image objects, text, resizing. A source is
//! kept with a box-filtered mip pyramid, and each output pixel samples the
//! level nearest its footprint bilinearly. Shrinking a photo to a tenth of
//! its size therefore averages properly instead of aliasing, and because the
//! original is resampled every time, transforms never accumulate blur.
//! Nearest-neighbour mode keeps pixel art crisp.

use std::sync::{Arc, OnceLock};

use rayon::prelude::*;

use crate::tile::{Px, TILE, Tile, TileCoord, TileGrid, TileRect, tiles_for};

/// A 2D affine map: `(x, y) ↦ (a·x + c·y + e, b·x + d·y + f)`.
#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Affine {
    pub m: [f32; 6],
}

impl Default for Affine {
    fn default() -> Self {
        Affine::IDENTITY
    }
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        m: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    };

    pub fn translate(x: f32, y: f32) -> Affine {
        Affine {
            m: [1.0, 0.0, 0.0, 1.0, x, y],
        }
    }

    pub fn scale(sx: f32, sy: f32) -> Affine {
        Affine {
            m: [sx, 0.0, 0.0, sy, 0.0, 0.0],
        }
    }

    pub fn rotate(a: f32) -> Affine {
        let (s, c) = a.sin_cos();
        Affine {
            m: [c, s, -s, c, 0.0, 0.0],
        }
    }

    /// `self` applied after `o`.
    pub fn then(self, after: Affine) -> Affine {
        after.mul(self)
    }

    /// Matrix product `self · o` (apply `o` first).
    pub fn mul(self, o: Affine) -> Affine {
        let [a, b, c, d, e, f] = self.m;
        let [oa, ob, oc, od, oe, of] = o.m;
        Affine {
            m: [
                a * oa + c * ob,
                b * oa + d * ob,
                a * oc + c * od,
                b * oc + d * od,
                a * oe + c * of + e,
                b * oe + d * of + f,
            ],
        }
    }

    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.m;
        (a * x + c * y + e, b * x + d * y + f)
    }

    pub fn det(&self) -> f32 {
        self.m[0] * self.m[3] - self.m[1] * self.m[2]
    }

    pub fn inverse(&self) -> Option<Affine> {
        let det = self.det();
        if det.abs() < 1e-9 {
            return None;
        }
        let [a, b, c, d, e, f] = self.m;
        let id = 1.0 / det;
        Some(Affine {
            m: [
                d * id,
                -b * id,
                -c * id,
                a * id,
                (c * f - d * e) * id,
                (b * e - a * f) * id,
            ],
        })
    }

    /// Lengths of the transformed unit vectors (horizontal, vertical scale).
    pub fn scales(&self) -> (f32, f32) {
        let [a, b, c, d, ..] = self.m;
        ((a * a + b * b).sqrt(), (c * c + d * d).sqrt())
    }

    /// Rotation angle of the transformed x axis.
    pub fn angle(&self) -> f32 {
        self.m[1].atan2(self.m[0])
    }

    pub fn is_translation(&self) -> bool {
        let [a, b, c, d, ..] = self.m;
        (a - 1.0).abs() < 1e-6 && b.abs() < 1e-6 && c.abs() < 1e-6 && (d - 1.0).abs() < 1e-6
    }
}

/// An image to resample, with its mip pyramid.
pub struct Source {
    pub width: u32,
    pub height: u32,
    /// The original pixels.
    pub base: TileGrid,
    /// Successive halvings, built the first time a smooth downscale needs
    /// them.
    mips: OnceLock<Vec<TileGrid>>,
    /// Pixel bounds of the content in level 0: `x0, y0, x1, y1`.
    pub bounds: (u32, u32, u32, u32),
}

impl Source {
    pub fn new(grid: TileGrid, width: u32, height: u32) -> Source {
        let t = TILE as u32;
        let bounds = grid.bounds().map_or((0, 0, 0, 0), |b| {
            (b.x0 * t, b.y0 * t, (b.x1 * t).min(width), (b.y1 * t).min(height))
        });
        Source {
            width,
            height,
            base: grid,
            mips: OnceLock::new(),
            bounds,
        }
    }

    fn mips(&self) -> &[TileGrid] {
        self.mips.get_or_init(|| {
            let mut levels: Vec<TileGrid> = vec![];
            let (mut w, mut h) = (self.width, self.height);
            while w > 64 || h > 64 {
                let next = downsample_grid(levels.last().unwrap_or(&self.base), w, h);
                w = w.div_ceil(2);
                h = h.div_ceil(2);
                levels.push(next);
            }
            levels
        })
    }

    fn level(&self, l: usize) -> &TileGrid {
        if l == 0 { &self.base } else { &self.mips()[l - 1] }
    }

    /// Builds a source from straight-alpha RGBA rows.
    pub fn from_rgba(w: u32, h: u32, rgba: &[u8]) -> Source {
        let grid = crate::io::grid_from_rgba(w, h, rgba, 0, 0, w, h);
        Source::new(grid, w, h)
    }

    /// The source's content rectangle mapped through `m`, in canvas tiles.
    pub fn dest_tiles(&self, m: &Affine, cols: u32, rows: u32) -> TileRect {
        let (x0, y0, x1, y1) = self.bounds;
        if x0 >= x1 || y0 >= y1 {
            return TileRect {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
            };
        }
        let pts = [
            m.apply(x0 as f32, y0 as f32),
            m.apply(x1 as f32, y0 as f32),
            m.apply(x0 as f32, y1 as f32),
            m.apply(x1 as f32, y1 as f32),
        ];
        let minx = pts.iter().map(|p| p.0).fold(f32::MAX, f32::min);
        let maxx = pts.iter().map(|p| p.0).fold(f32::MIN, f32::max);
        let miny = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min);
        let maxy = pts.iter().map(|p| p.1).fold(f32::MIN, f32::max);
        TileRect::from_px(
            minx.floor() as i64 - 1,
            miny.floor() as i64 - 1,
            maxx.ceil() as i64 + 1,
            maxy.ceil() as i64 + 1,
            cols,
            rows,
        )
    }

    /// Renders one canvas tile of the source mapped through `m` (source →
    /// canvas). `inv` must be `m`'s inverse.
    pub fn render_tile(&self, m: &Affine, inv: &Affine, smooth: bool, c: TileCoord) -> Option<Tile> {
        let (sx, sy) = m.scales();
        // Source pixels per canvas pixel decides the pyramid level.
        let footprint = 1.0 / sx.min(sy).max(1e-6);
        let level = if smooth && footprint >= 2.0 {
            (footprint.log2().floor() as usize).min(self.mips().len())
        } else {
            0
        };
        let grid = self.level(level);
        let k = (1u32 << level) as f32;
        let (bx0, by0, bx1, by1) = self.bounds;
        let (lw, lh) = (
            (bx1 as f32 / k).ceil() as i64,
            (by1 as f32 / k).ceil() as i64,
        );
        let (lx0, ly0) = ((bx0 as f32 / k).floor() as i64, (by0 as f32 / k).floor() as i64);
        let mut out = Tile::filled([0; 4]);
        let mut any = false;
        let (ox, oy) = (c.x as f32 * TILE as f32, c.y as f32 * TILE as f32);
        for y in 0..TILE {
            for x in 0..TILE {
                let (u, v) = inv.apply(ox + x as f32 + 0.5, oy + y as f32 + 0.5);
                let p = if smooth {
                    // Pixel centres are at +0.5 in each level.
                    let (u, v) = (u / k - 0.5, v / k - 0.5);
                    if u < lx0 as f32 - 1.0 || v < ly0 as f32 - 1.0 || u > lw as f32 || v > lh as f32 {
                        continue;
                    }
                    bilinear(grid, u, v)
                } else {
                    let (u, v) = (u.floor() as i64, v.floor() as i64);
                    if u < bx0 as i64 || v < by0 as i64 || u >= bx1 as i64 || v >= by1 as i64 {
                        continue;
                    }
                    grid.pixel(u, v)
                };
                if p[3] != 0 {
                    any = true;
                    out.px[y * TILE + x] = p;
                }
            }
        }
        any.then_some(out)
    }

    /// Renders the whole source mapped through `m` into a canvas-sized grid.
    pub fn render(&self, m: &Affine, smooth: bool, width: u32, height: u32) -> TileGrid {
        let mut g = TileGrid::new(width, height);
        let Some(inv) = m.inverse() else { return g };
        let rect = self.dest_tiles(m, g.cols(), g.rows());
        let coords: Vec<TileCoord> = rect.iter().collect();
        let tiles: Vec<(TileCoord, Option<Tile>)> = coords
            .par_iter()
            .map(|&c| (c, self.render_tile(m, &inv, smooth, c)))
            .collect();
        for (c, t) in tiles {
            if let Some(t) = t {
                g.set(c, Some(Arc::new(t)));
            }
        }
        // Partial tiles at the canvas edge must not carry pixels outside it.
        crop_to_canvas(&mut g, width, height);
        g
    }
}

/// Zeroes pixels beyond the canvas in edge tiles.
pub fn crop_to_canvas(g: &mut TileGrid, width: u32, height: u32) {
    let t = TILE as u32;
    if width % t == 0 && height % t == 0 {
        return;
    }
    let coords: Vec<TileCoord> = g
        .iter()
        .map(|(c, _)| c)
        .filter(|c| (c.x + 1) * t > width || (c.y + 1) * t > height)
        .collect();
    for c in coords {
        let tile = g.get_mut_or_insert(c);
        for y in 0..TILE {
            for x in 0..TILE {
                if c.x * t + x as u32 >= width || c.y * t + y as u32 >= height {
                    tile.px[y * TILE + x] = [0; 4];
                }
            }
        }
    }
    let all: Vec<TileCoord> = g.iter().map(|(c, _)| c).collect();
    g.prune(all);
}

#[inline]
fn bilinear(g: &TileGrid, u: f32, v: f32) -> Px {
    let (x0, y0) = (u.floor(), v.floor());
    let (fx, fy) = (u - x0, v - y0);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let p00 = g.pixel(x0, y0);
    let p10 = g.pixel(x0 + 1, y0);
    let p01 = g.pixel(x0, y0 + 1);
    let p11 = g.pixel(x0 + 1, y0 + 1);
    let w00 = (1.0 - fx) * (1.0 - fy);
    let w10 = fx * (1.0 - fy);
    let w01 = (1.0 - fx) * fy;
    let w11 = fx * fy;
    let mut o = [0u8; 4];
    for i in 0..4 {
        let v = p00[i] as f32 * w00 + p10[i] as f32 * w10 + p01[i] as f32 * w01 + p11[i] as f32 * w11;
        o[i] = (v + 0.5) as u8;
    }
    // Premultiplied colour can't exceed alpha; guard rounding.
    for i in 0..3 {
        o[i] = o[i].min(o[3]);
    }
    o
}

/// Halves a grid with a box filter.
pub fn downsample_grid(src: &TileGrid, w: u32, h: u32) -> TileGrid {
    let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = TileGrid::new(nw, nh);
    let (cols, rows) = (tiles_for(nw), tiles_for(nh));
    let coords: Vec<TileCoord> = (0..rows)
        .flat_map(|y| (0..cols).map(move |x| TileCoord::new(x, y)))
        .filter(|c| {
            // Any of the four source tiles present?
            (0..2).any(|dy| {
                (0..2).any(|dx| {
                    src.get_checked((c.x * 2 + dx) as i64, (c.y * 2 + dy) as i64)
                        .is_some()
                })
            })
        })
        .collect();
    let tiles: Vec<(TileCoord, Option<Tile>)> = coords
        .par_iter()
        .map(|&c| {
            let mut t = Tile::filled([0; 4]);
            let mut any = false;
            for y in 0..TILE {
                for x in 0..TILE {
                    let sx = (c.x as i64 * TILE as i64 + x as i64) * 2;
                    let sy = (c.y as i64 * TILE as i64 + y as i64) * 2;
                    let ps = [
                        src.pixel(sx, sy),
                        src.pixel(sx + 1, sy),
                        src.pixel(sx, sy + 1),
                        src.pixel(sx + 1, sy + 1),
                    ];
                    let mut o = [0u8; 4];
                    for ch in 0..4 {
                        o[ch] = ((ps.iter().map(|p| p[ch] as u32).sum::<u32>() + 2) / 4) as u8;
                    }
                    any |= o[3] != 0;
                    t.px[y * TILE + x] = o;
                }
            }
            (c, any.then_some(t))
        })
        .collect();
    for (c, t) in tiles {
        if let Some(t) = t {
            out.set(c, Some(Arc::new(t)));
        }
    }
    out
}

/// Resamples a whole grid to a new size (for image resize).
pub fn resize_grid(src: &TileGrid, w: u32, h: u32, nw: u32, nh: u32, smooth: bool) -> TileGrid {
    let s = Source::new(src.clone(), w, h);
    let m = Affine::scale(nw as f32 / w as f32, nh as f32 / h as f32);
    s.render(&m, smooth, nw, nh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_inverse() {
        let m = Affine::translate(10.0, -3.0)
            .mul(Affine::rotate(0.4))
            .mul(Affine::scale(2.0, 0.5));
        let inv = m.inverse().unwrap();
        let (x, y) = m.apply(7.0, 9.0);
        let (u, v) = inv.apply(x, y);
        assert!((u - 7.0).abs() < 1e-3 && (v - 9.0).abs() < 1e-3);
        assert!((m.scales().0 - 2.0).abs() < 1e-4);
        assert!((m.angle() - 0.4).abs() < 1e-4);
    }

    #[test]
    fn identity_render_is_exact() {
        let mut g = TileGrid::new(150, 100);
        for y in 0..100 {
            for x in 0..150 {
                let t = g.get_mut_or_insert(TileCoord::new(x / 64, y / 64));
                t.px[(y as usize % 64) * 64 + x as usize % 64] = [x as u8, y as u8, 7, 255];
            }
        }
        let s = Source::new(g.clone(), 150, 100);
        for smooth in [false, true] {
            let r = s.render(&Affine::IDENTITY, smooth, 150, 100);
            for (x, y) in [(0, 0), (149, 99), (70, 33)] {
                assert_eq!(r.pixel(x, y), g.pixel(x, y), "{smooth} {x},{y}");
            }
        }
        // Integer translation, nearest.
        let r = s.render(&Affine::translate(5.0, 3.0), false, 150, 100);
        assert_eq!(r.pixel(15, 13), [10, 10, 7, 255]);
        assert_eq!(r.pixel(2, 2), [0; 4]);
    }

    #[test]
    fn downscale_averages() {
        // A fine checkerboard shrunk 8× should be mid-grey, not aliased.
        let mut g = TileGrid::new(256, 256);
        for y in 0..256 {
            for x in 0..256 {
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                let t = g.get_mut_or_insert(TileCoord::new(x / 64, y / 64));
                t.px[(y as usize % 64) * 64 + x as usize % 64] = [v, v, v, 255];
            }
        }
        let r = resize_grid(&g, 256, 256, 32, 32, true);
        let p = r.pixel(16, 16);
        assert!((p[0] as i32 - 128).abs() < 8, "{p:?}");
        assert_eq!(p[3], 255);
    }
}
