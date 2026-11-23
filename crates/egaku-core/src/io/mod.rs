//! File formats.
//!
//! egaku's native format is OpenRaster (`.ora`), the open layered format
//! shared by Krita, GIMP and MyPaint, so work is never locked into this app.
//! Flat images (PNG, JPEG, WebP) can be opened, and PNG is the export format.

pub mod ora;

use std::path::Path;
use std::sync::Arc;

use crate::color::premultiply;
use crate::document::{Document, MAX_CANVAS};
use crate::tile::{TILE, TileCoord, TileGrid};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Format(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Format(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<zip::result::ZipError> for Error {
    fn from(e: zip::result::ZipError) -> Self {
        Error::Format(e.to_string())
    }
}

impl From<image::ImageError> for Error {
    fn from(e: image::ImageError) -> Self {
        Error::Format(e.to_string())
    }
}

impl From<png::EncodingError> for Error {
    fn from(e: png::EncodingError) -> Self {
        Error::Format(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Extensions that [`open`] understands.
pub const OPEN_EXTENSIONS: &[&str] = &["ora", "png", "jpg", "jpeg", "webp", "gif"];

/// Flat image formats that can be imported.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif"];

/// A decoded flat image, converted to sRGB.
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha sRGB RGBA rows.
    pub rgba: Vec<u8>,
    /// Whether an embedded colour profile was converted from.
    pub converted: bool,
}

/// Decodes an image file. Embedded ICC profiles (Display P3 photos from
/// phones, Adobe RGB from cameras…) are converted to sRGB, egaku's working
/// space, so colours look the same as in the viewer they came from.
pub fn decode(path: &Path) -> Result<Decoded> {
    let reader = image::ImageReader::open(path)?.with_guessed_format()?;
    decode_reader(reader)
}

pub fn decode_bytes(bytes: &[u8]) -> Result<Decoded> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    decode_reader(reader)
}

fn decode_reader<R: std::io::BufRead + std::io::Seek>(r: image::ImageReader<R>) -> Result<Decoded> {
    use image::ImageDecoder;
    let mut dec = r.into_decoder()?;
    let icc = dec.icc_profile().ok().flatten();
    let img = image::DynamicImage::from_decoder(dec)?.into_rgba8();
    let (width, height) = img.dimensions();
    check_size(width, height)?;
    let mut rgba = img.into_raw();
    let converted = icc.is_some_and(|p| to_srgb(&mut rgba, width, &p));
    Ok(Decoded {
        width,
        height,
        rgba,
        converted,
    })
}

/// Converts RGBA rows from an ICC profile to sRGB in place. Returns false
/// (leaving pixels alone) for sRGB itself or profiles that can't be used.
pub fn to_srgb(rgba: &mut [u8], width: u32, icc: &[u8]) -> bool {
    use moxcms::{ColorProfile, Layout, TransformOptions};
    use rayon::prelude::*;
    let Ok(src) = ColorProfile::new_from_slice(icc) else {
        return false;
    };
    let dst = ColorProfile::new_srgb();
    // Skip the work when the profile already is sRGB.
    if src.red_colorant == dst.red_colorant
        && src.green_colorant == dst.green_colorant
        && src.blue_colorant == dst.blue_colorant
        && src.red_trc == dst.red_trc
    {
        return false;
    }
    let Ok(t) = src.create_transform_8bit(
        Layout::Rgba,
        &dst,
        Layout::Rgba,
        TransformOptions::default(),
    ) else {
        return false;
    };
    let row = width as usize * 4;
    rgba.par_chunks_mut(row * 32).for_each(|band| {
        let src = band.to_vec();
        let _ = t.transform(&src, band);
    });
    true
}

/// Opens any supported file as a document.
pub fn open(path: &Path) -> Result<Document> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "ora" {
        ora::load(path)
    } else {
        let d = decode(path)?;
        let mut doc = Document::empty(d.width, d.height, None);
        let mut layer = doc.new_layer(path.file_stem().and_then(|s| s.to_str()).unwrap_or("Image"));
        layer.tiles = Arc::new(grid_from_rgba(
            d.width, d.height, &d.rgba, 0, 0, d.width, d.height,
        ));
        doc.state.active = layer.id;
        doc.state.layers.push(layer);
        Ok(doc)
    }
}

/// File formats for flat export.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Format {
    Png,
    /// Quality 1..=100. Transparency is flattened onto white.
    Jpeg(u8),
    /// Lossless.
    Webp,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg(_) => "jpg",
            Format::Webp => "webp",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Format> {
        match ext.to_ascii_lowercase().as_str() {
            "png" => Some(Format::Png),
            "jpg" | "jpeg" => Some(Format::Jpeg(90)),
            "webp" => Some(Format::Webp),
            _ => None,
        }
    }
}

/// Enlarges straight RGBA by an integer factor with nearest neighbour
/// (crisp pixel-art exports).
pub fn upscale(w: u32, h: u32, rgba: &[u8], k: u32) -> (u32, u32, Vec<u8>) {
    if k <= 1 {
        return (w, h, rgba.to_vec());
    }
    let (nw, nh) = (w * k, h * k);
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        for x in 0..nw {
            let s = (((y / k) * w + x / k) * 4) as usize;
            let d = ((y * nw + x) * 4) as usize;
            out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
        }
    }
    (nw, nh, out)
}

