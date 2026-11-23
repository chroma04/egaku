//! Small previews of layers for the layer panel.

use crate::tile::TileGrid;

/// Renders a premultiplied RGBA thumbnail fitting in `max × max`.
///
/// Each thumbnail pixel averages a grid of point samples sized to its
/// footprint (up to 8×8), so thin strokes still show up while the cost stays
/// bounded by the thumbnail size, never the canvas size.
pub fn thumbnail(grid: &TileGrid, width: u32, height: u32, max: u32) -> (u32, u32, Vec<u8>) {
    let scale = (max as f32 / width as f32).min(max as f32 / height as f32);
    let tw = ((width as f32 * scale).round() as u32).max(1);
    let th = ((height as f32 * scale).round() as u32).max(1);
    let mut out = vec![0u8; (tw * th * 4) as usize];
    let step = 1.0 / scale;
    let k = ((step / 4.0).ceil() as u32).clamp(1, 8);
    let n = k * k;
    for y in 0..th {
        for x in 0..tw {
            let mut acc = [0u32; 4];
            for sy in 0..k {
                for sx in 0..k {
                    let px = (x as f32 + (sx as f32 + 0.5) / k as f32) * step;
                    let py = (y as f32 + (sy as f32 + 0.5) / k as f32) * step;
                    let p = grid.pixel(px as i64, py as i64);
                    for c in 0..4 {
                        acc[c] += p[c] as u32;
                    }
                }
            }
            let o = ((y * tw + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = ((acc[c] + n / 2) / n) as u8;
            }
        }
    }
    (tw, th, out)
}
