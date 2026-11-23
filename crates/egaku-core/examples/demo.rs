//! Builds a small showcase document (used for the README screenshot):
//! a placed photo object, a gradient, a group with a clipped shading layer
//! and a text title.
//!
//! `cargo run --release -p egaku-core --example demo -- demo.ora`

use std::sync::Arc;

use egaku_core::adjust::Gradient;
use egaku_core::brush::{BrushParams, Paint, PaintMode};
use egaku_core::object::TextObject;
use egaku_core::transform::{Affine, Source};
use egaku_core::{Document, Editor};

/// A procedural "photo": sky, sun and layered hills.
fn photo(w: u32, h: u32) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let mut c = [
                (70.0 + 150.0 * fy) as f32,
                (120.0 + 90.0 * fy) as f32,
                (200.0 - 20.0 * fy) as f32,
            ];
            let d = ((fx - 0.7).powi(2) + (fy - 0.35).powi(2)).sqrt();
            let sun = (1.0 - d / 0.12).clamp(0.0, 1.0);
            for (i, s) in [255.0, 236.0, 190.0].iter().enumerate() {
                c[i] += (s - c[i]) * sun.powf(0.6);
            }
            for (k, col) in [
                [64.0, 110.0, 90.0],
                [40.0, 80.0, 70.0],
                [26, 52, 50].map(|v| v as f32),
            ]
            .iter()
            .enumerate()
            {
                let k = k as f32;
                let hill = 0.55 + k * 0.12 + (fx * (5.0 + k * 3.0) + k).sin() * 0.05;
                if fy > hill {
                    c = *col;
                }
            }
            let o = ((y * w + x) * 4) as usize;
            px[o..o + 4].copy_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, 255]);
        }
    }
    px
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "demo.ora".into());
    let (w, h) = (2400u32, 1600u32);
    let mut ed = Editor::new(Document::new(w, h, Some([246, 243, 236])));
    let paper = ed.doc.active_id();
    ed.set_layer(paper, "Rename", |l| l.name = "Background".into());
    ed.apply_filter(
        "Gradient",
        Arc::new(Gradient {
            radial: true,
            from: (1200.0, 700.0),
            to: (2400.0, 1600.0),
            c0: [252, 248, 240, 255],
            c1: [222, 214, 200, 255],
            opacity: 1.0,
        }),
    )
    .unwrap();

    let (pw, ph) = (1600u32, 1000u32);
    let img = ed.place_image(
        "Landscape photo",
        Source::from_rgba(pw, ph, &photo(pw, ph)),
        None,
    );
    ed.begin_transform(true).unwrap();
    ed.set_transform(
        Affine::translate(1320.0, 760.0)
            .compose(Affine::rotate(-0.06))
            .compose(Affine::scale(0.62, 0.62))
            .compose(Affine::translate(-1200.0, -800.0)),
        true,
    );
    ed.commit_transform();
    let _ = img;

    ed.add_layer();
    let sketch = ed.doc.active_id();
    ed.set_layer(sketch, "Rename", |l| l.name = "Ink".into());
    let ink = BrushParams {
        size: 9.0,
        hardness: 0.95,
        pressure_size: false,
        smoothing: 0.0,
        ..Default::default()
    };
    let p = |c| Paint {
        color: c,
        mode: PaintMode::Paint,
        alpha_lock: false,
    };
    // A loose brush flourish under the title.
    let mut prev = (300.0, 520.0);
    for i in 1..=60 {
        let t = i as f32 / 60.0;
        let pt = (
            300.0 + t * 820.0,
            520.0 + (t * 9.0).sin() * 18.0 * (1.0 - t),
        );
        ed.line(ink, p([214, 88, 58]), prev, pt).unwrap();
        prev = pt;
    }
    ed.add_layer();
    let shade = ed.doc.active_id();
    ed.set_layer(shade, "Rename", |l| {
        l.name = "Glow (clipped)".into();
        l.blend = egaku_core::blend::BlendMode::Screen;
    });
    ed.line(
        BrushParams {
            size: 120.0,
            hardness: 0.0,
            flow: 0.3,
            pressure_size: false,
            smoothing: 0.0,
            ..Default::default()
        },
        p([255, 214, 120]),
        (300.0, 520.0),
        (1120.0, 520.0),
    )
    .unwrap();
    ed.toggle_clip(shade);
    ed.select_layer(sketch);
    ed.group_layer(sketch);
    let g = ed.doc.active_id();
    ed.set_layer(g, "Rename", |l| l.name = "Lettering".into());
    // Move the clipped layer into the group above its base.
    if let Some(place) = ed.doc.place_of(sketch) {
        ed.move_layer(
            shade,
            egaku_core::document::Place {
                parent: place.parent,
                index: 1,
            },
        );
    }

    ed.add_text(TextObject {
        text: "Summer sketchbook".into(),
        size: 120.0,
        color: [34, 34, 40],
        bold: true,
        transform: Affine::translate(290.0, 300.0),
        ..Default::default()
    });
    ed.add_text(TextObject {
        text: "Painting, photos, pixels and frames,\nall in one quiet place.".into(),
        size: 46.0,
        color: [90, 90, 100],
        line_height: 1.35,
        transform: Affine::translate(296.0, 600.0),
        ..Default::default()
    });
    egaku_core::io::ora::save(&ed.doc, std::path::Path::new(&path)).unwrap();
    println!("wrote {path}");
}
