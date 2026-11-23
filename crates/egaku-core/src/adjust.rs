//! Adjustments and filters.
//!
//! Each one is a function from a layer to new pixels for one tile, so the
//! editor can preview it live on just the visible tiles (the same lazy path
//! the compositor uses for strokes), restrict it to the selection, and
//! commit it as one undo step.

use crate::color::{premultiply, unpremultiply};
use crate::tile::{Px, TILE, Tile, TileCoord, TileGrid};

/// Something that computes new pixels for a tile of a layer.
pub trait Filter: Send + Sync {
    /// How far (in pixels) outside a tile the filter reads, which is also how
    /// far it can spread paint into empty tiles.
    fn margin(&self) -> u32 {
        0
    }
    /// New contents of tile `c`, or `None` if it ends up empty.
    fn apply(&self, src: &TileGrid, c: TileCoord) -> Option<Tile>;
    /// Whether the filter can paint anywhere, not just near existing pixels.
    fn full_canvas(&self) -> bool {
        false
    }
}

/// A linear or radial gradient drawn over the layer.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Gradient {
    pub radial: bool,
    pub from: (f32, f32),
    pub to: (f32, f32),
    /// Straight-alpha colours at the start and end.
    pub c0: [u8; 4],
    pub c1: [u8; 4],
    pub opacity: f32,
}

impl Filter for Gradient {
    fn full_canvas(&self) -> bool {
        true
    }

