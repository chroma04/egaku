//! Brush tips made from images.
//!
//! An image tip is a grey coverage map: where it is opaque (or dark, for
//! images without transparency) the brush lays down paint. Tips are kept in a
//! small process-wide registry keyed by a hash of their pixels, so brush
//! settings can name one with a plain number, stay `Copy`, and compare equal
//! exactly when they'd paint the same thing.
//!
//! Each tip carries a box-filtered mip pyramid, so a 256 px tip used as a
//! 12 px brush averages properly instead of sparkling.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

/// The longest side an imported tip is scaled down to.
pub const MAX_TIP: u32 = 256;

struct Level {
    w: u32,
    h: u32,
    v: Vec<u8>,
}

/// A brush tip image: coverage 0..=255 per pixel.
pub struct TipImage {
    levels: Vec<Level>,
}

impl std::fmt::Debug for TipImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TipImage({}×{})", self.width(), self.height())
    }
}

impl TipImage {
    /// A tip from raw coverage rows.
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        assert_eq!(data.len(), (width * height) as usize);
        let mut levels = vec![Level {
            w: width.max(1),
            h: height.max(1),
            v: data,
        }];
        while let Some(l) = levels.last()
            && (l.w > 1 || l.h > 1)
        {
            let (w, h) = (l.w.div_ceil(2), l.h.div_ceil(2));
            let mut v = vec![0u8; (w * h) as usize];
            for y in 0..h {
                for x in 0..w {
                    let mut sum = 0u32;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (sx, sy) = (2 * x + dx, 2 * y + dy);
                        if sx < l.w && sy < l.h {
                            sum += l.v[(sy * l.w + sx) as usize] as u32;
                        }
                    }
                    v[(y * w + x) as usize] = ((sum + 2) / 4) as u8;
                }
            }
            levels.push(Level { w, h, v });
        }
        TipImage { levels }
    }

    /// A tip from straight-alpha RGBA. Images with transparency use their
    /// alpha; opaque ones use darkness, so black ink on white paper works
    /// as well as a cut-out shape. The result is cropped to what it covers
    /// (so the tip sits centred under the cursor) and scaled to fit
    /// [`MAX_TIP`].
    pub fn from_rgba(width: u32, height: u32, rgba: &[u8]) -> Option<Self> {
        let has_alpha = rgba.as_chunks::<4>().0.iter().any(|p| p[3] < 250);
        let cov: Vec<u8> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                if has_alpha {
                    p[3]
                } else {
                    let l = (p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8;
                    255 - l as u8
                }
            })
            .collect();
        // Crop to the covered area.
        let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0, 0);
        for y in 0..height {
            for x in 0..width {
                if cov[(y * width + x) as usize] > 2 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        let (w, h) = (x1 - x0, y1 - y0);
        let mut img = image::GrayImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.put_pixel(
                    x,
                    y,
                    image::Luma([cov[((y0 + y) * width + x0 + x) as usize]]),
                );
            }
        }
        let long = w.max(h);
        if long > MAX_TIP {
            let k = MAX_TIP as f32 / long as f32;
            let (nw, nh) = (
                ((w as f32 * k).round() as u32).max(1),
                ((h as f32 * k).round() as u32).max(1),
            );
            img = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle);
        }
        let (w, h) = img.dimensions();
        Some(TipImage::new(w, h, img.into_raw()))
    }

    pub fn width(&self) -> u32 {
        self.levels[0].w
    }

    pub fn height(&self) -> u32 {
        self.levels[0].h
    }

    /// Full-resolution coverage rows.
    pub fn data(&self) -> &[u8] {
        &self.levels[0].v
    }

    /// A stable id from the tip's pixels; never 0.
    pub fn id(&self) -> u64 {
        // FNV-1a.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |b: u8| {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        };
        for b in self.width().to_le_bytes() {
            eat(b);
        }
        for b in self.height().to_le_bytes() {
            eat(b);
        }
        for &b in self.data() {
            eat(b);
        }
        h.max(1)
    }

    /// The tip as a greyscale PNG, for saving.
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = vec![];
        let mut enc = png::Encoder::new(&mut out, self.width(), self.height());
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        if let Ok(mut w) = enc.write_header() {
            let _ = w.write_image_data(self.data());
        }
        out
    }

    /// Reads a tip written by [`TipImage::to_png`].
    pub fn from_png(bytes: &[u8]) -> Option<Self> {
        let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .ok()?
            .into_luma8();
        let (w, h) = img.dimensions();
        (w > 0 && h > 0 && w <= MAX_TIP * 4 && h <= MAX_TIP * 4)
            .then(|| TipImage::new(w, h, img.into_raw()))
    }

    /// Coverage 0..=1 at `(x, y)` in full-resolution image pixels, where
    /// one canvas pixel spans `step` image pixels. Smooth sampling is
    /// bilinear from the matching mip level; otherwise it's the nearest
    /// pixel, for aliased brushes.
    #[inline]
    pub fn sample(&self, x: f32, y: f32, step: f32, smooth: bool) -> f32 {
        let lv = (step.max(1.0).log2().floor() as usize).min(self.levels.len() - 1);
        let l = &self.levels[lv];
        let k = 1.0 / (1u32 << lv) as f32;
        let (x, y) = (x * k, y * k);
        let at = |xi: i64, yi: i64| -> f32 {
            if xi < 0 || yi < 0 || xi >= l.w as i64 || yi >= l.h as i64 {
                0.0
            } else {
                l.v[(yi as u32 * l.w + xi as u32) as usize] as f32
            }
        };
        let v = if smooth {
            let (fx, fy) = (x - 0.5, y - 0.5);
            let (x0, y0) = (fx.floor(), fy.floor());
            let (tx, ty) = (fx - x0, fy - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let a = at(x0, y0) + (at(x0 + 1, y0) - at(x0, y0)) * tx;
            let b = at(x0, y0 + 1) + (at(x0 + 1, y0 + 1) - at(x0, y0 + 1)) * tx;
            a + (b - a) * ty
        } else {
            at(x.floor() as i64, y.floor() as i64)
        };
        v * (1.0 / 255.0)
    }
}

