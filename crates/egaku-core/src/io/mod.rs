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
pub const OPEN_EXTENSIONS: &[&str] = &["ora", "png", "jpg", "jpeg", "webp"];

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
        let img = image::open(path)?.into_rgba8();
        let (w, h) = img.dimensions();
        check_size(w, h)?;
        let mut doc = Document::empty(w, h, None);
        let mut layer = doc.new_layer(path.file_stem().and_then(|s| s.to_str()).unwrap_or("Image"));
        layer.tiles = Arc::new(grid_from_rgba(w, h, img.as_raw(), 0, 0, w, h));
        doc.state.layers.push(layer);
        Ok(doc)
    }
}

/// Decodes an image file into a layer-sized grid (for "import as layer").
pub fn decode_to_grid(path: &Path, canvas_w: u32, canvas_h: u32) -> Result<TileGrid> {
    let img = image::open(path)?.into_rgba8();
    let (w, h) = img.dimensions();
    // Centre the image on the canvas.
    let x = (canvas_w as i64 - w as i64) / 2;
    let y = (canvas_h as i64 - h as i64) / 2;
    Ok(grid_from_rgba_at(
        canvas_w,
        canvas_h,
        img.as_raw(),
        x,
        y,
        w,
        h,
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

fn grid_from_rgba_at(cw: u32, ch: u32, rgba: &[u8], x: i64, y: i64, w: u32, h: u32) -> TileGrid {
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
        let mut wr = enc.write_header()?;
        wr.write_image_data(rgba)?;
    }
    Ok(buf)
}

/// Writes the flattened document as a PNG file.
pub fn export_png(doc: &Document, path: &Path) -> Result<()> {
    let rgba = crate::render::flatten(doc);
    let data = encode_png(doc.width, doc.height, &rgba, png::Compression::Balanced)?;
    write_atomic(path, &data)
}

/// Writes via a temporary file and rename, so a crash mid-save never
/// destroys the previous version.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("egaku-tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
