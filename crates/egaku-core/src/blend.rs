//! Layer blending.
//!
//! All pixels are premultiplied and sRGB-encoded, and blending happens in
//! that space. This is what artists have trained their eyes on for thirty
//! years (Photoshop, SAI, Procreate and Krita's defaults all blend this way),
//! and it lets the overwhelmingly common Normal mode run as pure integer math.
//! The formulas follow the W3C Compositing and Blending spec.

use crate::tile::{TILE, TILE_PX, Tile, TileCoord};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    /// Each pixel is either fully drawn or skipped, at random, with a
    /// chance set by its opacity.
    Dissolve,
    Darken,
    Multiply,
    ColorBurn,
    LinearBurn,
    DarkerColor,
    Lighten,
    Screen,
    ColorDodge,
    /// Linear Dodge.
    Add,
    LighterColor,
    Overlay,
    SoftLight,
    HardLight,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    Difference,
    Exclusion,
    Subtract,
    Divide,
    Hue,
    Saturation,
    Color,
    Luminosity,
    /// Groups only: the children blend straight into what's below, as if
    /// the group weren't there. Treated as Normal for anything else.
    PassThrough,
}

impl BlendMode {
    /// Every mode, in menu order (Photoshop's groups: normal, darken,
    /// lighten, contrast, comparison, colour). Pass Through comes last
    /// and is only offered for groups.
    pub const ALL: [BlendMode; 28] = [
        BlendMode::Normal,
        BlendMode::Dissolve,
        BlendMode::Darken,
        BlendMode::Multiply,
        BlendMode::ColorBurn,
        BlendMode::LinearBurn,
        BlendMode::DarkerColor,
        BlendMode::Lighten,
        BlendMode::Screen,
        BlendMode::ColorDodge,
        BlendMode::Add,
        BlendMode::LighterColor,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::VividLight,
        BlendMode::LinearLight,
        BlendMode::PinLight,
        BlendMode::HardMix,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Subtract,
        BlendMode::Divide,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
        BlendMode::PassThrough,
    ];