/// Encodes straight RGBA in a flat format.
pub fn encode(w: u32, h: u32, rgba: &[u8], format: Format) -> Result<Vec<u8>> {
    match format {
        Format::Png => encode_png(w, h, rgba, png::Compression::Balanced),
        Format::Jpeg(q) => {
            // Flatten onto white: JPEG has no alpha.
            let rgb: Vec<u8> = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| {
                    let a = p[3] as u32;
                    [0, 1, 2].map(|i| ((p[i] as u32 * a + 255 * (255 - a) + 127) / 255) as u8)
                })
                .collect();
            let mut buf = vec![];
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, q.clamp(1, 100)).encode(
                &rgb,
                w,
                h,
                image::ExtendedColorType::Rgb8,
            )?;
            Ok(buf)
        }
        Format::Webp => {
            let mut buf = vec![];
            image::codecs::webp::WebPEncoder::new_lossless(&mut buf).encode(
                rgba,
                w,
                h,
                image::ExtendedColorType::Rgba8,
            )?;
            Ok(buf)
        }
    }
}

/// Writes the flattened document as an image file.
pub fn export(doc: &Document, path: &Path, format: Format, scale: u32) -> Result<()> {
    let rgba = crate::render::flatten(doc);
    let (w, h, rgba) = upscale(doc.width, doc.height, &rgba, scale);
    write_atomic(path, &encode(w, h, &rgba, format)?)
}

/// Writes animation frames (straight RGBA, all `w × h`) as an animated GIF
/// that loops forever.
pub fn export_gif(path: &Path, w: u32, h: u32, frames: &[Vec<u8>], fps: u32) -> Result<()> {
    use image::codecs::gif::{GifEncoder, Repeat};
    let mut buf = vec![];
    {
        let mut enc = GifEncoder::new_with_speed(&mut buf, 10);
        enc.set_repeat(Repeat::Infinite)?;
        let delay = image::Delay::from_numer_denom_ms(1000, fps.max(1));
        let fr = frames.iter().map(|f| {
            image::Frame::from_parts(
                image::RgbaImage::from_raw(w, h, f.clone()).expect("frame size"),
                0,
                0,
                delay,
            )
        });
        enc.encode_frames(fr)?;
    }
    write_atomic(path, &buf)
}

/// Writes animation frames as an animated PNG (full colour and alpha).
pub fn export_apng(path: &Path, w: u32, h: u32, frames: &[Vec<u8>], fps: u32) -> Result<()> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        enc.set_animated(frames.len() as u32, 0)?;
        enc.set_frame_delay(1, fps.clamp(1, 60) as u16)?;
        let mut wr = enc.write_header()?;
        for f in frames {
            wr.write_image_data(f)?;
        }
        wr.finish()?;
    }
    write_atomic(path, &buf)
}

/// Lays frames out in a grid (left to right, top to bottom).
pub fn sprite_sheet(w: u32, h: u32, frames: &[Vec<u8>], columns: u32) -> (u32, u32, Vec<u8>) {
    let cols = columns.clamp(1, frames.len().max(1) as u32);
    let rows = (frames.len() as u32).div_ceil(cols).max(1);
    let (sw, sh) = (w * cols, h * rows);
    let mut out = vec![0u8; (sw * sh * 4) as usize];
    for (i, f) in frames.iter().enumerate() {
        let (cx, cy) = (i as u32 % cols, i as u32 / cols);
        for y in 0..h {
            let d = (((cy * h + y) * sw + cx * w) * 4) as usize;
            let s = (y * w * 4) as usize;
            out[d..d + (w * 4) as usize].copy_from_slice(&f[s..s + (w * 4) as usize]);
        }
    }
    (sw, sh, out)
}

/// Decodes an image file into a layer-sized grid, centred (for "import as
/// layer").
pub fn decode_to_grid(path: &Path, canvas_w: u32, canvas_h: u32) -> Result<TileGrid> {
    let d = decode(path)?;
    let x = (canvas_w as i64 - d.width as i64) / 2;
    let y = (canvas_h as i64 - d.height as i64) / 2;
    Ok(grid_from_rgba_at(
        canvas_w, canvas_h, &d.rgba, x, y, d.width, d.height,
    ))
}

