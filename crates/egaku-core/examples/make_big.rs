//! Writes a large multi-layer test document:
//! `cargo run --release -p egaku-core --example make_big -- out.ora [size] [layers]`
use egaku_core::brush::{BrushParams, Paint, PaintMode};
use egaku_core::{Document, Editor};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args.get(1).map_or("big.ora", String::as_str);
    let size: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8192);
    let layers: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(80);
    let mut ed = Editor::new(Document::new(size, size, Some([246, 243, 236])));
    let s = size as f32;
    for i in 0..layers {
        if i > 0 {
            ed.add_layer();
        }
        let t = i as f32 / layers as f32;
        let hue = egaku_core::color::Hsv {
            h: t,
            s: 0.6,
            v: 0.85,
        }
        .to_rgb()
        .to_u8();
        let b = BrushParams {
            size: s * 0.02 + 40.0 * (i % 5) as f32,
            hardness: 0.7,
            smoothing: 0.0,
            opacity: 0.8,
            ..Default::default()
        };
        let p = Paint {
            color: hue,
            mode: PaintMode::Paint,
            alpha_lock: false,
        };
        let (cx, cy, r) = (
            s * (0.2 + 0.6 * ((i * 37) % 100) as f32 / 100.0),
            s * (0.2 + 0.6 * ((i * 61) % 100) as f32 / 100.0),
            s * 0.12,
        );
        let mut prev = (cx + r, cy);
        for k in 1..=24 {
            let a = k as f32 / 24.0 * std::f32::consts::TAU;
            let next = (cx + r * a.cos(), cy + r * a.sin() * 0.7);
            ed.line(b, p, prev, next).unwrap();
            prev = next;
        }
    }
    egaku_core::io::ora::save(&ed.doc, std::path::Path::new(out)).unwrap();
    println!("wrote {out}");
}
