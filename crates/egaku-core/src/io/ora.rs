//! OpenRaster reading and writing.
//!
//! Layers are cropped to their painted bounds and encoded in parallel, so
//! saving a hundred mostly-empty layers is about as fast as saving one. The
//! paper colour is written as an ordinary bottom layer (tagged so egaku can
//! recognise it) because that is what every other ORA reader will display.

use std::io::{Cursor, Read, Write};
use std::path::Path;
use std::sync::Arc;

use quick_xml::events::Event;
use rayon::prelude::*;
use zip::write::SimpleFileOptions;

use super::{Error, Result, check_size, encode_png, grid_from_rgba};
use crate::blend::BlendMode;
use crate::color::unpremultiply;
use crate::document::{Document, Layer};
use crate::tile::{TILE, TileGrid};

const NS: &str = "https://github.com/chroma04/egaku";

/// Straight-alpha pixels of a grid's painted bounds: `(x, y, w, h, rgba)`.
fn crop_rgba(grid: &TileGrid, width: u32, height: u32) -> Option<(u32, u32, u32, u32, Vec<u8>)> {
    let b = grid.bounds()?;
    let t = TILE as u32;
    let (x0, y0) = (b.x0 * t, b.y0 * t);
    let (x1, y1) = ((b.x1 * t).min(width), (b.y1 * t).min(height));
    let (w, h) = (x1 - x0, y1 - y0);
    let mut out = vec![0u8; (w * h * 4) as usize];
    out.par_chunks_mut((w * 4) as usize)
        .enumerate()
        .for_each(|(row, line)| {
            let y = y0 + row as u32;
            let (ty, ly) = (y / t, (y % t) as usize);
            for tx in b.x0..b.x1 {
                let Some(tile) = grid.get(crate::tile::TileCoord::new(tx, ty)) else {
                    continue;
                };
                let ox = ((tx * t - x0) * 4) as usize;
                let n = (t.min(width - tx * t)) as usize;
                let src = &tile.px[ly * TILE..ly * TILE + n];
                for (d, p) in line[ox..ox + n * 4].chunks_exact_mut(4).zip(src) {
                    d.copy_from_slice(&unpremultiply(*p));
                }
            }
        });
    Some((x0, y0, w, h, out))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Serialises a document as OpenRaster bytes.
pub fn save_to_vec(doc: &Document) -> Result<Vec<u8>> {
    let (w, h) = (doc.width, doc.height);
    // Encode every layer, plus the merged image and thumbnail, in parallel.
    let layers: Vec<Option<(u32, u32, Vec<u8>)>> = doc
        .layers()
        .par_iter()
        .map(|l| {
            crop_rgba(&l.tiles, w, h)
                .map(|(x, y, cw, ch, rgba)| {
                    encode_png(cw, ch, &rgba, png::Compression::Fast).map(|png| (x, y, png))
                })
                .transpose()
        })
        .collect::<Result<_>>()?;
    let merged = crate::render::flatten(doc);
    let (merged_png, thumb_png) = rayon::join(
        || encode_png(w, h, &merged, png::Compression::Fast),
        || {
            let (tw, th, t) = thumbnail_rgba(&merged, w, h, 256);
            encode_png(tw, th, &t, png::Compression::Fast)
        },
    );
    let (merged_png, thumb_png) = (merged_png?, thumb_png?);

    let mut xml = format!(
        "<?xml version='1.0' encoding='UTF-8'?>\n<image version=\"0.0.6\" w=\"{w}\" h=\"{h}\" xmlns:egaku=\"{NS}\">\n<stack>\n"
    );
    for (i, l) in doc.layers().iter().enumerate().rev() {
        let (x, y, src) = match &layers[i] {
            Some((x, y, _)) => (*x, *y, format!("data/layer{i}.png")),
            None => (0, 0, String::new()),
        };
        let src_attr = if src.is_empty() {
            String::new()
        } else {
            format!(" src=\"{src}\"")
        };
        xml += &format!(
            "<layer name=\"{}\"{src_attr} x=\"{x}\" y=\"{y}\" opacity=\"{:.4}\" visibility=\"{}\" composite-op=\"{}\"{}{} />\n",
            esc(&l.name),
            l.opacity,
            if l.visible { "visible" } else { "hidden" },
            l.blend.ora_name(),
            if i == doc.active() {
                " selected=\"true\""
            } else {
                ""
            },
            if l.alpha_lock {
                " egaku:alpha-lock=\"true\""
            } else {
                ""
            },
        );
    }
    let paper_png = match doc.state.paper {
        Some([r, g, b]) => {
            xml += "<layer name=\"Paper\" src=\"data/paper.png\" x=\"0\" y=\"0\" opacity=\"1.0\" visibility=\"visible\" composite-op=\"svg:src-over\" egaku:paper=\"true\" />\n";
            let row: Vec<u8> = (0..w).flat_map(|_| [r, g, b, 255]).collect();
            let all: Vec<u8> = (0..h).flat_map(|_| row.iter().copied()).collect();
            Some(encode_png(w, h, &all, png::Compression::Fast)?)
        }
        None => None,
    };
    xml += "</stack>\n</image>\n";

    let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zw.start_file("mimetype", stored)?;
    zw.write_all(b"image/openraster")?;
    zw.start_file("stack.xml", deflated)?;
    zw.write_all(xml.as_bytes())?;
    for (i, l) in layers.iter().enumerate() {
        if let Some((_, _, png)) = l {
            zw.start_file(format!("data/layer{i}.png"), stored)?;
            zw.write_all(png)?;
        }
    }
    if let Some(p) = paper_png {
        zw.start_file("data/paper.png", stored)?;
        zw.write_all(&p)?;
    }
    zw.start_file("mergedimage.png", stored)?;
    zw.write_all(&merged_png)?;
    zw.start_file("Thumbnails/thumbnail.png", stored)?;
    zw.write_all(&thumb_png)?;
    Ok(zw.finish()?.into_inner())
}

pub fn save(doc: &Document, path: &Path) -> Result<()> {
    super::write_atomic(path, &save_to_vec(doc)?)
}

fn thumbnail_rgba(rgba: &[u8], w: u32, h: u32, max: u32) -> (u32, u32, Vec<u8>) {
    let s = (max as f32 / w.max(h) as f32).min(1.0);
    let (tw, th) = (
        ((w as f32 * s) as u32).max(1),
        ((h as f32 * s) as u32).max(1),
    );
    let mut out = vec![0u8; (tw * th * 4) as usize];
    for y in 0..th {
        for x in 0..tw {
            let sx = ((x as f32 + 0.5) / s) as u32;
            let sy = ((y as f32 + 0.5) / s) as u32;
            let i = ((sy.min(h - 1) * w + sx.min(w - 1)) * 4) as usize;
            let o = ((y * tw + x) * 4) as usize;
            out[o..o + 4].copy_from_slice(&rgba[i..i + 4]);
        }
    }
    (tw, th, out)
}

#[derive(Default)]
struct LayerSpec {
    name: String,
    src: String,
    x: i64,
    y: i64,
    opacity: f32,
    visible: bool,
    blend: BlendMode,
    selected: bool,
    alpha_lock: bool,
    paper: bool,
}

pub fn load(path: &Path) -> Result<Document> {
    load_from_reader(std::fs::File::open(path)?)
}

pub fn load_from_reader<R: Read + std::io::Seek>(r: R) -> Result<Document> {
    let mut zip = zip::ZipArchive::new(r)?;
    let mut xml = String::new();
    zip.by_name("stack.xml")?.read_to_string(&mut xml)?;

    let mut reader = quick_xml::Reader::from_str(&xml);
    let (mut w, mut h) = (0u32, 0u32);
    let mut specs: Vec<LayerSpec> = vec![];
    // Opacity/visibility of enclosing groups; egaku has no groups, so nested
    // stacks are flattened with their properties folded into each layer.
    let mut groups: Vec<(f32, bool)> = vec![];
    loop {
        match reader
            .read_event()
            .map_err(|e| Error::Format(e.to_string()))?
        {
            Event::Eof => break,
            Event::End(e) if e.name().0 == "stack" => {
                groups.pop();
            }
            ev @ (Event::Start(_) | Event::Empty(_)) => {
                let is_start = matches!(ev, Event::Start(_));
                let (Event::Start(e) | Event::Empty(e)) = ev else {
                    unreachable!()
                };
                let mut attrs = std::collections::HashMap::new();
                for a in e.attributes().flatten() {
                    let k = a.key.0.to_string();
                    let v = a
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map(|v| v.into_owned())
                        .unwrap_or_default();
                    attrs.insert(k, v);
                }
                let get = |k: &str| attrs.get(k).map(String::as_str);
                let opacity = get("opacity")
                    .and_then(|v| v.parse::<f32>().ok())
                    .unwrap_or(1.0)
                    .clamp(0.0, 1.0);
                let visible = get("visibility") != Some("hidden");
                match e.name().0 {
                    "image" => {
                        w = get("w").and_then(|v| v.parse().ok()).unwrap_or(0);
                        h = get("h").and_then(|v| v.parse().ok()).unwrap_or(0);
                    }
                    "stack" if is_start => {
                        let (po, pv) = groups.last().copied().unwrap_or((1.0, true));
                        // The root stack has no attributes, so this is identity.
                        groups.push((po * opacity, pv && visible));
                    }
                    "layer" => {
                        let (go, gv) = groups.last().copied().unwrap_or((1.0, true));
                        specs.push(LayerSpec {
                            name: get("name").unwrap_or("Layer").to_string(),
                            src: get("src").unwrap_or("").to_string(),
                            x: get("x").and_then(|v| v.parse().ok()).unwrap_or(0),
                            y: get("y").and_then(|v| v.parse().ok()).unwrap_or(0),
                            opacity: opacity * go,
                            visible: visible && gv,
                            blend: BlendMode::from_ora_name(get("composite-op").unwrap_or("")),
                            selected: get("selected") == Some("true"),
                            alpha_lock: get("egaku:alpha-lock") == Some("true"),
                            paper: get("egaku:paper") == Some("true"),
                        });
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    check_size(w, h)?;
    // stack.xml lists layers top first.
    specs.reverse();

    let mut paper = None;
    if specs.first().is_some_and(|s| s.paper) {
        let s = specs.remove(0);
        let mut data = vec![];
        zip.by_name(&s.src)?.read_to_end(&mut data)?;
        let img = image::load_from_memory(&data)?.into_rgba8();
        let p = img.get_pixel(0, 0).0;
        paper = Some([p[0], p[1], p[2]]);
    }

    // Read compressed PNGs serially (zip access is sequential), decode in parallel.
    let mut blobs = Vec::with_capacity(specs.len());
    for s in &specs {
        let mut data = vec![];
        if !s.src.is_empty() {
            zip.by_name(&s.src)?.read_to_end(&mut data)?;
        }
        blobs.push(data);
    }
    let grids: Vec<TileGrid> = specs
        .par_iter()
        .zip(blobs.par_iter())
        .map(|(s, data)| {
            if data.is_empty() {
                return Ok(TileGrid::new(w, h));
            }
            let img = image::load_from_memory(data)?.into_rgba8();
            let (iw, ih) = img.dimensions();
            Ok(super::grid_from_rgba_at(
                w,
                h,
                img.as_raw(),
                s.x,
                s.y,
                iw,
                ih,
            ))
        })
        .collect::<Result<_>>()?;

    let mut doc = Document::empty(w, h, paper);
    let mut active = None;
    for (i, (s, g)) in specs.into_iter().zip(grids).enumerate() {
        let mut l: Layer = doc.new_layer(s.name);
        l.opacity = s.opacity;
        l.visible = s.visible;
        l.blend = s.blend;
        l.alpha_lock = s.alpha_lock;
        l.tiles = Arc::new(g);
        if s.selected {
            active = Some(i);
        }
        doc.state.layers.push(l);
    }
    if doc.state.layers.is_empty() {
        let l = doc.new_layer("Layer 1");
        doc.state.layers.push(l);
    }
    doc.state.active = active.unwrap_or(doc.state.layers.len() - 1);
    let _ = grid_from_rgba;
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::{BrushParams, Paint, PaintMode};
    use crate::editor::Editor;

    #[test]
    fn roundtrip() {
        let mut ed = Editor::new(Document::new(300, 200, Some([250, 240, 230])));
        let p = Paint {
            color: [200, 10, 10],
            mode: PaintMode::Paint,
            alpha_lock: false,
        };
        let b = BrushParams {
            size: 30.0,
            hardness: 0.5,
            ..Default::default()
        };
        ed.line(b, p, (20.0, 20.0), (280.0, 180.0)).unwrap();
        ed.add_layer();
        ed.commit("props", |d| {
            let l = d.active_layer_mut();
            l.name = "Ink & \"shade\"".into();
            l.blend = BlendMode::Multiply;
            l.opacity = 0.5;
            l.alpha_lock = true;
        });
        ed.add_layer();
        ed.commit("hide", |d| d.active_layer_mut().visible = false);
        ed.select_layer(1);

        let bytes = save_to_vec(&ed.doc).unwrap();
        let doc = load_from_reader(Cursor::new(bytes)).unwrap();
        assert_eq!((doc.width, doc.height), (300, 200));
        assert_eq!(doc.state.paper, Some([250, 240, 230]));
        assert_eq!(doc.layers().len(), 3);
        assert_eq!(doc.active(), 1);
        let l = &doc.layers()[1];
        assert_eq!(l.name, "Ink & \"shade\"");
        assert_eq!(l.blend, BlendMode::Multiply);
        assert!((l.opacity - 0.5).abs() < 1e-3);
        assert!(l.alpha_lock);
        assert!(!doc.layers()[2].visible);
        // Pixels survive (within premultiplication rounding).
        for (x, y) in [(150, 100), (20, 20), (100, 150), (299, 199)] {
            let a = ed.doc.layers()[0].tiles.pixel(x, y);
            let b = doc.layers()[0].tiles.pixel(x, y);
            for c in 0..4 {
                assert!(
                    (a[c] as i32 - b[c] as i32).abs() <= 1,
                    "{x},{y}: {a:?} vs {b:?}"
                );
            }
        }
    }
}
