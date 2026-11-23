//! Tiny vector icons drawn with the painter, so they stay crisp at any UI
//! scale and need no font or image assets.

use eframe::egui::{self, Color32, Painter, Pos2, Rect, Shape, Stroke, vec2};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Icon {
    Brush,
    Eraser,
    Fill,
    Picker,
    Move,
    Hand,
    Eye,
    EyeOff,
    Lock,
    Plus,
    Trash,
    Duplicate,
    MergeDown,
    Mirror,
}

/// Paints `icon` centred in `rect`.
pub fn paint(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height());
    let o = rect.center() - vec2(s, s) * 0.5;
    let at = |x: f32, y: f32| -> Pos2 { o + vec2(x * s, y * s) };
    let w = (s * 0.09).max(1.2);
    let st = Stroke::new(w, color);
    match icon {
        Icon::Brush => {
            p.line_segment(
                [at(0.82, 0.18), at(0.48, 0.52)],
                Stroke::new(w * 1.4, color),
            );
            p.add(Shape::convex_polygon(
                vec![
                    at(0.44, 0.48),
                    at(0.52, 0.56),
                    at(0.40, 0.72),
                    at(0.18, 0.84),
                    at(0.28, 0.62),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Eraser => {
            let pts = [
                at(0.15, 0.62),
                at(0.52, 0.25),
                at(0.85, 0.58),
                at(0.48, 0.95),
            ];
            p.add(Shape::closed_line(pts.to_vec(), st));
            p.add(Shape::convex_polygon(
                vec![
                    at(0.15, 0.62),
                    at(0.33, 0.44),
                    at(0.66, 0.77),
                    at(0.48, 0.95),
                ],
                color,
                Stroke::NONE,
            ));
            p.line_segment([at(0.48, 0.92), at(0.9, 0.92)], Stroke::new(w * 0.8, color));
        }
        Icon::Fill => {
            let pts = [at(0.18, 0.5), at(0.48, 0.2), at(0.78, 0.5), at(0.48, 0.8)];
            p.add(Shape::closed_line(pts.to_vec(), st));
            p.add(Shape::convex_polygon(
                vec![at(0.18, 0.5), at(0.78, 0.5), at(0.48, 0.8)],
                color,
                Stroke::NONE,
            ));
            p.circle_filled(at(0.86, 0.74), s * 0.08, color);
            p.add(Shape::convex_polygon(
                vec![at(0.86, 0.56), at(0.92, 0.70), at(0.80, 0.70)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Picker => {
            p.line_segment([at(0.2, 0.8), at(0.62, 0.38)], Stroke::new(w * 1.2, color));
            p.line_segment([at(0.5, 0.3), at(0.7, 0.5)], Stroke::new(w * 1.4, color));
            p.circle_filled(at(0.72, 0.28), s * 0.13, color);
            p.circle_filled(at(0.2, 0.8), w * 0.9, color);
        }
        Icon::Move => {
            let c = at(0.5, 0.5);
            for d in [
                vec2(1.0, 0.0),
                vec2(-1.0, 0.0),
                vec2(0.0, 1.0),
                vec2(0.0, -1.0),
            ] {
                let tip = c + d * s * 0.4;
                p.line_segment([c, tip], st);
                let n = vec2(-d.y, d.x);
                p.add(Shape::convex_polygon(
                    vec![
                        tip + d * s * 0.04,
                        tip - d * s * 0.13 + n * s * 0.1,
                        tip - d * s * 0.13 - n * s * 0.1,
                    ],
                    color,
                    Stroke::NONE,
                ));
            }
        }
        Icon::Hand => {
            p.rect_filled(
                Rect::from_min_max(at(0.28, 0.48), at(0.74, 0.86)),
                s * 0.12,
                color,
            );
            for (i, h) in [0.30, 0.18, 0.20, 0.30].iter().enumerate() {
                let x = 0.31 + i as f32 * 0.13;
                p.line_segment([at(x, 0.56), at(x, *h)], Stroke::new(w * 1.5, color));
            }
            p.line_segment(
                [at(0.30, 0.70), at(0.16, 0.52)],
                Stroke::new(w * 1.5, color),
            );
        }
        Icon::Eye | Icon::EyeOff => {
            let mut pts = vec![];
            for i in 0..=16 {
                let t = i as f32 / 16.0;
                pts.push(at(
                    0.1 + 0.8 * t,
                    0.5 - (t * std::f32::consts::PI).sin() * 0.24,
                ));
            }
            for i in (0..=16).rev() {
                let t = i as f32 / 16.0;
                pts.push(at(
                    0.1 + 0.8 * t,
                    0.5 + (t * std::f32::consts::PI).sin() * 0.24,
                ));
            }
            p.add(Shape::closed_line(pts, st));
            if icon == Icon::Eye {
                p.circle_filled(at(0.5, 0.5), s * 0.12, color);
            } else {
                p.line_segment([at(0.18, 0.82), at(0.82, 0.18)], st);
            }
        }
        Icon::Lock => {
            p.rect_filled(
                Rect::from_min_max(at(0.22, 0.46), at(0.78, 0.86)),
                s * 0.06,
                color,
            );
            let mut arc = vec![];
            for i in 0..=12 {
                let a = std::f32::consts::PI * (1.0 + i as f32 / 12.0);
                arc.push(at(0.5 + a.cos() * 0.18, 0.46 + a.sin() * 0.24));
            }
            p.add(Shape::line(arc, st));
        }
        Icon::Plus => {
            p.line_segment([at(0.5, 0.2), at(0.5, 0.8)], st);
            p.line_segment([at(0.2, 0.5), at(0.8, 0.5)], st);
        }
        Icon::Trash => {
            p.line_segment([at(0.18, 0.28), at(0.82, 0.28)], st);
            p.line_segment([at(0.4, 0.28), at(0.42, 0.16)], st);
            p.line_segment([at(0.42, 0.16), at(0.58, 0.16)], st);
            p.line_segment([at(0.58, 0.16), at(0.6, 0.28)], st);
            p.add(Shape::closed_line(
                vec![
                    at(0.26, 0.34),
                    at(0.74, 0.34),
                    at(0.68, 0.86),
                    at(0.32, 0.86),
                ],
                st,
            ));
        }
        Icon::Duplicate => {
            p.rect_stroke(
                Rect::from_min_max(at(0.18, 0.32), at(0.64, 0.82)),
                2.0,
                st,
                egui::StrokeKind::Middle,
            );
            p.rect_stroke(
                Rect::from_min_max(at(0.36, 0.16), at(0.82, 0.66)),
                2.0,
                st,
                egui::StrokeKind::Middle,
            );
        }
        Icon::MergeDown => {
            p.line_segment([at(0.5, 0.14), at(0.5, 0.62)], st);
            p.add(Shape::convex_polygon(
                vec![at(0.32, 0.5), at(0.68, 0.5), at(0.5, 0.7)],
                color,
                Stroke::NONE,
            ));
            p.line_segment(
                [at(0.18, 0.84), at(0.82, 0.84)],
                Stroke::new(w * 1.3, color),
            );
        }
        Icon::Mirror => {
            p.line_segment([at(0.5, 0.12), at(0.5, 0.88)], Stroke::new(w * 0.7, color));
            p.add(Shape::convex_polygon(
                vec![at(0.42, 0.22), at(0.42, 0.78), at(0.12, 0.78)],
                color,
                Stroke::NONE,
            ));
            p.add(Shape::closed_line(
                vec![at(0.58, 0.22), at(0.88, 0.78), at(0.58, 0.78)],
                st,
            ));
        }
    }
}

/// A square, frameless icon button. Returns its response.
pub fn button(
    ui: &mut egui::Ui,
    icon: Icon,
    size: f32,
    selected: bool,
    tip: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), egui::Sense::click());
    let v = ui.visuals();
    let bg = if selected {
        crate::theme::ACCENT_DIM
    } else if resp.hovered() {
        v.widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    let fg = if selected {
        crate::theme::ACCENT
    } else if resp.hovered() {
        Color32::WHITE
    } else {
        crate::theme::TEXT
    };
    paint(ui.painter(), rect.shrink(size * 0.2), icon, fg);
    resp.on_hover_text(tip)
}
