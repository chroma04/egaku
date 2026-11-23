//! Canvas display: the view transform and GPU texture cache.
//!
//! The composite lives on the CPU (see `egaku_core::render`). The GPU holds
//! 256×256 textures for just the chunks in view, at the pyramid level that
//! matches the zoom, and receives only the 64×64 tiles that changed. Pan,
//! zoom, rotation and mirroring are pure vertex transforms, so navigating
//! never recomposites anything.

use std::collections::HashMap;

use eframe::egui::{
    self, Color32, ColorImage, Mesh, Pos2, Rect, TextureHandle, TextureOptions, Vec2, pos2, vec2,
};
use egaku_core::render::Projection;
use egaku_core::tile::{TILE, TileCoord, TileRect};

/// How the canvas is placed on screen.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct View {
    /// Canvas point shown at the centre of the viewport.
    pub center: Vec2,
    /// Physical screen pixels per canvas pixel, so 1.0 is truly 100%.
    pub zoom: f32,
    /// Radians, clockwise.
    pub rotation: f32,
    /// Horizontal flip.
    pub mirror: bool,
}

pub const MIN_ZOOM: f32 = 1.0 / 64.0;
pub const MAX_ZOOM: f32 = 64.0;

impl Default for View {
    fn default() -> Self {
        View {
            center: Vec2::ZERO,
            zoom: 1.0,
            rotation: 0.0,
            mirror: false,
        }
    }
}

fn rot(v: Vec2, a: f32) -> Vec2 {
    let (s, c) = a.sin_cos();
    vec2(v.x * c - v.y * s, v.x * s + v.y * c)
}

impl View {
    /// Screen points per canvas pixel.
    pub fn scale(&self, ppp: f32) -> f32 {
        self.zoom / ppp
    }

    pub fn to_screen(self, vp: Rect, ppp: f32, c: Pos2) -> Pos2 {
        let mut d = (c.to_vec2() - self.center) * self.scale(ppp);
        if self.mirror {
            d.x = -d.x;
        }
        vp.center() + rot(d, self.rotation)
    }

    pub fn to_canvas(self, vp: Rect, ppp: f32, s: Pos2) -> Pos2 {
        let mut d = rot(s - vp.center(), -self.rotation);
        if self.mirror {
            d.x = -d.x;
        }
        (self.center + d / self.scale(ppp)).to_pos2()
    }

    /// Changes zoom keeping the canvas point under `anchor` fixed.
    pub fn zoom_at(&mut self, vp: Rect, ppp: f32, anchor: Pos2, zoom: f32) {
        let before = self.to_canvas(vp, ppp, anchor);
        let mut zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        // Snap to 100% when passing close by: it is the one zoom level where
        // what you see is exactly what you get.
        if (zoom - 1.0).abs() < 0.03 && (self.zoom - 1.0).abs() >= 0.03 {
            zoom = 1.0;
        }
        self.zoom = zoom;
        let after = self.to_canvas(vp, ppp, anchor);
        self.center += before - after;
    }

    /// Rotates about the viewport centre.
    pub fn set_rotation(&mut self, r: f32) {
        let tau = std::f32::consts::TAU;
        self.rotation = (r + tau * 0.5).rem_euclid(tau) - tau * 0.5;
    }

    /// Flips the view horizontally around the viewport centre, keeping what
    /// is on screen in place apart from the flip.
    pub fn toggle_mirror(&mut self) {
        self.mirror = !self.mirror;
        self.set_rotation(-self.rotation);
    }

    pub fn fit(&mut self, vp: Rect, ppp: f32, w: u32, h: u32) {
        self.rotation = 0.0;
        self.center = vec2(w as f32, h as f32) * 0.5;
        let margin = 48.0;
        let sx = (vp.width() - margin).max(32.0) / w as f32;
        let sy = (vp.height() - margin).max(32.0) / h as f32;
        // Never enlarge past 100% when fitting a small image.
        self.zoom = (sx.min(sy) * ppp).min(1.0).clamp(MIN_ZOOM, MAX_ZOOM);
    }

    /// Canvas-space bounding box of the viewport.
    pub fn visible_canvas(&self, vp: Rect, ppp: f32) -> Rect {
        let pts = [
            vp.left_top(),
            vp.right_top(),
            vp.left_bottom(),
            vp.right_bottom(),
        ]
        .map(|p| self.to_canvas(vp, ppp, p));
        Rect::from_points(&pts)
    }

    /// Pyramid level to display for the current zoom.
    pub fn level(&self, levels: usize) -> usize {
        if self.zoom >= 1.0 {
            return 0;
        }
        ((1.0 / self.zoom).log2().floor().max(0.0) as usize).min(levels - 1)
    }
}

const CHUNK_TILES: u32 = 4;
const CHUNK: usize = TILE * CHUNK_TILES as usize;
/// Upper bound on cached chunk textures (256 KiB each).
const MAX_TEXTURES: usize = 768;

struct Chunk {
    tex: TextureHandle,
    last_used: u64,
    stale: Vec<(u32, u32)>,
}

type Key = (u8, u32, u32);