    pub fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Dissolve => "Dissolve",
            BlendMode::Darken => "Darken",
            BlendMode::Multiply => "Multiply",
            BlendMode::ColorBurn => "Color Burn",
            BlendMode::LinearBurn => "Linear Burn",
            BlendMode::DarkerColor => "Darker Color",
            BlendMode::Lighten => "Lighten",
            BlendMode::Screen => "Screen",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::Add => "Add (Linear Dodge)",
            BlendMode::LighterColor => "Lighter Color",
            BlendMode::Overlay => "Overlay",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::HardLight => "Hard Light",
            BlendMode::VividLight => "Vivid Light",
            BlendMode::LinearLight => "Linear Light",
            BlendMode::PinLight => "Pin Light",
            BlendMode::HardMix => "Hard Mix",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Subtract => "Subtract",
            BlendMode::Divide => "Divide",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
            BlendMode::PassThrough => "Pass Through",
        }
    }

    /// Whether a menu should draw a separator before this mode.
    pub fn starts_group(self) -> bool {
        matches!(
            self,
            BlendMode::Darken
                | BlendMode::Lighten
                | BlendMode::Overlay
                | BlendMode::Difference
                | BlendMode::Hue
                | BlendMode::PassThrough
        )
    }

    /// The OpenRaster `composite-op` name. Modes the spec lacks use the
    /// names Krita writes, so files round-trip through it.
    pub fn ora_name(self) -> &'static str {
        match self {
            BlendMode::Normal | BlendMode::PassThrough => "svg:src-over",
            BlendMode::Dissolve => "krita:dissolve",
            BlendMode::Darken => "svg:darken",
            BlendMode::Multiply => "svg:multiply",
            BlendMode::ColorBurn => "svg:color-burn",
            BlendMode::LinearBurn => "krita:linear_burn",
            BlendMode::DarkerColor => "krita:darker color",
            BlendMode::Lighten => "svg:lighten",
            BlendMode::Screen => "svg:screen",
            BlendMode::ColorDodge => "svg:color-dodge",
            BlendMode::Add => "svg:plus",
            BlendMode::LighterColor => "krita:lighter color",
            BlendMode::Overlay => "svg:overlay",
            BlendMode::SoftLight => "svg:soft-light",
            BlendMode::HardLight => "svg:hard-light",
            BlendMode::VividLight => "krita:vivid_light",
            BlendMode::LinearLight => "krita:linear light",
            BlendMode::PinLight => "krita:pin_light",
            BlendMode::HardMix => "krita:hard_mix_photoshop",
            BlendMode::Difference => "svg:difference",
            BlendMode::Exclusion => "svg:exclusion",
            BlendMode::Subtract => "krita:subtract",
            BlendMode::Divide => "krita:divide",
            BlendMode::Hue => "svg:hue",
            BlendMode::Saturation => "svg:saturation",
            BlendMode::Color => "svg:color",
            BlendMode::Luminosity => "svg:luminosity",
        }
    }

    pub fn from_ora_name(s: &str) -> BlendMode {
        match s {
            "krita:linear_dodge" | "krita:add" => return BlendMode::Add,
            "krita:hard mix" | "krita:hard_mix" => return BlendMode::HardMix,
            "krita:pass through" => return BlendMode::PassThrough,
            _ => {}
        }
        BlendMode::ALL
            .into_iter()
            .filter(|m| *m != BlendMode::PassThrough)
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
/// Dissolve's pattern depends on where the tile is; use [`blend_tile_at`]
/// when it matters.
pub fn blend_tile(dst: &mut Tile, src: &Tile, opacity: u8, mode: BlendMode) {
    blend_tile_at(dst, src, opacity, mode, TileCoord::new(0, 0));
}

/// [`blend_tile`] for the tile at `c`.
pub fn blend_tile_at(dst: &mut Tile, src: &Tile, opacity: u8, mode: BlendMode, c: TileCoord) {
    if opacity == 0 {
        return;
    }
    match mode {
        BlendMode::Normal | BlendMode::PassThrough => normal(dst, src, opacity),
        BlendMode::Dissolve => dissolve(dst, src, opacity, c),
        m => general(dst, src, opacity, m),
    }
}

/// A stable random byte per canvas pixel.
#[inline]
fn noise(x: u32, y: u32) -> u32 {
    let mut h = x.wrapping_mul(0x9e37_79b1) ^ y.wrapping_mul(0x85eb_ca77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    (h ^ (h >> 15)) & 0xff
}

fn dissolve(dst: &mut Tile, src: &Tile, opacity: u8, c: TileCoord) {
    let (bx, by) = (c.x * TILE as u32, c.y * TILE as u32);
    let o = opacity as u32;
    for (i, (d, s)) in dst.px.iter_mut().zip(src.px.iter()).enumerate() {
        let a = div255(s[3] as u32 * o);
        if a == 0 {
            continue;
        }
        let (x, y) = (bx + (i % TILE) as u32, by + (i / TILE) as u32);
        if noise(x, y) < a {
            *d = crate::color::unpremultiply(*s);
            d[3] = 255;
            *d = crate::color::premultiply(*d);
        }
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
fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

/// The W3C SetSat: stretches `c` to saturation `s`, keeping its hue.
fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    if mx - mn <= 1e-6 {
        return [0.0; 3];
    }
    c.map(|v| (v - mn) * s / (mx - mn))
}

#[inline]
fn color_burn(b: f32, s: f32) -> f32 {
    if b >= 1.0 {
        1.0
    } else if s <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - b) / s).min(1.0)
    }
}

