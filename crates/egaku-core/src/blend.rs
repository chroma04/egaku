//! Layer blending.
//!
//! All pixels are premultiplied and sRGB-encoded, and blending happens in
//! that space. This is what artists have trained their eyes on for thirty
//! years (Photoshop, SAI, Procreate and Krita's defaults all blend this way),
//! and it lets the overwhelmingly common Normal mode run as pure integer math.
//! The formulas follow the W3C Compositing and Blending spec.

use crate::tile::{TILE_PX, Tile};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
    Darken,
    Lighten,
    ColorDodge,
    Add,
    Difference,
    Color,
    Luminosity,
}

impl BlendMode {
    pub const ALL: [BlendMode; 12] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::Add,
        BlendMode::Difference,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];

    pub fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::Add => "Add",
            BlendMode::Difference => "Difference",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
        }
    }

    /// The OpenRaster `composite-op` name.
    pub fn ora_name(self) -> &'static str {
        match self {
            BlendMode::Normal => "svg:src-over",
            BlendMode::Multiply => "svg:multiply",
            BlendMode::Screen => "svg:screen",
            BlendMode::Overlay => "svg:overlay",
            BlendMode::SoftLight => "svg:soft-light",
            BlendMode::Darken => "svg:darken",
            BlendMode::Lighten => "svg:lighten",
            BlendMode::ColorDodge => "svg:color-dodge",
            BlendMode::Add => "svg:plus",
            BlendMode::Difference => "svg:difference",
            BlendMode::Color => "svg:color",
            BlendMode::Luminosity => "svg:luminosity",
        }
    }

    pub fn from_ora_name(s: &str) -> BlendMode {
        BlendMode::ALL
            .into_iter()
            .find(|m| m.ora_name() == s)
            .unwrap_or_default()
    }
}

/// Exact rounding division by 255 for `x <= 65535 + 255`.
#[inline(always)]
pub fn div255(x: u32) -> u32 {
    let x = x + 128;
    (x + (x >> 8)) >> 8
}

/// Composites `src` over `dst` with the given layer opacity and blend mode.
pub fn blend_tile(dst: &mut Tile, src: &Tile, opacity: u8, mode: BlendMode) {
    if opacity == 0 {
        return;
    }
    match mode {
        BlendMode::Normal => normal(dst, src, opacity),
        m => general(dst, src, opacity, m),
    }
}

fn normal(dst: &mut Tile, src: &Tile, opacity: u8) {
    let pairs = dst.px.iter_mut().zip(src.px.iter());
    if opacity == 255 {
        for (d, s) in pairs {
            let inv = 255 - s[3] as u32;
            if inv == 0 {
                *d = *s;
            } else if inv != 255 {
                for c in 0..4 {
                    d[c] = (s[c] as u32 + div255(d[c] as u32 * inv)) as u8;
                }
            }
        }
    } else {
        let o = opacity as u32;
        for (d, s) in pairs {
            if s[3] == 0 {
                continue;
            }
            let sa = div255(s[3] as u32 * o);
            let inv = 255 - sa;
            for c in 0..4 {
                d[c] = (div255(s[c] as u32 * o) + div255(d[c] as u32 * inv)) as u8;
            }
        }
    }
}

#[inline]
fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut c = c;
    if n < 0.0 {
        for v in &mut c {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1.0 {
        for v in &mut c {
            *v = l + (*v - l) * (1.0 - l) / (x - l);
        }
    }
    c
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

#[inline]
fn separable(mode: BlendMode, b: f32, s: f32) -> f32 {
    match mode {
        BlendMode::Multiply => b * s,
        BlendMode::Screen => b + s - b * s,
        BlendMode::Overlay => {
            if b <= 0.5 {
                2.0 * b * s
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - s)
            }
        }
        BlendMode::SoftLight => {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 {
                    ((16.0 * b - 12.0) * b + 4.0) * b
                } else {
                    b.sqrt()
                };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }
        BlendMode::Darken => b.min(s),
        BlendMode::Lighten => b.max(s),
        BlendMode::ColorDodge => {
            if b == 0.0 {
                0.0
            } else if s >= 1.0 {
                1.0
            } else {
                (b / (1.0 - s)).min(1.0)
            }
        }
        BlendMode::Add => (b + s).min(1.0),
        BlendMode::Difference => (b - s).abs(),
        _ => s,
    }
}

fn general(dst: &mut Tile, src: &Tile, opacity: u8, mode: BlendMode) {
    const K: f32 = 1.0 / 255.0;
    let o = opacity as f32 * K;
    for i in 0..TILE_PX {
        let s = src.px[i];
        if s[3] == 0 {
            continue;
        }
        let d = dst.px[i];
        let sa = s[3] as f32 * K * o;
        let da = d[3] as f32 * K;
        let sp = [
            s[0] as f32 * K * o,
            s[1] as f32 * K * o,
            s[2] as f32 * K * o,
        ];
        let dp = [d[0] as f32 * K, d[1] as f32 * K, d[2] as f32 * K];
        // Unpremultiplied colours for the blend function.
        let cs = [sp[0] / sa, sp[1] / sa, sp[2] / sa];
        let cb = if da > 0.0 {
            [dp[0] / da, dp[1] / da, dp[2] / da]
        } else {
            [0.0; 3]
        };
        let b = match mode {
            BlendMode::Color => set_lum(cs, lum(cb)),
            BlendMode::Luminosity => set_lum(cb, lum(cs)),
            m => [
                separable(m, cb[0], cs[0]),
                separable(m, cb[1], cs[1]),
                separable(m, cb[2], cs[2]),
            ],
        };
        let ra = sa + da - sa * da;
        let mut out = [0u8; 4];
        for c in 0..3 {
            let v = (1.0 - da) * sp[c] + (1.0 - sa) * dp[c] + sa * da * b[c];
            out[c] = (v.clamp(0.0, ra) * 255.0 + 0.5) as u8;
        }
        out[3] = (ra * 255.0 + 0.5) as u8;
        dst.px[i] = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn div255_exact() {
        for x in 0..=65535u32 {
            assert_eq!(div255(x), (x as f64 / 255.0).round() as u32, "{x}");
        }
    }

    #[test]
    fn normal_matches_general_formula() {
        let mut a = Tile::filled([100, 50, 20, 200]);
        let b = Tile::filled([30, 60, 90, 120]);
        let mut a2 = a.clone();
        blend_tile(&mut a, &b, 180, BlendMode::Normal);
        // The general path with the identity blend function is src-over.
        general(&mut a2, &b, 180, BlendMode::Normal);
        for c in 0..4 {
            assert!((a.px[0][c] as i32 - a2.px[0][c] as i32).abs() <= 1);
        }
    }

    #[test]
    fn multiply_white_is_identity() {
        let mut a = Tile::filled([120, 80, 40, 255]);
        let b = Tile::filled([255, 255, 255, 255]);
        blend_tile(&mut a, &b, 255, BlendMode::Multiply);
        assert_eq!(a.px[7], [120, 80, 40, 255]);
    }

    #[test]
    fn blend_over_transparent_is_source() {
        for m in BlendMode::ALL {
            let mut a = Tile::filled([0; 4]);
            let b = Tile::filled([60, 30, 10, 128]);
            blend_tile(&mut a, &b, 255, m);
            assert_eq!(a.px[0], [60, 30, 10, 128], "{m:?}");
        }
    }
}
