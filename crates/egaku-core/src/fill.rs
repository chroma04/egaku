//! Flood fill.
//!
//! The fill produces a coverage [`Mask`] that is committed exactly like a
//! brush stroke, so it honours alpha lock, opacity and the eraser for free.
//! Following Clip Studio and SAI, the region can be grown a few pixels so
//! fills tuck under anti-aliased line art instead of leaving a halo.

use crate::brush::Mask;
use crate::tile::{Px, TILE, TileRect};

pub struct FillParams {
    /// Maximum per-channel difference (0..=255) still considered "same".
    pub tolerance: u8,
    /// Pixels to grow the region by after filling.
    pub grow: u32,
    /// Coverage written into the mask, 0..=1.
    pub opacity: f32,
}

struct Bits {
    w: usize,
    v: Vec<u64>,
}

impl Bits {
    fn new(w: usize, h: usize) -> Self {
        Bits {
            w,
            v: vec![0; (w * h).div_ceil(64)],
        }
    }
    #[inline]
    fn get(&self, x: usize, y: usize) -> bool {
        let i = y * self.w + x;
        self.v[i >> 6] >> (i & 63) & 1 != 0
    }
    #[inline]
    fn set(&mut self, x: usize, y: usize) {
        let i = y * self.w + x;
        self.v[i >> 6] |= 1 << (i & 63);
    }
}

/// Fills the region connected to `(sx, sy)` whose pixels (read through
/// `sample`) match the seed colour. Returns `None` if the seed is outside the
/// canvas.
pub fn flood_fill(
    width: u32,
    height: u32,
    sx: u32,
    sy: u32,
    sample: impl Fn(u32, u32) -> Px + Sync,
    p: &FillParams,
) -> Option<Mask> {
    if sx >= width || sy >= height {
        return None;
    }
    let (w, h) = (width as usize, height as usize);
    let seed = sample(sx, sy);
    let tol = p.tolerance as i32;
    let matches = |x: usize, y: usize| {
        let q = sample(x as u32, y as u32);
        (0..4).all(|c| (q[c] as i32 - seed[c] as i32).abs() <= tol)
    };

    let mut filled = Bits::new(w, h);
    let (mut bx0, mut by0, mut bx1, mut by1) = (sx as usize, sy as usize, sx as usize, sy as usize);
    let mut stack = vec![(sx as usize, sy as usize)];
    while let Some((x, y)) = stack.pop() {
        if filled.get(x, y) || !matches(x, y) {
            continue;
        }
        let mut l = x;
        while l > 0 && !filled.get(l - 1, y) && matches(l - 1, y) {
            l -= 1;
        }
        let mut r = x;
        while r + 1 < w && !filled.get(r + 1, y) && matches(r + 1, y) {
            r += 1;
        }
        for i in l..=r {
            filled.set(i, y);
        }
        bx0 = bx0.min(l);
        bx1 = bx1.max(r);
        by0 = by0.min(y);
        by1 = by1.max(y);
        for ny in [y.wrapping_sub(1), y + 1] {
            if ny >= h {
                continue;
            }
            let mut i = l;
            while i <= r {
                if !filled.get(i, ny) && matches(i, ny) {
                    stack.push((i, ny));
                    // Skip the rest of this run; it will be found by the span walk.
                    while i <= r && matches(i, ny) {
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
        }
    }

    // Grow with a separable square max filter over the bounding box.
    let g = p.grow as usize;
    if g > 0 {
        bx0 = bx0.saturating_sub(g);
        by0 = by0.saturating_sub(g);
        bx1 = (bx1 + g).min(w - 1);
        by1 = (by1 + g).min(h - 1);
        let mut tmp = Bits::new(w, h);
        for y in by0..=by1 {
            let mut last: Option<usize> = None; // last filled x seen
            let mut next = bx0;
            for x in bx0..=bx1 {
                // Look ahead for the nearest filled pixel to the right.
                if next < x {
                    next = x;
                }
                while next <= (x + g).min(bx1) && !filled.get(next, y) {
                    next += 1;
                }
                if filled.get(x, y) {
                    last = Some(x);
                }
                let near_left = last.is_some_and(|lx| x - lx <= g);
                let near_right = next <= (x + g).min(bx1);
                if near_left || near_right {
                    tmp.set(x, y);
                }
            }
        }
        let mut out = Bits::new(w, h);
        for x in bx0..=bx1 {
            let mut last: Option<usize> = None;
            let mut next = by0;
            for y in by0..=by1 {
                if next < y {
                    next = y;
                }
                while next <= (y + g).min(by1) && !tmp.get(x, next) {
                    next += 1;
                }
                if tmp.get(x, y) {
                    last = Some(y);
                }
                if last.is_some_and(|ly| y - ly <= g) || next <= (y + g).min(by1) {
                    out.set(x, y);
                }
            }
        }
        filled = out;
    }

    let mut mask = Mask::new(width, height);
    let value = (p.opacity.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
    let rect = TileRect::from_px(
        bx0 as i64,
        by0 as i64,
        bx1 as i64 + 1,
        by1 as i64 + 1,
        width.div_ceil(TILE as u32),
        height.div_ceil(TILE as u32),
    );
    mask.set_from(rect, |c, t| {
        let mut any = false;
        let (ox, oy) = (c.x as usize * TILE, c.y as usize * TILE);
        for ly in 0..TILE.min(h - oy) {
            for lx in 0..TILE.min(w - ox) {
                if filled.get(ox + lx, oy + ly) {
                    t.v[ly * TILE + lx] = value;
                    any = true;
                }
            }
        }
        any
    });
    Some(mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_enclosed_region_and_grows() {
        // A 100x100 white canvas with a black ring between radius 20 and 23.
        let sample = |x: u32, y: u32| {
            let (dx, dy) = (x as f32 - 50.0, y as f32 - 50.0);
            let d = (dx * dx + dy * dy).sqrt();
            if (20.0..23.0).contains(&d) {
                [0, 0, 0, 255]
            } else {
                [255; 4]
            }
        };
        let p = FillParams {
            tolerance: 10,
            grow: 0,
            opacity: 1.0,
        };
        let m = flood_fill(100, 100, 50, 50, sample, &p).unwrap();
        assert_eq!(m.value(50, 50), 65535);
        assert_eq!(m.value(50, 50 + 21), 0);
        assert_eq!(m.value(2, 2), 0);
        let p = FillParams { grow: 2, ..p };
        let m = flood_fill(100, 100, 50, 50, sample, &p).unwrap();
        assert_eq!(m.value(50, 50 + 21), 65535);
        assert_eq!(m.value(50, 50 + 23), 0);
    }

    #[test]
    fn outside_is_none() {
        let p = FillParams {
            tolerance: 0,
            grow: 0,
            opacity: 1.0,
        };
        assert!(flood_fill(10, 10, 10, 0, |_, _| [0; 4], &p).is_none());
    }
}