fn registry() -> &'static RwLock<HashMap<u64, Arc<TipImage>>> {
    static R: OnceLock<RwLock<HashMap<u64, Arc<TipImage>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Makes a tip available to brushes and returns its id.
pub fn register(img: TipImage) -> u64 {
    let id = img.id();
    registry()
        .write()
        .unwrap()
        .entry(id)
        .or_insert_with(|| Arc::new(img));
    id
}

/// A registered tip.
pub fn get(id: u64) -> Option<Arc<TipImage>> {
    registry().read().unwrap().get(&id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_import_crops_and_round_trips() {
        // A 2×2 black block on a 6×4 white, opaque image.
        let (w, h) = (6u32, 4u32);
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        for (x, y) in [(2, 1), (3, 1), (2, 2), (3, 2)] {
            let o = ((y * w + x) * 4) as usize;
            rgba[o..o + 3].fill(0);
        }
        let t = TipImage::from_rgba(w, h, &rgba).unwrap();
        assert_eq!((t.width(), t.height()), (2, 2));
        assert!(t.data().iter().all(|v| *v == 255));
        let back = TipImage::from_png(&t.to_png()).unwrap();
        assert_eq!(back.id(), t.id());
        let id = register(back);
        assert!(get(id).is_some());
        // Blank images make no tip.
        assert!(TipImage::from_rgba(2, 2, &[255; 16]).is_none());
    }

    #[test]
    fn mips_average() {
        let t = TipImage::new(4, 4, vec![255; 16]);
        assert!((t.sample(2.0, 2.0, 4.0, true) - 1.0).abs() < 1e-3);
        assert_eq!(t.sample(-3.0, 2.0, 1.0, false), 0.0);
    }
}
