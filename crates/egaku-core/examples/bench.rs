//! Rough performance check: `cargo run --release -p egaku-core --example bench`
use std::time::Instant;

use egaku_core::brush::{BrushParams, Paint, PaintMode};
use egaku_core::tile::TileRect;
use egaku_core::{Document, Editor};

fn main() {
    let (w, h) = (8192, 8192);
    let layers = 200;
    let mut ed = Editor::new(Document::new(w, h, Some([255; 3])));
    let b = BrushParams {
        size: 120.0,
        hardness: 0.6,
        smoothing: 0.0,
        ..Default::default()
    };
    let p = |c| Paint {
        color: c,
        mode: PaintMode::Paint,
        alpha_lock: false,
    };

    let t = Instant::now();
    for i in 0..layers {
        if i > 0 {
            ed.add_layer();
        }
        let y = (i as f32 * 37.0) % h as f32;
        ed.line(
            b,
            p([i as u8, 100, 200]),
            (0.0, y),
            (w as f32, (y + 2000.0) % h as f32),
        )
        .unwrap();
    }
    println!(
        "{layers} layers with a full-width stroke each: {:?}",
        t.elapsed()
    );
    println!(
        "layer pixel memory: {} MiB, history: {} MiB",
        ed.doc.pixel_bytes() >> 20,
        ed.history.memory() >> 20
    );

    let t = Instant::now();
    ed.projection.invalidate_all();
    ed.update_projection(None, usize::MAX);
    println!(
        "full composite of {}x{} with {layers} layers: {:?}",
        w,
        h,
        t.elapsed()
    );

    // A stroke on the bottom layer under 199 others, as if in a 2560x1440 view.
    ed.select_layer(0);
    let vis = TileRect {
        x0: 0,
        y0: 0,
        x1: 40,
        y1: 23,
    };
    ed.begin_stroke(b, p([0, 0, 0]), 100.0, 100.0, 1.0).unwrap();
    let mut frames = vec![];
    for i in 0..120 {
        for k in 0..4 {
            let s = (i * 4 + k) as f32;
            ed.stroke_to(
                100.0 + s * 4.0,
                100.0 + (s * 0.05).sin() * 300.0 + 300.0,
                1.0,
            );
        }
        let t = Instant::now();
        ed.update_projection(Some(vis), 64);
        frames.push(t.elapsed());
    }
    let t = Instant::now();
    ed.end_stroke();
    let commit = t.elapsed();
    frames.sort();
    println!(
        "painting under 199 layers: median frame {:?}, p95 {:?}, max {:?}; commit {:?}",
        frames[60], frames[114], frames[119], commit
    );
    let t = Instant::now();
    ed.undo();
    ed.update_projection(None, usize::MAX);
    println!("undo + recomposite: {:?}", t.elapsed());

    let t = Instant::now();
    let bytes = egaku_core::io::ora::save_to_vec(&ed.doc).unwrap();
    println!("ORA save: {:?} ({} MiB)", t.elapsed(), bytes.len() >> 20);
    let t = Instant::now();
    let d = egaku_core::io::ora::load_from_reader(std::io::Cursor::new(bytes)).unwrap();
    println!("ORA load: {:?} ({} layers)", t.elapsed(), d.layers().len());
}
