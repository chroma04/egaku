//! Objects: layers described by parameters rather than pixels.
//!
//! A text layer keeps its words, font and transform; an image layer keeps
//! the untouched original and a transform. Their pixels are a cache rendered
//! from that description, so they can be moved, scaled, rotated and edited
//! any number of times without degrading, and rasterized on demand.

use std::sync::{Arc, Mutex, OnceLock};

use cosmic_text::{
    Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight,
    Wrap,
};

use crate::tile::TileGrid;
use crate::transform::{Affine, Source};

/// A placed image.
#[derive(Clone)]
pub struct ImageObject {
    pub source: Arc<Source>,
    /// Maps source pixels to canvas pixels.
    pub transform: Affine,
    /// Bilinear (true) or nearest-neighbour (false) sampling.
    pub smooth: bool,
}

impl ImageObject {
    pub fn render(&self, width: u32, height: u32) -> TileGrid {
        self.source
            .render(&self.transform, self.smooth, width, height)
    }

    /// The untransformed size.
    pub fn natural_size(&self) -> (f32, f32) {
        (self.source.width as f32, self.source.height as f32)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// Editable text.
#[derive(Clone, Debug, PartialEq)]
pub struct TextObject {
    pub text: String,
    /// Font family name; empty means the default sans-serif.
    pub font: String,
    /// Em size in canvas pixels (before the transform's scale).
    pub size: f32,
    pub color: [u8; 3],
    pub bold: bool,
    pub italic: bool,
    pub align: TextAlign,
    /// Line height as a multiple of the size.
    pub line_height: f32,
    /// Wrap width, or `None` to break lines only at newlines.
    pub wrap: Option<f32>,
    /// Maps text-box coordinates (origin at the box's top-left) to canvas.
    pub transform: Affine,
    /// Laid-out box size in text-box coordinates; filled in by rendering.
    pub bbox: (f32, f32),
}

impl Default for TextObject {
    fn default() -> Self {
        TextObject {
            text: String::new(),
            font: String::new(),
            size: 64.0,
            color: [0, 0, 0],
            bold: false,
            italic: false,
            align: TextAlign::Left,
            line_height: 1.2,
            wrap: None,
            transform: Affine::IDENTITY,
            bbox: (0.0, 0.0),
        }
    }
}

struct Fonts {
    system: FontSystem,
    cache: SwashCache,
}

fn fonts() -> &'static Mutex<Fonts> {
    static F: OnceLock<Mutex<Fonts>> = OnceLock::new();
    F.get_or_init(|| {
        Mutex::new(Fonts {
            system: FontSystem::new(),
            cache: SwashCache::new(),
        })
    })
}

/// Loads the system font database in the background so the first text
/// edit doesn't stall.
pub fn warm_up_fonts() {
    std::thread::spawn(|| {
        drop(fonts().lock());
    });
}

/// Installed font families, sorted.
pub fn font_families() -> Vec<String> {
    let f = fonts().lock().unwrap();
    let mut v: Vec<String> = f
        .system
        .db()
        .faces()
        .filter_map(|face| face.families.first().map(|(n, _)| n.clone()))
        .collect();
    v.sort_by_key(|s| s.to_lowercase());
    v.dedup();
    v
}

impl TextObject {
    /// Rasterises the text in its own box at `scale` pixels per unit,
    /// returning straight-alpha RGBA, its size, the padding around the box,
    /// and the unscaled box size.
    fn rasterize(&self, scale: f32) -> (u32, u32, Vec<u8>, f32, (f32, f32)) {
        let mut guard = fonts().lock().unwrap();
        let Fonts { system, cache } = &mut *guard;
        let size = (self.size * scale).max(1.0);
        let metrics = Metrics::new(size, size * self.line_height.max(0.5));
        let mut buf = Buffer::new(system, metrics);
        buf.set_wrap(if self.wrap.is_some() {
            Wrap::WordOrGlyph
        } else {
            Wrap::None
        });
        let family = if self.font.is_empty() {
            Family::SansSerif
        } else {
            Family::Name(&self.font)
        };
        let attrs = Attrs::new()
            .family(family)
            .weight(if self.bold {
                Weight::BOLD
            } else {
                Weight::NORMAL
            })
            .style(if self.italic {
                Style::Italic
            } else {
                Style::Normal
            });
        let align = match self.align {
            TextAlign::Left => Align::Left,
            TextAlign::Center => Align::Center,
            TextAlign::Right => Align::Right,
        };
        let text = if self.text.is_empty() { " " } else { &self.text };
        // Lay out once to measure, then again at that width so alignment
        // has a box to align within.
        buf.set_size(self.wrap.map(|w| w * scale), None);
        buf.set_text(text, &attrs, Shaping::Advanced, Some(align));
        buf.shape_until_scroll(system, false);
        let measure = |buf: &Buffer| {
            let mut w: f32 = 0.0;
            let mut h: f32 = 0.0;
            for run in buf.layout_runs() {
                w = w.max(run.line_w);
                h = h.max(run.line_top + run.line_height);
            }
            (w, h)
        };
        let (mw, _) = measure(&buf);
        let box_w = self.wrap.map_or(mw, |w| w * scale).max(1.0);
        buf.set_size(Some(box_w), None);
        buf.shape_until_scroll(system, false);
        let (_, h) = measure(&buf);
        let box_h = h.max(size);
        // Glyphs (italics, descenders, swashes) can overhang the box.
        let pad = (size * 0.5).ceil();
        let (w, hh) = (
            (box_w + 2.0 * pad).ceil() as u32,
            (box_h + 2.0 * pad).ceil() as u32,
        );
        let mut rgba = vec![0u8; (w * hh * 4) as usize];
        let [r, g, b] = self.color;
        buf.draw(system, cache, Color::rgb(r, g, b), |x, y, gw, gh, c| {
            let a = c.a() as u32;
            if a == 0 {
                return;
            }
            for yy in 0..gh as i32 {
                for xx in 0..gw as i32 {
                    let px = x + xx + pad as i32;
                    let py = y + yy + pad as i32;
                    if px < 0 || py < 0 || px >= w as i32 || py >= hh as i32 {
                        continue;
                    }
                    let o = ((py as u32 * w + px as u32) * 4) as usize;
                    // Straight-alpha source-over.
                    let da = rgba[o + 3] as u32;
                    let oa = a + da * (255 - a) / 255;
                    if oa == 0 {
                        continue;
                    }
                    for (i, s) in [c.r(), c.g(), c.b()].into_iter().enumerate() {
                        let v = (s as u32 * a + rgba[o + i] as u32 * da * (255 - a) / 255) / oa;
                        rgba[o + i] = v as u8;
                    }
                    rgba[o + 3] = oa as u8;
                }
            }
        });
        (w, hh, rgba, pad, (box_w / scale, box_h / scale))
    }

