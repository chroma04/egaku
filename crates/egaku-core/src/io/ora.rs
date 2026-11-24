//! OpenRaster reading and writing.
//!
//! Layers are cropped to their painted bounds and encoded in parallel, so
//! saving a hundred mostly-empty layers is about as fast as saving one.
//! Groups are nested `<stack>` elements, as the spec defines them. What ORA
//! has no words for is kept in `egaku:` attributes that other programs
//! ignore: clipping, text and image objects (each layer's pixels are still
//! written, so other readers show them correctly), and animation frames.
//! The paper colour is written as an ordinary bottom layer (tagged so egaku
//! can recognise it) because that is what every other ORA reader displays.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::path::Path;
use std::sync::Arc;

use quick_xml::events::Event;
use rayon::prelude::*;
use zip::write::SimpleFileOptions;

use super::{Error, Result, check_size, encode_png};
use crate::blend::BlendMode;
use crate::color::unpremultiply;
use crate::document::{Content, Document, Layer};
use crate::object::{ImageObject, TextAlign, TextObject};
use crate::tile::{TILE, TileGrid};
use crate::transform::{Affine, Source};

const NS: &str = "https://github.com/chroma04/egaku";

/// Straight-alpha pixels of a grid's painted bounds: `(x, y, w, h, rgba)`.
fn crop_rgba(grid: &TileGrid, width: u32, height: u32) -> Option<(u32, u32, u32, u32, Vec<u8>)> {
    let b = grid.bounds()?;
    let t = TILE as u32;
    let (x0, y0) = (b.x0 * t, b.y0 * t);
    let (x1, y1) = ((b.x1 * t).min(width), (b.y1 * t).min(height));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
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
                let (dst, _) = line[ox..ox + n * 4].as_chunks_mut::<4>();
                for (d, p) in dst.iter_mut().zip(src) {
                    *d = unpremultiply(*p);
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
        .replace('\n', "&#10;")
        .replace('\r', "")
        .replace('\t', "&#9;")
}

fn fmt_affine(m: &Affine) -> String {
    m.m.iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_affine(s: &str) -> Option<Affine> {
    let v: Vec<f32> = s
        .split_whitespace()
        .filter_map(|x| x.parse().ok())
        .collect();
    (v.len() == 6).then(|| Affine {
        m: [v[0], v[1], v[2], v[3], v[4], v[5]],
    })
}

/// One image file to encode: grid, its size, and the archive path.
struct Blob<'a> {
    grid: &'a TileGrid,
    w: u32,
    h: u32,
    path: String,
}

/// Serialises a document as OpenRaster bytes.
pub fn save_to_vec(doc: &Document) -> Result<Vec<u8>> {
    let (w, h) = (doc.width, doc.height);
    let mut blobs: Vec<Blob> = vec![];
    let mut xml = format!(
        "<?xml version='1.0' encoding='UTF-8'?>\n<image version=\"0.0.6\" w=\"{w}\" h=\"{h}\" xmlns:egaku=\"{NS}\" egaku:frames=\"{}\" egaku:frame=\"{}\" egaku:fps=\"{}\">\n<stack>\n",
        doc.state.frames, doc.state.frame, doc.state.fps
    );
    let mut n = 0usize;
    // Pass 1: XML and the list of images (positions filled in after encoding
    // via placeholders, since crop offsets are only known then).
    fn write_list<'a>(
        doc: &Document,
        ls: &'a [Layer],
        xml: &mut String,
        blobs: &mut Vec<Blob<'a>>,
        n: &mut usize,
    ) {
        let (w, h) = (doc.width, doc.height);
        for l in ls.iter().rev() {
            let common = format!(
                " name=\"{}\" opacity=\"{:.4}\" visibility=\"{}\" composite-op=\"{}\"{}{}{}",
                esc(&l.name),
                l.opacity,
                if l.visible { "visible" } else { "hidden" },
                l.blend.ora_name(),
                if l.id == doc.active_id() {
                    " selected=\"true\""
                } else {
                    ""
                },
                if l.clip { " egaku:clip=\"true\"" } else { "" },
                if l.alpha_lock {
                    " egaku:alpha-lock=\"true\""
                } else {
                    ""
                },
            );
            if let Content::Group { children, expanded } = &l.content {
                *xml += &format!(
                    "<stack{common} isolation=\"{}\"{}>\n",
                    if l.blend == BlendMode::PassThrough {
                        "auto"
                    } else {
                        "isolate"
                    },
                    if *expanded {
                        ""
                    } else {
                        " egaku:collapsed=\"true\""
                    }
                );
                write_list(doc, children, xml, blobs, n);
                *xml += "</stack>\n";
                continue;
            }
            let i = *n;
            *n += 1;
            let src = format!("data/layer{i}.png");
            blobs.push(Blob {
                grid: &l.tiles,
                w,
                h,
                path: src.clone(),
            });
            let mut extra = String::new();
            match &l.content {
                Content::Text(t) => {
                    extra += &format!(
                        " egaku:kind=\"text\" egaku:text=\"{}\" egaku:font=\"{}\" egaku:size=\"{}\" egaku:color=\"{}\" egaku:align=\"{}\" egaku:line-height=\"{}\" egaku:transform=\"{}\"",
                        esc(&t.text),
                        esc(&t.font),
                        t.size,
                        crate::color::Rgb::from_u8(t.color).to_hex(),
                        match t.align {
                            TextAlign::Left => "left",
                            TextAlign::Center => "center",
                            TextAlign::Right => "right",
                        },
                        t.line_height,
                        fmt_affine(&t.transform)
                    );
                    if t.bold {
                        extra += " egaku:bold=\"true\"";
                    }
                    if t.italic {
                        extra += " egaku:italic=\"true\"";
                    }
                    if t.pixel {
                        extra += " egaku:pixel=\"true\"";
                    }
                    if let Some(ww) = t.wrap {
                        extra += &format!(" egaku:wrap=\"{ww}\"");
                    }
                }
                Content::Image(o) => {
                    let sp = format!("data/source{i}.png");
                    blobs.push(Blob {
                        grid: &o.source.base,
                        w: o.source.width,
                        h: o.source.height,
                        path: sp.clone(),
                    });
                    extra += &format!(
                        " egaku:kind=\"image\" egaku:source=\"{sp}\" egaku:source-size=\"{} {}\" egaku:transform=\"{}\"{}",
                        o.source.width,
                        o.source.height,
                        fmt_affine(&o.transform),
                        if o.smooth {
                            ""
                        } else {
                            " egaku:smooth=\"false\""
                        }
                    );
                }
                _ => {
                    let mut cels = vec![];
                    for (f, c) in l.cels.iter().enumerate() {
                        if f == doc.state.frame {
                            continue;
                        }
                        if let Some(c) = c {
                            let p = format!("data/layer{i}-f{f}.png");
                            blobs.push(Blob {
                                grid: c,
                                w,
                                h,
                                path: p.clone(),
                            });
                            cels.push(format!("{f}:{p}@CPOS{p}@"));
                        }
                    }
                    if !cels.is_empty() {
                        extra += &format!(" egaku:cels=\"{}\"", cels.join(";"));
                    }
                }
            }
            *xml += &format!("<layer{common} src=\"{src}\" @POS{src}@{extra} />\n");
        }
    }
    write_list(doc, doc.layers(), &mut xml, &mut blobs, &mut n);

    // Encode every image, plus the merged image and thumbnail, in parallel.
    let encoded: Vec<Option<(u32, u32, Vec<u8>)>> = blobs
        .par_iter()
        .map(|b| {
            crop_rgba(b.grid, b.w, b.h)
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
    for (b, e) in blobs.iter().zip(&encoded) {
        let pos = match e {
            Some((x, y, _)) => format!("x=\"{x}\" y=\"{y}\""),
            None => "x=\"0\" y=\"0\"".to_string(),
        };
        xml = xml.replace(&format!("@POS{}@", b.path), &pos);
        let (x, y) = e.as_ref().map_or((0, 0), |(x, y, _)| (*x, *y));
        xml = xml.replace(&format!("@CPOS{}@", b.path), &format!(":{x}:{y}"));
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
    for (b, e) in blobs.iter().zip(&encoded) {
        if let Some((_, _, png)) = e {
            zw.start_file(&b.path, stored)?;
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

type Attrs = HashMap<String, String>;

enum Node {
    Layer(Attrs),
    Group(Attrs, Vec<Node>),
}

pub fn load(path: &Path) -> Result<Document> {
    load_from_reader(std::fs::File::open(path)?)
}

pub fn load_from_reader<R: Read + std::io::Seek>(r: R) -> Result<Document> {
    let mut zip = zip::ZipArchive::new(r)?;
    let mut xml = String::new();
    zip.by_name("stack.xml")?.read_to_string(&mut xml)?;

    let mut reader = quick_xml::Reader::from_str(&xml);
    let mut image: Attrs = HashMap::new();
    // Open stacks: their attributes and children so far (top first).
    let mut stacks: Vec<(Attrs, Vec<Node>)> = vec![];
    let mut root: Vec<Node> = vec![];
    loop {
        match reader
            .read_event()
            .map_err(|e| Error::Format(e.to_string()))?
        {
            Event::Eof => break,
            Event::End(e) if e.name().0 == "stack" => {
                if let Some((a, ch)) = stacks.pop() {
                    match stacks.last_mut() {
                        Some((_, parent)) => parent.push(Node::Group(a, ch)),
                        None => root = ch,
                    }
                }
            }
            ev @ (Event::Start(_) | Event::Empty(_)) => {
                let is_start = matches!(ev, Event::Start(_));
                let (Event::Start(e) | Event::Empty(e)) = ev else {
                    unreachable!()
                };
                let mut attrs = HashMap::new();
                for a in e.attributes().flatten() {
                    let k = a.key.0.to_string();
                    let v = a
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map(|v| v.into_owned())
                        .unwrap_or_default();
                    attrs.insert(k, v);
                }
                match e.name().0 {
                    "image" => image = attrs,
                    "stack" if is_start => stacks.push((attrs, vec![])),
                    "stack" => {
                        if let Some((_, p)) = stacks.last_mut() {
                            p.push(Node::Group(attrs, vec![]));
                        }
                    }
                    "layer" => {
                        if let Some((_, p)) = stacks.last_mut() {
                            p.push(Node::Layer(attrs));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    let num = |a: &Attrs, k: &str| a.get(k).and_then(|v| v.parse::<u32>().ok());
    let (w, h) = (num(&image, "w").unwrap_or(0), num(&image, "h").unwrap_or(0));
    check_size(w, h)?;

    // Paper: a tagged layer at the very bottom of the root stack.
    let mut paper_src = None;
    if let Some(Node::Layer(a)) = root.last()
        && a.get("egaku:paper").map(String::as_str) == Some("true")
    {
        paper_src = a.get("src").cloned();
        root.pop();
    }

    // Gather every image path to read.
    let mut paths: Vec<String> = vec![];
    fn collect(ns: &[Node], out: &mut Vec<String>) {
        for n in ns {
            match n {
                Node::Layer(a) => {
                    for k in ["src", "egaku:source"] {
                        if let Some(s) = a.get(k).filter(|s| !s.is_empty()) {
                            out.push(s.clone());
                        }
                    }
                    if let Some(c) = a.get("egaku:cels") {
                        for part in c.split(';') {
                            if let Some(p) = part.split(':').nth(1) {
                                out.push(p.to_string());
                            }
                        }
                    }
                }
                Node::Group(_, ch) => collect(ch, out),
            }
        }
    }
    collect(&root, &mut paths);
    if let Some(p) = &paper_src {
        paths.push(p.clone());
    }
    paths.sort();
    paths.dedup();
    // Read compressed PNGs serially (zip access is sequential), decode in parallel.
    let mut data: Vec<(String, Vec<u8>)> = vec![];
    for p in &paths {
        let mut d = vec![];
        if let Ok(mut f) = zip.by_name(p) {
            f.read_to_end(&mut d)?;
        }
        data.push((p.clone(), d));
    }
    let decoded: HashMap<String, image::RgbaImage> = data
        .par_iter()
        .filter(|(_, d)| !d.is_empty())
        .map(|(p, d)| Ok((p.clone(), image::load_from_memory(d)?.into_rgba8())))
        .collect::<Result<_>>()?;

    let paper = paper_src.and_then(|p| decoded.get(&p)).map(|img| {
        let p = img.get_pixel(0, 0).0;
        [p[0], p[1], p[2]]
    });
    let mut doc = Document::empty(w, h, paper);
    let frames = num(&image, "egaku:frames").unwrap_or(1).max(1) as usize;
    let frame = (num(&image, "egaku:frame").unwrap_or(0) as usize).min(frames - 1);
    doc.state.frames = frames;
    doc.state.frame = frame;
    doc.state.fps = num(&image, "egaku:fps").unwrap_or(12).clamp(1, 60);

    let grid_at = |path: Option<&String>, x: i64, y: i64| -> TileGrid {
        match path.and_then(|p| decoded.get(p)) {
            Some(img) => {
                let (iw, ih) = img.dimensions();
                super::grid_from_rgba_at(w, h, img.as_raw(), x, y, iw, ih)
            }
            None => TileGrid::new(w, h),
        }
    };
    let mut active = None;
    fn build(
        doc: &mut Document,
        ns: Vec<Node>,
        grid_at: &dyn Fn(Option<&String>, i64, i64) -> TileGrid,
        decoded: &HashMap<String, image::RgbaImage>,
        active: &mut Option<u64>,
    ) -> Vec<Layer> {
        let mut out = vec![];
        // Listed top first.
        for n in ns.into_iter().rev() {
            let a = match &n {
                Node::Layer(a) | Node::Group(a, _) => a.clone(),
            };
            let get = |k: &str| a.get(k).map(String::as_str);
            let t = |k: &str| get(k) == Some("true");
            let f = |k: &str, d: f32| get(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
            let mut l = match n {
                Node::Group(_, ch) => {
                    let mut g = doc.new_group(get("name").unwrap_or("Group"));
                    let kids = build(doc, ch, grid_at, decoded, active);
                    g.content = Content::Group {
                        children: kids,
                        expanded: !t("egaku:collapsed"),
                    };
                    g
                }
                Node::Layer(_) => {
                    let mut l = doc.new_layer(get("name").unwrap_or("Layer"));
                    let x = get("x").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let y = get("y").and_then(|v| v.parse().ok()).unwrap_or(0);
                    l.tiles = Arc::new(grid_at(a.get("src"), x, y));
                    match get("egaku:kind") {
                        Some("text") => {
                            let obj = TextObject {
                                text: get("egaku:text").unwrap_or("").to_string(),
                                font: get("egaku:font").unwrap_or("").to_string(),
                                size: f("egaku:size", 64.0),
                                color: get("egaku:color")
                                    .and_then(crate::color::Rgb::from_hex)
                                    .map_or([0; 3], |c| c.to_u8()),
                                bold: t("egaku:bold"),
                                italic: t("egaku:italic"),
                                align: match get("egaku:align") {
                                    Some("center") => TextAlign::Center,
                                    Some("right") => TextAlign::Right,
                                    _ => TextAlign::Left,
                                },
                                line_height: f("egaku:line-height", 1.2),
                                wrap: get("egaku:wrap").and_then(|v| v.parse().ok()),
                                transform: get("egaku:transform")
                                    .and_then(parse_affine)
                                    .unwrap_or_default(),
                                bbox: (0.0, 0.0),
                                pixel: t("egaku:pixel"),
                            };
                            let mut obj = obj;
                            // Re-render to recover the box; fall back to the
                            // stored pixels if no font matches.
                            let g = obj.render(doc.width, doc.height);
                            if !g.is_empty() {
                                l.tiles = Arc::new(g);
                            }
                            l.content = Content::Text(Arc::new(obj));
                            l.cels.clear();
                        }
                        Some("image") => {
                            if let Some(img) = get("egaku:source").and_then(|p| decoded.get(p)) {
                                let (iw, ih) = img.dimensions();
                                let src = Source::new(
                                    super::grid_from_rgba_at(iw, ih, img.as_raw(), 0, 0, iw, ih),
                                    iw,
                                    ih,
                                );
                                l.content = Content::Image(Arc::new(ImageObject {
                                    source: Arc::new(src),
                                    transform: get("egaku:transform")
                                        .and_then(parse_affine)
                                        .unwrap_or_default(),
                                    smooth: get("egaku:smooth") != Some("false"),
                                }));
                                l.cels.clear();
                            }
                        }
                        _ => {
                            if let Some(c) = get("egaku:cels") {
                                for part in c.split(';') {
                                    let v: Vec<&str> = part.split(':').collect();
                                    if v.len() == 4
                                        && let (Ok(fi), Ok(x), Ok(y)) =
                                            (v[0].parse::<usize>(), v[2].parse(), v[3].parse())
                                        && fi < l.cels.len()
                                    {
                                        let p = v[1].to_string();
                                        l.cels[fi] = Some(Arc::new(grid_at(Some(&p), x, y)));
                                    }
                                }
                            }
                        }
                    }
                    l
                }
            };
            l.opacity = f("opacity", 1.0).clamp(0.0, 1.0);
            l.visible = get("visibility") != Some("hidden");
            let op = get("composite-op").unwrap_or("");
            l.blend = BlendMode::from_ora_name(op);
            // A stack that may share its backdrop and blends normally is a
            // Pass Through group (Krita may name it outright).
            if l.is_group()
                && ((get("isolation") == Some("auto")
                    && matches!(l.blend, BlendMode::Normal)
                    && op != "krita:dissolve")
                    || op == "krita:pass through")
            {
                l.blend = BlendMode::PassThrough;
            } else if !l.is_group() && l.blend == BlendMode::PassThrough {
                l.blend = BlendMode::Normal;
            }
            l.alpha_lock = t("egaku:alpha-lock");
            l.clip = t("egaku:clip");
            if t("selected") {
                *active = Some(l.id);
            }
            out.push(l);
        }
        out
    }
    let layers = build(&mut doc, root, &grid_at, &decoded, &mut active);
    doc.state.layers = layers;
    if doc.state.layers.is_empty() {
        let l = doc.new_layer("Layer 1");
        doc.state.layers.push(l);
    }
    doc.state.active = active
        .filter(|id| doc.find(*id).is_some_and(|l| !l.is_group()) || doc.find(*id).is_some())
        .unwrap_or_else(|| doc.state.layers.last().unwrap().id);
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::{BrushParams, Paint, PaintMode};
    use crate::document::Place;
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
        let bottom = ed.doc.active_id();
        ed.line(b, p, (20.0, 20.0), (280.0, 180.0)).unwrap();
        ed.add_layer();
        let mid = ed.doc.active_id();
        ed.set_layer(mid, "props", |l| {
            l.name = "Ink & \"shade\"".into();
            l.blend = BlendMode::Multiply;
            l.opacity = 0.5;
            l.alpha_lock = true;
            l.clip = true;
        });
        ed.add_layer();
        let top = ed.doc.active_id();
        ed.set_layer(top, "hide", |l| l.visible = false);
        ed.group_layer(top);
        let g = ed.doc.active_id();
        ed.set_layer(g, "rename", |l| {
            l.name = "Folder".into();
            l.blend = BlendMode::PassThrough;
        });
        ed.select_layer(mid);
        let _ = Place {
            parent: None,
            index: 0,
        };

        let bytes = save_to_vec(&ed.doc).unwrap();
        let doc = load_from_reader(Cursor::new(bytes)).unwrap();
        assert_eq!((doc.width, doc.height), (300, 200));
        assert_eq!(doc.state.paper, Some([250, 240, 230]));
        assert_eq!(doc.layers().len(), 3);
        assert_eq!(doc.active_layer().name, "Ink & \"shade\"");
        let l = &doc.layers()[1];
        assert_eq!(l.name, "Ink & \"shade\"");
        assert_eq!(l.blend, BlendMode::Multiply);
        assert!((l.opacity - 0.5).abs() < 1e-3);
        assert!(l.alpha_lock && l.clip);
        let grp = &doc.layers()[2];
        assert!(grp.is_group());
        assert_eq!(grp.name, "Folder");
        assert_eq!(grp.blend, BlendMode::PassThrough);
        assert_eq!(grp.children().len(), 1);
        assert!(!grp.children()[0].visible);
        // Pixels survive (within premultiplication rounding).
        let src = &ed.doc.find(bottom).unwrap().tiles;
        for (x, y) in [(150, 100), (20, 20), (100, 150), (299, 199)] {
            let a = src.pixel(x, y);
            let b = doc.layers()[0].tiles.pixel(x, y);
            for c in 0..4 {
                assert!(
                    (a[c] as i32 - b[c] as i32).abs() <= 1,
                    "{x},{y}: {a:?} vs {b:?}"
                );
            }
        }
    }

    #[test]
    fn objects_and_frames_roundtrip() {
        let mut ed = Editor::new(Document::new(256, 128, None));
        let rgba: Vec<u8> = (0..32 * 16).flat_map(|i| [i as u8, 50, 60, 255]).collect();
        ed.place_image("pic", Source::from_rgba(32, 16, &rgba), Some((100.0, 50.0)));
        ed.add_text(TextObject {
            text: "Hi\nthere & <you>".into(),
            size: 20.0,
            color: [10, 20, 30],
            bold: true,
            pixel: true,
            transform: Affine::translate(5.0, 6.0),
            ..Default::default()
        });
        ed.add_layer();
        ed.line(
            BrushParams::default(),
            Paint {
                color: [1, 1, 1],
                mode: PaintMode::Paint,
                alpha_lock: false,
            },
            (200.0, 100.0),
            (210.0, 100.0),
        )
        .unwrap();
        ed.add_frame(false);
        ed.line(
            BrushParams::default(),
            Paint {
                color: [2, 2, 2],
                mode: PaintMode::Paint,
                alpha_lock: false,
            },
            (20.0, 100.0),
            (30.0, 100.0),
        )
        .unwrap();
        ed.set_fps(8);
        let doc = load_from_reader(Cursor::new(save_to_vec(&ed.doc).unwrap())).unwrap();
        assert_eq!(
            (doc.state.frames, doc.state.frame, doc.state.fps),
            (2, 1, 8)
        );
        match &doc.layers()[1].content {
            Content::Image(o) => {
                assert_eq!((o.source.width, o.source.height), (32, 16));
                assert_eq!(o.source.base.pixel(5, 0), [5, 50, 60, 255]);
            }
            _ => panic!("expected image"),
        }
        match &doc.layers()[2].content {
            Content::Text(t) => {
                assert_eq!(t.text, "Hi\nthere & <you>");
                assert!(t.bold);
                assert!(t.pixel);
                assert_eq!(t.color, [10, 20, 30]);
                assert_eq!(t.transform, Affine::translate(5.0, 6.0));
            }
            _ => panic!("expected text"),
        }
        let l = &doc.layers()[3];
        assert_eq!(l.tiles.pixel(25, 100), [2, 2, 2, 255]);
        assert_eq!(l.cels[0].as_ref().unwrap().pixel(205, 100), [1, 1, 1, 255]);
    }
}
