//! Colour helpers.

use crate::tile::Px;

/// A straight (non-premultiplied) sRGB colour with channels in `0..=1`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb {
        r: 0.0,
        g: 0.0,
        b: 0.0,
    };
    pub const WHITE: Rgb = Rgb {
        r: 1.0,
        g: 1.0,
        b: 1.0,
    };

    pub fn new(r: f32, g: f32, b: f32) -> Self {
        Rgb { r, g, b }
    }

    pub fn from_u8(c: [u8; 3]) -> Self {
        Rgb::new(
            c[0] as f32 / 255.0,
            c[1] as f32 / 255.0,
            c[2] as f32 / 255.0,
        )
    }

    pub fn to_u8(self) -> [u8; 3] {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        [q(self.r), q(self.g), q(self.b)]
    }

    pub fn to_hex(self) -> String {
        let [r, g, b] = self.to_u8();
        format!("#{r:02X}{g:02X}{b:02X}")
    }

    pub fn from_hex(s: &str) -> Option<Rgb> {
        let s = s.trim().trim_start_matches('#');
        let v = match s.len() {
            6 => u32::from_str_radix(s, 16).ok()?,
            3 => {
                let v = u32::from_str_radix(s, 16).ok()?;
                let (r, g, b) = ((v >> 8) & 15, (v >> 4) & 15, v & 15);
                (r * 17) << 16 | (g * 17) << 8 | (b * 17)
            }
            _ => return None,
        };
        Some(Rgb::from_u8([(v >> 16) as u8, (v >> 8) as u8, v as u8]))
    }

    /// Converts to hue (0..1), saturation and value.
    pub fn to_hsv(self) -> Hsv {
        let max = self.r.max(self.g).max(self.b);
        let min = self.r.min(self.g).min(self.b);
        let d = max - min;
        let h = if d <= 0.0 {
            0.0
        } else if max == self.r {
            ((self.g - self.b) / d).rem_euclid(6.0) / 6.0
        } else if max == self.g {
            ((self.b - self.r) / d + 2.0) / 6.0
        } else {
            ((self.r - self.g) / d + 4.0) / 6.0
        };
        let s = if max <= 0.0 { 0.0 } else { d / max };
        Hsv { h, s, v: max }
    }
}

/// Hue, saturation, value; all in `0..=1`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Hsv {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

impl Hsv {
    pub fn to_rgb(self) -> Rgb {
        let h = self.h.rem_euclid(1.0) * 6.0;
        let c = self.v * self.s;
        let x = c * (1.0 - (h % 2.0 - 1.0).abs());
        let m = self.v - c;
        let (r, g, b) = match h as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        Rgb::new(r + m, g + m, b + m)
    }
}

/// Converts a premultiplied pixel to straight alpha.
pub fn unpremultiply(p: Px) -> Px {
    match p[3] {
        0 => [0; 4],
        255 => p,
        a => {
            let a = a as u32;
            let f = |c: u8| ((c as u32 * 255 + a / 2) / a).min(255) as u8;
            [f(p[0]), f(p[1]), f(p[2]), p[3]]
        }
    }
}

/// Converts a straight-alpha pixel to premultiplied.
pub fn premultiply(p: Px) -> Px {
    let a = p[3] as u32;
    let f = |c: u8| crate::blend::div255(c as u32 * a) as u8;
    [f(p[0]), f(p[1]), f(p[2]), p[3]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_roundtrip() {
        for c in [
            [255, 0, 0],
            [12, 200, 99],
            [0, 0, 0],
            [255, 255, 255],
            [40, 40, 220],
        ] {
            let rgb = Rgb::from_u8(c);
            assert_eq!(rgb.to_hsv().to_rgb().to_u8(), c);
        }
    }

    #[test]
    fn hex() {
        assert_eq!(Rgb::from_hex("#1a2B3c").unwrap().to_hex(), "#1A2B3C");
        assert_eq!(Rgb::from_hex("fff").unwrap().to_u8(), [255; 3]);
        assert!(Rgb::from_hex("zz").is_none());
    }

    #[test]
    fn premul_roundtrip() {
        let p = [200, 100, 50, 128];
        let q = unpremultiply(premultiply(p));
        for c in 0..4 {
            assert!((p[c] as i32 - q[c] as i32).abs() <= 1);
        }
    }
}