    /// Renders the text onto a canvas-sized grid and records its box size.
    pub fn render(&mut self, width: u32, height: u32) -> TileGrid {
        let (sx, sy) = self.transform.scales();
        // Rasterise at the final scale so text stays sharp when enlarged.
        let scale = sx.max(sy).clamp(0.02, 16.0);
        let (w, h, rgba, pad, bbox) = self.rasterize(scale);
        self.bbox = bbox;
        let src = Source::from_rgba(w, h, &rgba);
        let local = Affine::scale(1.0 / scale, 1.0 / scale).mul(Affine::translate(-pad, -pad));
        let m = self.transform.mul(local);
        src.render(&m, true, width, height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_renders_something() {
        let mut t = TextObject {
            text: "Hello\nworld".into(),
            size: 40.0,
            transform: Affine::translate(10.0, 10.0),
            ..Default::default()
        };
        let g = t.render(400, 300);
        // Some systems in CI may lack fonts entirely; only check when a
        // font was found.
        if font_families().is_empty() {
            return;
        }
        assert!(!g.is_empty());
        assert!(t.bbox.0 > 40.0 && t.bbox.1 > 80.0, "{:?}", t.bbox);
        // Nothing above or left of the transform origin minus overhang.
        assert_eq!(g.pixel(2, 2), [0; 4]);
    }
}