pub(crate) fn check_size(w: u32, h: u32) -> Result<()> {
    if w == 0 || h == 0 || w > MAX_CANVAS || h > MAX_CANVAS {
        return Err(Error::Format(format!("unsupported image size {w}×{h}")));
    }
    Ok(())
}

/// Builds a grid of the canvas size from straight-alpha RGBA rows placed at
/// `(x, y)`. Fully transparent tiles stay unallocated.
pub fn grid_from_rgba(cw: u32, ch: u32, rgba: &[u8], x: u32, y: u32, w: u32, h: u32) -> TileGrid {
    grid_from_rgba_at(cw, ch, rgba, x as i64, y as i64, w, h)
}

pub fn grid_from_rgba_at(
    cw: u32,
    ch: u32,
    rgba: &[u8],
    x: i64,
    y: i64,
    w: u32,
    h: u32,
) -> TileGrid {
    use rayon::prelude::*;
    let mut grid = TileGrid::new(cw, ch);
    let rect =
        crate::tile::TileRect::from_px(x, y, x + w as i64, y + h as i64, grid.cols(), grid.rows());
    let coords: Vec<TileCoord> = rect.iter().collect();
    let tiles: Vec<(TileCoord, Option<crate::tile::Tile>)> = coords
        .par_iter()
        .map(|&c| {
            let mut t = crate::tile::Tile::filled([0; 4]);
            let mut any = false;
            for ly in 0..TILE {
                let sy = c.y as i64 * TILE as i64 + ly as i64 - y;
                if sy < 0 || sy >= h as i64 {
                    continue;
                }
                for lx in 0..TILE {
                    let sx = c.x as i64 * TILE as i64 + lx as i64 - x;
                    if sx < 0 || sx >= w as i64 {
                        continue;
                    }
                    let o = ((sy as usize * w as usize) + sx as usize) * 4;
                    let p = premultiply([rgba[o], rgba[o + 1], rgba[o + 2], rgba[o + 3]]);
                    any |= p[3] != 0;
                    t.px[ly * TILE + lx] = p;
                }
            }
            (c, any.then_some(t))
        })
        .collect();
    for (c, t) in tiles {
        if let Some(t) = t {
            grid.set(c, Some(Arc::new(t)));
        }
    }
    grid
}

/// Encodes straight-alpha RGBA rows as PNG.
pub fn encode_png(w: u32, h: u32, rgba: &[u8], compression: png::Compression) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(compression);
        enc.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        let mut wr = enc.write_header()?;
        wr.write_image_data(rgba)?;
    }
    Ok(buf)
}

/// Writes the flattened document as a PNG file.
pub fn export_png(doc: &Document, path: &Path) -> Result<()> {
    export(doc, path, Format::Png, 1)
}

/// Writes via a temporary file and rename, so a crash mid-save never
/// destroys the previous version.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("egaku-tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_formats() {
        let rgba: Vec<u8> = (0..16 * 8).flat_map(|i| [i as u8, 0, 255, 128]).collect();
        for f in [Format::Png, Format::Jpeg(80), Format::Webp] {
            let bytes = encode(16, 8, &rgba, f).unwrap();
            let d = decode_bytes(&bytes).unwrap();
            assert_eq!((d.width, d.height), (16, 8), "{f:?}");
        }
        let (w, h, up) = upscale(16, 8, &rgba, 3);
        assert_eq!((w, h), (48, 24));
        assert_eq!(&up[(3 * 48 + 4) * 4..][..4], &rgba[17 * 4..][..4]);
        let (sw, sh, _) = sprite_sheet(16, 8, &[rgba.clone(), rgba.clone(), rgba], 2);
        assert_eq!((sw, sh), (32, 16));
    }

    #[test]
    fn icc_conversion_changes_wide_gamut() {
        // A Display P3 profile: pure P3 red is outside sRGB and must change.
        let p3 = moxcms::ColorProfile::new_display_p3();
        let Ok(icc) = p3.encode() else { return };
        let mut px = vec![255u8, 0, 0, 255];
        assert!(to_srgb(&mut px, 1, &icc));
        assert_eq!(px[0], 255);
        assert!(px[1] < 60 && px[2] < 60);
        let srgb = moxcms::ColorProfile::new_srgb().encode().unwrap();
        let mut px2 = vec![10u8, 20, 30, 255];
        to_srgb(&mut px2, 1, &srgb);
        assert!((px2[0] as i32 - 10).abs() <= 1);
    }
}