    fn apply(&self, src: &TileGrid, c: TileCoord) -> Option<Tile> {
        let (dx, dy) = (self.to.0 - self.from.0, self.to.1 - self.from.1);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        let len = len2.sqrt();
        let p0 = premultiply(self.c0).map(|v| v as f32);
        let p1 = premultiply(self.c1).map(|v| v as f32);
        let op = self.opacity.clamp(0.0, 1.0);
        let mut g = Tile::filled([0; 4]);
        let (ox, oy) = (c.x as f32 * TILE as f32, c.y as f32 * TILE as f32);
        for y in 0..TILE {
            for x in 0..TILE {
                let (px, py) = (
                    ox + x as f32 + 0.5 - self.from.0,
                    oy + y as f32 + 0.5 - self.from.1,
                );
                let t = if self.radial {
                    (px * px + py * py).sqrt() / len
                } else {
                    (px * dx + py * dy) / len2
                }
                .clamp(0.0, 1.0);
                // A 4×4 ordered dither hides banding in long, subtle ramps.
                const B: [f32; 16] = [
                    0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0,
                    13.0, 5.0,
                ];
                let d = B[(y % 4) * 4 + x % 4] / 16.0 - 0.47;
                let mut q = [0u8; 4];
                for ch in 0..4 {
                    q[ch] = ((p0[ch] + (p1[ch] - p0[ch]) * t) * op + d).clamp(0.0, 255.0) as u8;
                }
                for ch in 0..3 {
                    q[ch] = q[ch].min(q[3]);
                }
                g.px[y * TILE + x] = q;
            }
        }
        let mut out = src
            .get(c)
            .map_or_else(|| Tile::filled([0; 4]), |t| (**t).clone());
        crate::blend::blend_tile(&mut out, &g, 255, crate::blend::BlendMode::Normal);
        if out.is_empty() { None } else { Some(out) }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Adjustment {
    /// Both -1..=1.
    BrightnessContrast {
        brightness: f32,
        contrast: f32,
    },
    /// Hue in degrees, saturation and lightness -1..=1.
    HueSaturation {
        hue: f32,
        saturation: f32,
        lightness: f32,
    },
    /// Input black/white points and output range in 0..=1, gamma > 0.
    Levels {
        black: f32,
        white: f32,
        gamma: f32,
        out_black: f32,
        out_white: f32,
    },
    /// White balance and vibrance, -1..=1.
    Color {
        temperature: f32,
        tint: f32,
        vibrance: f32,
    },
    Invert,
    Desaturate,
    /// Pixels brighter than the level become white, others black.
    Threshold {
        level: f32,
    },
    /// Number of tones per channel.
    Posterize {
        levels: u32,
    },
    /// Gaussian-like blur radius in pixels.
    Blur {
        radius: f32,
    },
    /// Unsharp mask.
    Sharpen {
        amount: f32,
        radius: f32,
    },
    /// Film grain, 0..=1.
    Noise {
        amount: f32,
        mono: bool,
    },
    /// Block size in pixels.
    Pixelate {
        size: u32,
    },
}

impl Adjustment {
    pub fn name(&self) -> &'static str {
        match self {
            Adjustment::BrightnessContrast { .. } => "Brightness / Contrast",
            Adjustment::HueSaturation { .. } => "Hue / Saturation",
            Adjustment::Levels { .. } => "Levels",
            Adjustment::Color { .. } => "Colour Balance",
            Adjustment::Invert => "Invert",
            Adjustment::Desaturate => "Desaturate",
            Adjustment::Threshold { .. } => "Threshold",
            Adjustment::Posterize { .. } => "Posterize",
            Adjustment::Blur { .. } => "Blur",
            Adjustment::Sharpen { .. } => "Sharpen",
            Adjustment::Noise { .. } => "Add Noise",
            Adjustment::Pixelate { .. } => "Pixelate",
        }
    }

    /// Whether the adjustment has settings worth a dialog.
    pub fn has_params(&self) -> bool {
        !matches!(self, Adjustment::Invert | Adjustment::Desaturate)
    }

    /// Sensible starting settings.
    pub fn defaults() -> [Adjustment; 12] {
        [
            Adjustment::BrightnessContrast {
                brightness: 0.0,
                contrast: 0.0,
            },
            Adjustment::HueSaturation {
                hue: 0.0,
                saturation: 0.0,
                lightness: 0.0,
            },
            Adjustment::Levels {
                black: 0.0,
                white: 1.0,
                gamma: 1.0,
                out_black: 0.0,
                out_white: 1.0,
            },
            Adjustment::Color {
                temperature: 0.0,
                tint: 0.0,
                vibrance: 0.0,
            },
            Adjustment::Invert,
            Adjustment::Desaturate,
            Adjustment::Threshold { level: 0.5 },
            Adjustment::Posterize { levels: 4 },
            Adjustment::Blur { radius: 4.0 },
            Adjustment::Sharpen {
                amount: 0.8,
                radius: 2.0,
            },
            Adjustment::Noise {
                amount: 0.15,
                mono: true,
            },
            Adjustment::Pixelate { size: 8 },
        ]
    }

    /// Per-channel lookup table, for adjustments that are one.
    fn lut(&self) -> Option<[u8; 256]> {
        let f: Box<dyn Fn(f32) -> f32> = match *self {
            Adjustment::BrightnessContrast {
                brightness,
                contrast,
            } => {
                // Contrast pivots around mid-grey; brightness shifts.
                let k = if contrast >= 0.0 {
                    1.0 / (1.0 - contrast.min(0.99))
                } else {
                    1.0 + contrast
                };
                Box::new(move |v| (v - 0.5) * k + 0.5 + brightness * 0.5)
            }
            Adjustment::Levels {
                black,
                white,
                gamma,
                out_black,
                out_white,
            } => {
                let span = (white - black).max(1e-3);
                let g = 1.0 / gamma.max(0.01);
                Box::new(move |v| {
                    let t = ((v - black) / span).clamp(0.0, 1.0).powf(g);
                    out_black + t * (out_white - out_black)
                })
            }
            Adjustment::Invert => Box::new(|v| 1.0 - v),
            Adjustment::Posterize { levels } => {
                let n = levels.clamp(2, 255) as f32 - 1.0;
                Box::new(move |v| (v * n).round() / n)
            }
            _ => return None,
        };
        let mut lut = [0u8; 256];
        for (i, o) in lut.iter_mut().enumerate() {
            *o = (f(i as f32 / 255.0).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
        Some(lut)
    }

    /// Adjusts one straight-alpha colour.
    fn pixel(&self, c: [u8; 3], x: i64, y: i64) -> [u8; 3] {
        let f = |v: u8| v as f32 / 255.0;
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let (r, g, b) = (f(c[0]), f(c[1]), f(c[2]));
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        match *self {
            Adjustment::HueSaturation {
                hue,
                saturation,
                lightness,
            } => {
                let mut hsl = rgb_to_hsl(r, g, b);
                hsl.0 = (hsl.0 + hue / 360.0).rem_euclid(1.0);
                hsl.1 = if saturation >= 0.0 {
                    hsl.1 + (1.0 - hsl.1) * saturation * hsl.1.sqrt()
                } else {
                    hsl.1 * (1.0 + saturation)
                };
                hsl.2 = if lightness >= 0.0 {
                    hsl.2 + (1.0 - hsl.2) * lightness
                } else {
                    hsl.2 * (1.0 + lightness)
                };
                let (r, g, b) = hsl_to_rgb(hsl.0, hsl.1.clamp(0.0, 1.0), hsl.2);
                [q(r), q(g), q(b)]
            }
            Adjustment::Color {
                temperature,
                tint,
                vibrance,
            } => {
                let mut r = r * (1.0 + temperature * 0.25);
                let mut b = b * (1.0 - temperature * 0.25);
                let g = g * (1.0 - tint * 0.2);
                r *= 1.0 + tint * 0.1;
                b *= 1.0 + tint * 0.1;
                // Vibrance boosts low-saturation colours most.
                let mx = r.max(g).max(b);
                let mn = r.min(g).min(b);
                let sat = if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
                let k = 1.0 + vibrance * (1.0 - sat);
                let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                [q(l + (r - l) * k), q(l + (g - l) * k), q(l + (b - l) * k)]
            }
            Adjustment::Desaturate => {
                let l = q(lum);
                [l, l, l]
            }
            Adjustment::Threshold { level } => {
                let v = if lum >= level { 255 } else { 0 };
                [v, v, v]
            }
            Adjustment::Noise { amount, mono } => {
                let h = |k: u64| {
                    let mut z = (x as u64).wrapping_mul(0x9E3779B97F4A7C15)
                        ^ (y as u64).wrapping_mul(0xC2B2AE3D27D4EB4F)
                        ^ k.wrapping_mul(0x165667B19E3779F9);
                    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
                    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
                    ((z >> 40) as f32 / (1u64 << 24) as f32) - 0.5
                };
                let n = [h(1), h(2), h(3)];
                let a = amount * 0.6;
                if mono {
                    [q(r + n[0] * a), q(g + n[0] * a), q(b + n[0] * a)]
                } else {
                    [q(r + n[0] * a), q(g + n[1] * a), q(b + n[2] * a)]
                }
            }
            _ => c,
        }
    }
}

impl Filter for Adjustment {
    fn margin(&self) -> u32 {
        match *self {
            Adjustment::Blur { radius } => blur_margin(radius),
            Adjustment::Sharpen { radius, .. } => blur_margin(radius),
            Adjustment::Pixelate { size } => size.max(1),
            _ => 0,
        }
    }

    fn apply(&self, src: &TileGrid, c: TileCoord) -> Option<Tile> {
        match *self {
            Adjustment::Blur { radius } => return blur_tile(src, c, radius),
            Adjustment::Sharpen { amount, radius } => {
                let orig = src.get(c)?;
                let blurred = blur_tile(src, c, radius).unwrap_or_else(|| Tile::filled([0; 4]));
                let mut out = (**orig).clone();
                for (o, b) in out.px.iter_mut().zip(blurred.px.iter()) {
                    let a = o[3];
                    for ch in 0..3 {
                        let v = o[ch] as f32 + (o[ch] as f32 - b[ch] as f32) * amount;
                        o[ch] = v.clamp(0.0, a as f32) as u8;
                    }
                }
                return Some(out);
            }
            Adjustment::Pixelate { size } => return pixelate_tile(src, c, size.max(1)),
            _ => {}
        }
        let t = src.get(c)?;
        let mut out = (**t).clone();
        if let Some(lut) = self.lut() {
            for p in out.px.iter_mut() {
                if p[3] == 0 {
                    continue;
                }
                let u = unpremultiply(*p);
                *p = premultiply([
                    lut[u[0] as usize],
                    lut[u[1] as usize],
                    lut[u[2] as usize],
                    u[3],
                ]);
            }
        } else {
            let (ox, oy) = (c.x as i64 * TILE as i64, c.y as i64 * TILE as i64);
            for (i, p) in out.px.iter_mut().enumerate() {
                if p[3] == 0 {
                    continue;
                }
                let u = unpremultiply(*p);
                let n = self.pixel(
                    [u[0], u[1], u[2]],
                    ox + (i % TILE) as i64,
                    oy + (i / TILE) as i64,
                );
                *p = premultiply([n[0], n[1], n[2], u[3]]);
            }
        }
        Some(out)
    }
}

fn blur_box(radius: f32) -> usize {
    ((radius.max(0.0) / 3f32.sqrt()).round() as usize).max(1)
}

fn blur_margin(radius: f32) -> u32 {
    (blur_box(radius) * 3 + 1) as u32
}

/// Reads a padded square around a tile as f32 premultiplied channels.
fn gather(src: &TileGrid, c: TileCoord, m: usize) -> (Vec<[f32; 4]>, usize) {
    let n = TILE + 2 * m;
    let (ox, oy) = (
        c.x as i64 * TILE as i64 - m as i64,
        c.y as i64 * TILE as i64 - m as i64,
    );
    let mut buf = vec![[0f32; 4]; n * n];
    for y in 0..n {
        for x in 0..n {
            let p = src.pixel(ox + x as i64, oy + y as i64);
            buf[y * n + x] = [p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32];
        }
    }
    (buf, n)
}

fn blur_tile(src: &TileGrid, c: TileCoord, radius: f32) -> Option<Tile> {
    let br = blur_box(radius);
    let m = br * 3 + 1;
    let (mut buf, n) = gather(src, c, m);
    if buf.iter().all(|p| p[3] == 0.0) {
        return None;
    }
    let mut tmp = vec![[0f32; 4]; n * n];
    let inv = 1.0 / (2 * br + 1) as f32;
    for _ in 0..3 {
        // Horizontal pass into tmp, vertical back into buf.
        for y in 0..n {
            let row = &buf[y * n..(y + 1) * n];
            let get = |x: i64| row[x.clamp(0, n as i64 - 1) as usize];
            let mut acc = [0f32; 4];
            for k in -(br as i64)..=br as i64 {
                let p = get(k);
                for ch in 0..4 {
                    acc[ch] += p[ch];
                }
            }
            for x in 0..n {
                tmp[y * n + x] = acc.map(|v| v * inv);
                let (a, b) = (get(x as i64 + br as i64 + 1), get(x as i64 - br as i64));
                for ch in 0..4 {
                    acc[ch] += a[ch] - b[ch];
                }
            }
        }
        for x in 0..n {
            let get = |y: i64| tmp[y.clamp(0, n as i64 - 1) as usize * n + x];
            let mut acc = [0f32; 4];
            for k in -(br as i64)..=br as i64 {
                let p = get(k);
                for ch in 0..4 {
                    acc[ch] += p[ch];
                }
            }
            for y in 0..n {
                buf[y * n + x] = acc.map(|v| v * inv);
                let (a, b) = (get(y as i64 + br as i64 + 1), get(y as i64 - br as i64));
                for ch in 0..4 {
                    acc[ch] += a[ch] - b[ch];
                }
            }
        }
    }
    let mut out = Tile::filled([0; 4]);
    let mut any = false;
    for y in 0..TILE {
        for x in 0..TILE {
            let p = buf[(y + m) * n + x + m];
            let a = (p[3] + 0.5).min(255.0) as u8;
            let q: Px = [
                (p[0] + 0.5).min(a as f32) as u8,
                (p[1] + 0.5).min(a as f32) as u8,
                (p[2] + 0.5).min(a as f32) as u8,
                a,
            ];
            any |= a != 0;
            out.px[y * TILE + x] = q;
        }
    }
    any.then_some(out)
}

fn pixelate_tile(src: &TileGrid, c: TileCoord, s: u32) -> Option<Tile> {
    let s = s as i64;
    let (ox, oy) = (c.x as i64 * TILE as i64, c.y as i64 * TILE as i64);
    let mut out = Tile::filled([0; 4]);
    let mut any = false;
    let bx0 = ox.div_euclid(s);
    let by0 = oy.div_euclid(s);
    let bx1 = (ox + TILE as i64 - 1).div_euclid(s);
    let by1 = (oy + TILE as i64 - 1).div_euclid(s);
    for by in by0..=by1 {
        for bx in bx0..=bx1 {
            let mut acc = [0u64; 4];
            for y in by * s..(by + 1) * s {
                for x in bx * s..(bx + 1) * s {
                    let p = src.pixel(x, y);
                    for ch in 0..4 {
                        acc[ch] += p[ch] as u64;
                    }
                }
            }
            let n = (s * s) as u64;
            let avg: Px = acc.map(|v| ((v + n / 2) / n) as u8);
            if avg[3] == 0 {
                continue;
            }
            any = true;
            for y in (by * s).max(oy)..((by + 1) * s).min(oy + TILE as i64) {
                for x in (bx * s).max(ox)..((bx + 1) * s).min(ox + TILE as i64) {
                    out.px[(y - oy) as usize * TILE + (x - ox) as usize] = avg;
                }
            }
        }
    }
    any.then_some(out)
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let l = (mx + mn) * 0.5;
    let d = mx - mn;
    if d <= 1e-6 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-6);
    let h = if mx == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } / 6.0;
    (h, s.min(1.0), l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h * 6.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c * 0.5;
    (r + m, g + m, b + m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn grid_with(p: Px) -> TileGrid {
        let mut g = TileGrid::new(128, 128);
        g.set(TileCoord::new(0, 0), Some(Arc::new(Tile::filled(p))));
        g
    }

    #[test]
    fn identity_settings_are_identity() {
        let g = grid_with([120, 60, 30, 255]);
        for a in [
            Adjustment::BrightnessContrast {
                brightness: 0.0,
                contrast: 0.0,
            },
            Adjustment::HueSaturation {
                hue: 0.0,
                saturation: 0.0,
                lightness: 0.0,
            },
            Adjustment::Levels {
                black: 0.0,
                white: 1.0,
                gamma: 1.0,
                out_black: 0.0,
                out_white: 1.0,
            },
        ] {
            let t = a.apply(&g, TileCoord::new(0, 0)).unwrap();
            let p = t.px[100];
            for ch in 0..3 {
                assert!((p[ch] as i32 - [120, 60, 30][ch]).abs() <= 1, "{a:?} {p:?}");
            }
        }
    }

    #[test]
    fn invert_and_blur() {
        let g = grid_with([200, 100, 0, 255]);
        let t = Adjustment::Invert.apply(&g, TileCoord::new(0, 0)).unwrap();
        assert_eq!(t.px[0], [55, 155, 255, 255]);
        let b = Adjustment::Blur { radius: 6.0 };
        // Blur spreads into the empty neighbour tile.
        let n = b.apply(&g, TileCoord::new(1, 0)).unwrap();
        assert!(n.px[0][3] > 0 && n.px[0][3] < 255);
        let inner = b.apply(&g, TileCoord::new(0, 0)).unwrap();
        assert_eq!(inner.px[20 * 64 + 20], [200, 100, 0, 255]);
    }

    #[test]
    fn pixelate_blocks() {
        let mut g = TileGrid::new(64, 64);
        let t = g.get_mut_or_insert(TileCoord::new(0, 0));
        t.px[0] = [255, 255, 255, 255];
        let p = Adjustment::Pixelate { size: 2 }
            .apply(&g, TileCoord::new(0, 0))
            .unwrap();
        assert_eq!(p.px[1][3], 64);
        assert_eq!(p.px[64][3], 64);
        assert_eq!(p.px[2][3], 0);
    }
}