#[derive(Default)]
pub struct CanvasTextures {
    chunks: HashMap<Key, Chunk>,
    frame: u64,
    checker: Option<TextureHandle>,
}

fn options(level: usize) -> TextureOptions {
    TextureOptions {
        // Crisp, honest pixels when zoomed in; smooth when zoomed out.
        magnification: if level == 0 {
            egui::TextureFilter::Nearest
        } else {
            egui::TextureFilter::Linear
        },
        minification: egui::TextureFilter::Linear,
        wrap_mode: egui::TextureWrapMode::ClampToEdge,
        mipmap_mode: None,
    }
}

impl CanvasTextures {
    pub fn clear(&mut self) {
        self.chunks.clear();
    }

    pub fn texture_count(&self) -> usize {
        self.chunks.len()
    }

    /// Records tiles the projection recomposited.
    pub fn note_changes(&mut self, changes: &[(u8, TileCoord)]) {
        for &(l, c) in changes {
            if let Some(ch) = self
                .chunks
                .get_mut(&(l, c.x / CHUNK_TILES, c.y / CHUNK_TILES))
            {
                ch.stale.push((c.x % CHUNK_TILES, c.y % CHUNK_TILES));
            }
        }
    }

    fn chunk_image(proj: &Projection, level: usize, cx: u32, cy: u32) -> ColorImage {
        let mut buf = vec![0u8; CHUNK * CHUNK * 4];
        for ty in 0..CHUNK_TILES {
            for tx in 0..CHUNK_TILES {
                let Some(t) = proj.tile(
                    level,
                    TileCoord::new(cx * CHUNK_TILES + tx, cy * CHUNK_TILES + ty),
                ) else {
                    continue;
                };
                for y in 0..TILE {
                    let o = ((ty as usize * TILE + y) * CHUNK + tx as usize * TILE) * 4;
                    buf[o..o + TILE * 4]
                        .copy_from_slice(&t.as_bytes()[y * TILE * 4..(y + 1) * TILE * 4]);
                }
            }
        }
        ColorImage::from_rgba_premultiplied([CHUNK, CHUNK], &buf)
    }

    fn texture(
        &mut self,
        ctx: &egui::Context,
        proj: &Projection,
        level: usize,
        cx: u32,
        cy: u32,
    ) -> egui::TextureId {
        let key = (level as u8, cx, cy);
        let frame = self.frame;
        let ch = self.chunks.entry(key).or_insert_with(|| Chunk {
            tex: ctx.load_texture(
                format!("canvas-{level}-{cx}-{cy}"),
                Self::chunk_image(proj, level, cx, cy),
                options(level),
            ),
            last_used: frame,
            stale: vec![],
        });
        ch.last_used = frame;
        if !ch.stale.is_empty() {
            ch.stale.sort_unstable();
            ch.stale.dedup();
            if ch.stale.len() > 8 {
                ch.tex
                    .set(Self::chunk_image(proj, level, cx, cy), options(level));
            } else {
                for &(tx, ty) in &ch.stale {
                    let c = TileCoord::new(cx * CHUNK_TILES + tx, cy * CHUNK_TILES + ty);
                    let img = match proj.tile(level, c) {
                        Some(t) => ColorImage::from_rgba_premultiplied([TILE, TILE], t.as_bytes()),
                        None => {
                            ColorImage::new([TILE, TILE], vec![Color32::TRANSPARENT; TILE * TILE])
                        }
                    };
                    ch.tex.set_partial(
                        [tx as usize * TILE, ty as usize * TILE],
                        img,
                        options(level),
                    );
                }
            }
            ch.stale.clear();
        }
        ch.tex.id()
    }

    fn checker(&mut self, ctx: &egui::Context) -> egui::TextureId {
        self.checker
            .get_or_insert_with(|| {
                let (a, b) = (Color32::from_gray(204), Color32::from_gray(160));
                let img = ColorImage::new([2, 2], vec![a, b, b, a]);
                ctx.load_texture(
                    "checker",
                    img,
                    TextureOptions {
                        magnification: egui::TextureFilter::Nearest,
                        minification: egui::TextureFilter::Nearest,
                        wrap_mode: egui::TextureWrapMode::Repeat,
                        mipmap_mode: None,
                    },
                )
            })
            .id()
    }

