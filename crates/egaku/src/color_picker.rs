//! HSV colour picker: a saturation/value square over a hue strip.
//!
//! Colour is held as HSV rather than RGB so hue survives passing through
//! greys and black; a classic annoyance in pickers that round-trip RGB.

use eframe::egui::{self, Color32, Mesh, Rect, Sense, Shape, Stroke, pos2, vec2};
use egaku_core::color::{Hsv, Rgb};

fn c32(rgb: Rgb) -> Color32 {
    let [r, g, b] = rgb.to_u8();
    Color32::from_rgb(r, g, b)
}

/// Shows the picker; returns true when the colour changed.
pub fn picker(ui: &mut egui::Ui, hsv: &mut Hsv, width: f32) -> bool {
    let mut changed = false;
    let sq = width;
    let (rect, resp) = ui.allocate_exact_size(vec2(sq, sq * 0.72), Sense::click_and_drag());
    let p = ui.painter_at(rect);

    // Saturation (x) / value (y) gradient as a subdivided mesh.
    const N: usize = 16;
    let mut mesh = Mesh::default();
    for j in 0..=N {
        for i in 0..=N {
            let (s, v) = (i as f32 / N as f32, 1.0 - j as f32 / N as f32);
            let pos = pos2(
                rect.left() + rect.width() * s,
                rect.top() + rect.height() * (1.0 - v),
            );
            mesh.colored_vertex(pos, c32(Hsv { h: hsv.h, s, v }.to_rgb()));
        }
    }
    for j in 0..N as u32 {
        for i in 0..N as u32 {
            let a = j * (N as u32 + 1) + i;
            let b = a + 1;
            let c = a + N as u32 + 1;
            let d = c + 1;
            mesh.add_triangle(a, b, c);
            mesh.add_triangle(b, d, c);
        }
    }
    p.add(Shape::mesh(mesh));
    if let Some(pos) = resp.interact_pointer_pos() {
        hsv.s = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        hsv.v = (1.0 - (pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        changed = true;
    }
    let m = pos2(
        rect.left() + hsv.s * rect.width(),
        rect.top() + (1.0 - hsv.v) * rect.height(),
    );
    p.circle_stroke(m, 5.5, Stroke::new(1.5, Color32::BLACK));
    p.circle_stroke(m, 4.5, Stroke::new(1.5, Color32::WHITE));

    ui.add_space(4.0);
    let (hr, hresp) = ui.allocate_exact_size(vec2(sq, 14.0), Sense::click_and_drag());
    let hp = ui.painter_at(hr);
    let mut mesh = Mesh::default();
    const H: usize = 36;
    for i in 0..=H {
        let t = i as f32 / H as f32;
        let col = c32(Hsv {
            h: t,
            s: 1.0,
            v: 1.0,
        }
        .to_rgb());
        let x = hr.left() + hr.width() * t;
        mesh.colored_vertex(pos2(x, hr.top()), col);
        mesh.colored_vertex(pos2(x, hr.bottom()), col);
        if i > 0 {
            let k = (i as u32) * 2;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k + 1, k);
        }
    }
    hp.add(Shape::mesh(mesh));
    if let Some(pos) = hresp.interact_pointer_pos() {
        hsv.h = ((pos.x - hr.left()) / hr.width()).clamp(0.0, 0.9999);
        changed = true;
    }
    let x = hr.left() + hsv.h * hr.width();
    hp.rect_stroke(
        Rect::from_center_size(pos2(x, hr.center().y), vec2(5.0, hr.height() + 2.0)),
        2.0,
        Stroke::new(1.5, Color32::WHITE),
        egui::StrokeKind::Middle,
    );
    changed
}

pub fn to_hsv(c: [u8; 3]) -> Hsv {
    Rgb::from_u8(c).to_hsv()
}

pub fn from_hsv(h: Hsv) -> [u8; 3] {
    h.to_rgb().to_u8()
}
