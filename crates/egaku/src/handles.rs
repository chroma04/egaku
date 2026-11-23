//! Transform and crop handles: hit testing and the maths of dragging them.

use eframe::egui::{Pos2, Rect, Vec2, pos2};
use egaku_core::transform::Affine;

use crate::canvas::View;

/// Part of a bounding box being dragged.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Handle {
    Inside,
    Rotate,
    /// Corner or edge, as signs: x and y in -1, 0, 1 (0 = not that axis).
    Edge(i8, i8),
    /// A new crop rectangle being drawn.
    New,
}

/// Screen-space corners of a canvas-space quad.
pub fn screen_quad(view: &View, vp: Rect, ppp: f32, q: &[(f32, f32); 4]) -> [Pos2; 4] {
    q.map(|(x, y)| view.to_screen(vp, ppp, pos2(x, y)))
}

/// Handle points around a quad (TL, TR, BR, BL order): the 8 resize
/// handles with their signs.
pub fn handle_points(s: &[Pos2; 4]) -> [(Pos2, (i8, i8)); 8] {
    let mid = |a: Pos2, b: Pos2| a + (b - a) * 0.5;
    [
        (s[0], (-1, -1)),
        (mid(s[0], s[1]), (0, -1)),
        (s[1], (1, -1)),
        (mid(s[1], s[2]), (1, 0)),
        (s[2], (1, 1)),
        (mid(s[2], s[3]), (0, 1)),
        (s[3], (-1, 1)),
        (mid(s[3], s[0]), (-1, 0)),
    ]
}

fn inside(s: &[Pos2; 4], p: Pos2) -> bool {
    // Works for convex quads of either winding.
    let mut sign = 0.0f32;
    for i in 0..4 {
        let (a, b) = (s[i], s[(i + 1) % 4]);
        let c = (b - a).x * (p - a).y - (b - a).y * (p - a).x;
        if c.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = c.signum();
        } else if c.signum() != sign {
            return false;
        }
    }
    true
}

/// What a pointer at `p` would grab.
pub fn hit(s: &[Pos2; 4], p: Pos2, rotate: bool) -> Option<Handle> {
    let r = 9.0;
    // Small boxes: favour moving over edge handles.
    let tiny = (s[0] - s[2]).length() < 40.0;
    for (hp, (sx, sy)) in handle_points(s) {
        if (sx != 0 && sy != 0 || !tiny) && (hp - p).length() <= r {
            return Some(Handle::Edge(sx, sy));
        }
    }
    if inside(s, p) {
        return Some(Handle::Inside);
    }
    if rotate {
        for c in s {
            if (*c - p).length() <= 28.0 {
                return Some(Handle::Rotate);
            }
        }
    }
    None
}

pub fn cursor_for(h: Handle, s: &[Pos2; 4]) -> eframe::egui::CursorIcon {
    use eframe::egui::CursorIcon as C;
    match h {
        Handle::Inside => C::Move,
        Handle::Rotate => C::Alias,
        Handle::New => C::Crosshair,
        Handle::Edge(sx, sy) => {
            // Pick a resize cursor by the on-screen direction of the handle.
            let c = s.iter().fold(Vec2::ZERO, |a, p| a + p.to_vec2()) / 4.0;
            let hp = handle_points(s)
                .into_iter()
                .find(|(_, k)| *k == (sx, sy))
                .map_or(c.to_pos2(), |(p, _)| p);
            let d = hp - c.to_pos2();
            let a = d.y.atan2(d.x).to_degrees().rem_euclid(180.0);
            if !(22.5..157.5).contains(&a) {
                C::ResizeHorizontal
            } else if (67.5..112.5).contains(&a) {
                C::ResizeVertical
            } else if a < 90.0 {
                C::ResizeNwSe
            } else {
                C::ResizeNeSw
            }
        }
    }
}