    /// Draws the canvas. Returns the level-0 tile rectangle in view, which
    /// the caller should prioritise when recompositing.
    pub fn paint(
        &mut self,
        ui: &egui::Ui,
        vp: Rect,
        view: &View,
        proj: &Projection,
        transparent: bool,
    ) {
        self.frame += 1;
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let painter = ui.painter_at(vp);
        let (w, h) = proj.size();
        let corners = [
            pos2(0.0, 0.0),
            pos2(w as f32, 0.0),
            pos2(w as f32, h as f32),
            pos2(0.0, h as f32),
        ]
        .map(|c| view.to_screen(vp, ppp, c));

        // Soft drop shadow, then the transparency checkerboard if needed.
        for (i, a) in [(10.0, 10u8), (5.0, 22), (2.0, 40)] {
            let c = Color32::from_black_alpha(a);
            let center = corners.iter().fold(Vec2::ZERO, |s, p| s + p.to_vec2()) / 4.0;
            let pts: Vec<Pos2> = corners
                .iter()
                .map(|p| {
                    let d = (*p - center.to_pos2()).normalized();
                    *p + d * i + vec2(0.0, i * 0.4)
                })
                .collect();
            painter.add(egui::Shape::convex_polygon(pts, c, egui::Stroke::NONE));
        }
        if transparent {
            let tex = self.checker(&ctx);
            let mut m = Mesh::with_texture(tex);
            let cell = 8.0; // screen points per checker square
            for p in corners {
                m.vertices.push(egui::epaint::Vertex {
                    pos: p,
                    uv: pos2(p.x / (2.0 * cell), p.y / (2.0 * cell)),
                    color: Color32::WHITE,
                });
            }
            m.add_triangle(0, 1, 2);
            m.add_triangle(0, 2, 3);
            painter.add(m);
        }

        let level = view.level(proj.level_count());
        let span = (CHUNK << level) as f32; // canvas px per chunk
        let vis = view
            .visible_canvas(vp, ppp)
            .intersect(Rect::from_min_size(Pos2::ZERO, vec2(w as f32, h as f32)));
        if vis.width() <= 0.0 || vis.height() <= 0.0 {
            self.evict();
            return;
        }
        let (cx0, cy0) = (
            (vis.left() / span).floor() as u32,
            (vis.top() / span).floor() as u32,
        );
        let (cx1, cy1) = (
            (vis.right() / span).ceil() as u32,
            (vis.bottom() / span).ceil() as u32,
        );
        for cy in cy0..cy1 {
            for cx in cx0..cx1 {
                let tex = self.texture(&ctx, proj, level, cx, cy);
                let x0 = cx as f32 * span;
                let y0 = cy as f32 * span;
                let x1 = (x0 + span).min(w as f32);
                let y1 = (y0 + span).min(h as f32);
                let uv = vec2((x1 - x0) / span, (y1 - y0) / span);
                let mut m = Mesh::with_texture(tex);
                for (c, t) in [
                    (pos2(x0, y0), pos2(0.0, 0.0)),
                    (pos2(x1, y0), pos2(uv.x, 0.0)),
                    (pos2(x1, y1), pos2(uv.x, uv.y)),
                    (pos2(x0, y1), pos2(0.0, uv.y)),
                ] {
                    m.vertices.push(egui::epaint::Vertex {
                        pos: view.to_screen(vp, ppp, c),
                        uv: t,
                        color: Color32::WHITE,
                    });
                }
                m.add_triangle(0, 1, 2);
                m.add_triangle(0, 2, 3);
                painter.add(m);
            }
        }
        self.evict();
    }

    fn evict(&mut self) {
        if self.chunks.len() <= MAX_TEXTURES {
            return;
        }
        let mut ages: Vec<(u64, Key)> =
            self.chunks.iter().map(|(k, c)| (c.last_used, *k)).collect();
        ages.sort_unstable();
        let excess = self.chunks.len() - MAX_TEXTURES;
        for (used, k) in ages.into_iter().take(excess) {
            if used < self.frame {
                self.chunks.remove(&k);
            }
        }
    }
}

/// Level-0 tiles intersecting the viewport.
pub fn visible_tiles(view: &View, vp: Rect, ppp: f32, w: u32, h: u32) -> TileRect {
    let r = view.visible_canvas(vp, ppp);
    let cols = w.div_ceil(TILE as u32);
    let rows = h.div_ceil(TILE as u32);
    TileRect::from_px(
        r.left().floor() as i64,
        r.top().floor() as i64,
        r.right().ceil() as i64,
        r.bottom().ceil() as i64,
        cols,
        rows,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_roundtrip() {
        let vp = Rect::from_min_size(pos2(10.0, 20.0), vec2(800.0, 600.0));
        let mut v = View {
            center: vec2(500.0, 300.0),
            zoom: 2.5,
            rotation: 0.7,
            mirror: true,
        };
        for p in [pos2(0.0, 0.0), pos2(123.0, 456.0)] {
            let s = v.to_screen(vp, 2.0, p);
            let back = v.to_canvas(vp, 2.0, s);
            assert!((back - p).length() < 1e-3);
        }
        let anchor = pos2(100.0, 100.0);
        let under = v.to_canvas(vp, 2.0, anchor);
        v.zoom_at(vp, 2.0, anchor, 7.0);
        assert!((v.to_canvas(vp, 2.0, anchor) - under).length() < 1e-2);
    }

    #[test]
    fn mirror_keeps_center() {
        let vp = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let mut v = View {
            center: vec2(50.0, 30.0),
            zoom: 1.0,
            rotation: 0.3,
            mirror: false,
        };
        let p = pos2(80.0, 10.0);
        let s = v.to_screen(vp, 1.0, p);
        v.toggle_mirror();
        let s2 = v.to_screen(vp, 1.0, p);
        // Mirrored about the vertical centre line.
        assert!((s2.x - (800.0 - s.x)).abs() < 1e-3 && (s2.y - s.y).abs() < 1e-3);
    }
}