#[inline]
fn color_dodge(b: f32, s: f32) -> f32 {
    if b <= 0.0 {
        0.0
    } else if s >= 1.0 {
        1.0
    } else {
        (b / (1.0 - s)).min(1.0)
    }
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
        BlendMode::HardLight => {
            if s <= 0.5 {
                2.0 * b * s
            } else {
                let s2 = 2.0 * s - 1.0;
                b + s2 - b * s2
            }
        }
        BlendMode::Darken => b.min(s),
        BlendMode::Lighten => b.max(s),
        BlendMode::ColorBurn => color_burn(b, s),
        BlendMode::ColorDodge => color_dodge(b, s),
        BlendMode::LinearBurn => (b + s - 1.0).max(0.0),
        BlendMode::Add => (b + s).min(1.0),
        BlendMode::VividLight => {
            if s <= 0.5 {
                color_burn(b, 2.0 * s)
            } else {
                color_dodge(b, 2.0 * s - 1.0)
            }
        }
        BlendMode::LinearLight => (b + 2.0 * s - 1.0).clamp(0.0, 1.0),
        BlendMode::PinLight => {
            if s <= 0.5 {
                b.min(2.0 * s)
            } else {
                b.max(2.0 * s - 1.0)
            }
        }
        BlendMode::HardMix => {
            if b + s >= 1.0 - 1e-6 {
                1.0
            } else {
                0.0
            }
        }
        BlendMode::Difference => (b - s).abs(),
        BlendMode::Exclusion => b + s - 2.0 * b * s,
        BlendMode::Subtract => (b - s).max(0.0),
        BlendMode::Divide => {
            if s <= 0.0 {
                if b > 0.0 { 1.0 } else { 0.0 }
            } else {
                (b / s).min(1.0)
            }
        }
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
            BlendMode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
            BlendMode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
            BlendMode::Color => set_lum(cs, lum(cb)),
            BlendMode::Luminosity => set_lum(cb, lum(cs)),
            BlendMode::DarkerColor => {
                if lum(cs) < lum(cb) {
                    cs
                } else {
                    cb
                }
            }
            BlendMode::LighterColor => {
                if lum(cs) > lum(cb) {
                    cs
                } else {
                    cb
                }
            }
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

    /// One opaque pixel of `b` blended onto opaque `a`, per channel 0..=255.
    fn one(m: BlendMode, a: u8, b: u8) -> u8 {
        let mut d = Tile::filled([a, a, a, 255]);
        blend_tile(&mut d, &Tile::filled([b, b, b, 255]), 255, m);
        d.px[0][0]
    }

    #[test]
    fn separable_modes() {
        use BlendMode::*;
        assert_eq!(one(HardLight, 100, 255), 255);
        assert_eq!(one(HardLight, 100, 0), 0);
        assert_eq!(one(ColorBurn, 255, 10), 255);
        assert_eq!(one(ColorBurn, 100, 0), 0);
        assert_eq!(one(LinearBurn, 100, 100), 0);
        assert_eq!(one(LinearBurn, 200, 200), 145);
        assert_eq!(one(Exclusion, 255, 255), 0);
        assert_eq!(one(Exclusion, 0, 77), 77);
        assert_eq!(one(Subtract, 200, 50), 150);
        assert_eq!(one(Subtract, 50, 200), 0);
        assert_eq!(one(Divide, 100, 200), 128);
        assert_eq!(one(Divide, 100, 0), 255);
        assert_eq!(one(LinearLight, 100, 128), 101);
        assert_eq!(one(PinLight, 200, 50), 100);
        assert_eq!(one(PinLight, 50, 200), 145);
        assert_eq!(one(HardMix, 100, 160), 255);
        assert_eq!(one(HardMix, 100, 150), 0);
        assert_eq!(one(VividLight, 128, 255), 255);
        assert_eq!(one(VividLight, 128, 0), 0);
    }

    #[test]
    fn non_separable_modes() {
        let red = [200, 0, 0, 255];
        let grey = [100, 100, 100, 255];
        let blend = |m, d: [u8; 4], s: [u8; 4]| {
            let mut t = Tile::filled(d);
            blend_tile(&mut t, &Tile::filled(s), 255, m);
            t.px[0]
        };
        // A grey has no hue or saturation to give.
        let h = blend(BlendMode::Saturation, red, grey);
        assert!(h[0] == h[1] && h[1] == h[2], "{h:?}");
        // Taking red's hue onto a grey does nothing (grey has no saturation).
        assert_eq!(blend(BlendMode::Hue, grey, red), grey);
        assert_eq!(blend(BlendMode::DarkerColor, grey, red), red);
        assert_eq!(blend(BlendMode::LighterColor, grey, red), grey);
    }

    #[test]
    fn dissolve_is_all_or_nothing() {
        let mut d = Tile::filled([0, 0, 0, 255]);
        blend_tile_at(
            &mut d,
            &Tile::filled([128, 128, 128, 128]),
            255,
            BlendMode::Dissolve,
            TileCoord::new(3, 4),
        );
        let lit = d.px.iter().filter(|p| p[0] == 255).count();
        assert!(d.px.iter().all(|p| p[0] == 0 || p[0] == 255));
        assert!((lit as i32 - TILE_PX as i32 / 2).abs() < 300, "{lit}");
    }

    #[test]
    fn ora_names_round_trip() {
        for m in BlendMode::ALL {
            let back = BlendMode::from_ora_name(m.ora_name());
            if m == BlendMode::PassThrough {
                assert_eq!(back, BlendMode::Normal);
            } else {
                assert_eq!(back, m);
            }
        }
    }

    #[test]
    fn blend_over_transparent_is_source() {
        for m in BlendMode::ALL {
            if m == BlendMode::Dissolve {
                continue;
            }
            let mut a = Tile::filled([0; 4]);
            let b = Tile::filled([60, 30, 10, 128]);
            blend_tile(&mut a, &b, 255, m);
            assert_eq!(a.px[0], [60, 30, 10, 128], "{m:?}");
        }
    }
}