/// The transform after dragging `h` from canvas point `p0` to `p`.
///
/// `frame` maps box space (0..w, 0..h) to canvas at the start of the
/// transform; `start` is the user transform when the drag began. Corner
/// drags keep proportions unless `free`; `centered` scales about the
/// middle.
#[allow(clippy::too_many_arguments)]
pub fn drag(
    h: Handle,
    frame: Affine,
    size: (f32, f32),
    start: Affine,
    p0: Pos2,
    p: Pos2,
    free: bool,
    centered: bool,
    snap_angle: bool,
) -> Affine {
    let f0 = start.mul(frame);
    let (w, hgt) = size;
    match h {
        Handle::Inside | Handle::New => Affine::translate(p.x - p0.x, p.y - p0.y).mul(start),
        Handle::Rotate => {
            let (cx, cy) = f0.apply(w * 0.5, hgt * 0.5);
            let a0 = (p0.y - cy).atan2(p0.x - cx);
            let mut a = (p.y - cy).atan2(p.x - cx) - a0;
            if snap_angle {
                // Snap the absolute angle, not the change.
                let base = f0.angle();
                let step = 15f32.to_radians();
                a = ((base + a) / step).round() * step - base;
            }
            Affine::translate(cx, cy)
                .mul(Affine::rotate(a))
                .mul(Affine::translate(-cx, -cy))
                .mul(start)
        }
        Handle::Edge(sx, sy) => {
            let Some(inv) = f0.inverse() else { return start };
            let (q0x, q0y) = inv.apply(p0.x, p0.y);
            let (qx, qy) = inv.apply(p.x, p.y);
            let anchor = |s: i8, len: f32| match (s, centered) {
                (_, true) => len * 0.5,
                (1, _) => 0.0,
                (-1, _) => len,
                _ => len * 0.5,
            };
            let (ax, ay) = (anchor(sx, w), anchor(sy, hgt));
            let fac = |q: f32, q0: f32, a: f32| {
                let d0 = q0 - a;
                if d0.abs() < 1e-3 { 1.0 } else { (q - a) / d0 }
            };
            let mut kx = if sx != 0 { fac(qx, q0x, ax) } else { 1.0 };
            let mut ky = if sy != 0 { fac(qy, q0y, ay) } else { 1.0 };
            if sx != 0 && sy != 0 && !free {
                // Proportional: the larger change wins, keeping the sign.
                let k = if kx.abs() > ky.abs() { kx } else { ky };
                kx = k.abs() * kx.signum();
                ky = k.abs() * ky.signum();
            }
            let kx = if kx.abs() < 1e-3 { 1e-3 } else { kx };
            let ky = if ky.abs() < 1e-3 { 1e-3 } else { ky };
            let local = Affine::translate(ax, ay)
                .mul(Affine::scale(kx, ky))
                .mul(Affine::translate(-ax, -ay));
            let f = f0.mul(local);
            match frame.inverse() {
                Some(fi) => f.mul(fi),
                None => start,
            }
        }
    }
}

/// A crop rectangle after dragging handle `h`.
pub fn drag_rect(h: Handle, r: [f32; 4], d: Vec2, w: f32, hgt: f32) -> [f32; 4] {
    let mut r = r;
    match h {
        Handle::Inside => {
            let dx = d.x.clamp(-r[0], w - r[2]);
            let dy = d.y.clamp(-r[1], hgt - r[3]);
            r = [r[0] + dx, r[1] + dy, r[2] + dx, r[3] + dy];
        }
        Handle::Edge(sx, sy) => {
            if sx < 0 {
                r[0] += d.x;
            }
            if sx > 0 {
                r[2] += d.x;
            }
            if sy < 0 {
                r[1] += d.y;
            }
            if sy > 0 {
                r[3] += d.y;
            }
        }
        _ => {}
    }
    normalize(r, w, hgt)
}

/// Orders and clamps a rectangle to the canvas, snapping to whole pixels.
pub fn normalize(r: [f32; 4], w: f32, h: f32) -> [f32; 4] {
    let (x0, x1) = (r[0].min(r[2]), r[0].max(r[2]));
    let (y0, y1) = (r[1].min(r[3]), r[1].max(r[3]));
    [
        x0.round().clamp(0.0, w),
        y0.round().clamp(0.0, h),
        x1.round().clamp(0.0, w),
        y1.round().clamp(0.0, h),
    ]
}

pub fn rect_quad(r: [f32; 4]) -> [(f32, f32); 4] {
    [(r[0], r[1]), (r[2], r[1]), (r[2], r[3]), (r[0], r[3])]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_drag_scales_about_opposite_corner() {
        let frame = Affine::translate(10.0, 10.0);
        let size = (100.0, 50.0);
        // Drag the bottom-right corner out by 100px horizontally.
        let d = drag(
            Handle::Edge(1, 1),
            frame,
            size,
            Affine::IDENTITY,
            pos2(110.0, 60.0),
            pos2(210.0, 60.0),
            false,
            false,
            false,
        );
        let f = d.mul(frame);
        let (x0, y0) = f.apply(0.0, 0.0);
        let (x1, y1) = f.apply(100.0, 50.0);
        assert!((x0 - 10.0).abs() < 1e-3 && (y0 - 10.0).abs() < 1e-3);
        assert!((x1 - 210.0).abs() < 1e-3);
        // Proportional: height doubled too.
        assert!((y1 - 110.0).abs() < 1e-3, "{y1}");
    }

    #[test]
    fn rotate_and_move() {
        let frame = Affine::IDENTITY;
        let d = drag(
            Handle::Rotate,
            frame,
            (100.0, 100.0),
            Affine::IDENTITY,
            pos2(150.0, 50.0),
            pos2(50.0, 150.0),
            false,
            false,
            false,
        );
        let (x, y) = d.apply(50.0, 50.0);
        assert!((x - 50.0).abs() < 1e-3 && (y - 50.0).abs() < 1e-3);
        assert!((d.angle().abs() - std::f32::consts::FRAC_PI_2).abs() < 1e-3);
        let m = drag(
            Handle::Inside,
            frame,
            (1.0, 1.0),
            Affine::IDENTITY,
            pos2(0.0, 0.0),
            pos2(5.0, 7.0),
            false,
            false,
            false,
        );
        assert_eq!(m, Affine::translate(5.0, 7.0));
    }
}
