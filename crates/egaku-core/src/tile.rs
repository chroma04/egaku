//! Sparse, copy-on-write tile storage.
//!
//! Every raster in egaku is a grid of 64×64 tiles. A tile that has never been
//! painted is simply absent, so an empty layer on a 16k canvas costs a few
//! hundred kilobytes of pointers rather than a gigabyte of zeros. Tiles are
//! reference counted: duplicating a layer, or keeping a step of undo history,
//! shares tiles until one side writes to them (`Arc::make_mut`).

use std::sync::Arc;

/// Edge length of a tile in pixels.
pub const TILE: usize = 64;
/// Pixels per tile.
pub const TILE_PX: usize = TILE * TILE;
/// Bytes of memory used by one colour tile.
pub const TILE_BYTES: usize = TILE_PX * 4;

/// A premultiplied-alpha, sRGB-encoded RGBA pixel.
pub type Px = [u8; 4];

/// A 64×64 block of premultiplied RGBA pixels.
#[derive(Clone)]
pub struct Tile {
    pub px: [Px; TILE_PX],
}

impl Tile {
    pub fn new_boxed() -> Box<Tile> {
        Box::new(Tile {
            px: [[0; 4]; TILE_PX],
        })
    }

    pub fn filled(p: Px) -> Tile {
        Tile { px: [p; TILE_PX] }
    }

    /// True when every pixel is fully transparent.
    pub fn is_empty(&self) -> bool {
        // Premultiplied: alpha 0 implies the colour channels are 0 too, so we
        // can test whole words at once.
        self.px.iter().all(|p| u32::from_ne_bytes(*p) == 0)
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> Px {
        self.px[y * TILE + x]
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.px.as_flattened()
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        self.px.as_flattened_mut()
    }
}

/// Integer tile coordinate.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileCoord {
    pub x: u32,
    pub y: u32,
}

impl TileCoord {
    pub const fn new(x: u32, y: u32) -> Self {
        TileCoord { x, y }
    }
}

/// An inclusive-exclusive rectangle of tiles.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TileRect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl TileRect {
    pub fn contains(&self, c: TileCoord) -> bool {
        c.x >= self.x0 && c.x < self.x1 && c.y >= self.y0 && c.y < self.y1
    }

    pub fn union(self, o: TileRect) -> TileRect {
        TileRect {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    pub fn iter(self) -> impl Iterator<Item = TileCoord> {
        (self.y0..self.y1).flat_map(move |y| (self.x0..self.x1).map(move |x| TileCoord::new(x, y)))
    }

    /// Tiles covering the pixel rectangle `[x0, x1) × [y0, y1)`, clipped to
    /// a grid of `cols × rows`.
    pub fn from_px(x0: i64, y0: i64, x1: i64, y1: i64, cols: u32, rows: u32) -> TileRect {
        let t = TILE as i64;
        let c = |v: i64, max: u32| v.clamp(0, max as i64) as u32;
        TileRect {
            x0: c(x0.div_euclid(t), cols),
            y0: c(y0.div_euclid(t), rows),
            x1: c((x1 + t - 1).div_euclid(t), cols),
            y1: c((y1 + t - 1).div_euclid(t), rows),
        }
    }
}

/// Number of tiles needed to span `px` pixels.
pub fn tiles_for(px: u32) -> u32 {
    px.div_ceil(TILE as u32)
}

/// A dense grid of optional, shared tiles.
#[derive(Clone)]
pub struct TileGrid {
    cols: u32,
    rows: u32,
    tiles: Vec<Option<Arc<Tile>>>,
}

impl TileGrid {
    pub fn new(width: u32, height: u32) -> Self {
        let cols = tiles_for(width);
        let rows = tiles_for(height);
        TileGrid {
            cols,
            rows,
            tiles: vec![None; (cols * rows) as usize],
        }
    }

    pub fn cols(&self) -> u32 {
        self.cols
    }

    pub fn rows(&self) -> u32 {
        self.rows
    }

    pub fn full_rect(&self) -> TileRect {
        TileRect {
            x0: 0,
            y0: 0,
            x1: self.cols,
            y1: self.rows,
        }
    }

    #[inline]
    fn idx(&self, c: TileCoord) -> usize {
        debug_assert!(c.x < self.cols && c.y < self.rows);
        (c.y * self.cols + c.x) as usize
    }

    #[inline]
    pub fn get(&self, c: TileCoord) -> Option<&Arc<Tile>> {
        self.tiles[self.idx(c)].as_ref()
    }

    /// Like `get`, but returns `None` for coordinates outside the grid.
    #[inline]
    pub fn get_checked(&self, x: i64, y: i64) -> Option<&Arc<Tile>> {
        if x < 0 || y < 0 || x >= self.cols as i64 || y >= self.rows as i64 {
            None
        } else {
            self.get(TileCoord::new(x as u32, y as u32))
        }
    }

    pub fn set(&mut self, c: TileCoord, t: Option<Arc<Tile>>) {
        let i = self.idx(c);
        self.tiles[i] = t;
    }

    /// Mutable access to a tile, allocating it if absent and un-sharing it if
    /// it is referenced elsewhere (e.g. by undo history).
    pub fn get_mut_or_insert(&mut self, c: TileCoord) -> &mut Tile {
        let i = self.idx(c);
        let slot = self.tiles[i].get_or_insert_with(|| Arc::from(Tile::new_boxed()));
        Arc::make_mut(slot)
    }

    /// Iterates over the tiles that are present.
    pub fn iter(&self) -> impl Iterator<Item = (TileCoord, &Arc<Tile>)> {
        let cols = self.cols;
        self.tiles.iter().enumerate().filter_map(move |(i, t)| {
            t.as_ref()
                .map(|t| (TileCoord::new(i as u32 % cols, i as u32 / cols), t))
        })
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.iter().filter(|t| t.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.iter().all(|t| t.is_none())
    }

    /// Bounding rectangle of present tiles.
    pub fn bounds(&self) -> Option<TileRect> {
        let mut r: Option<TileRect> = None;
        for (c, _) in self.iter() {
            let t = TileRect {
                x0: c.x,
                y0: c.y,
                x1: c.x + 1,
                y1: c.y + 1,
            };
            r = Some(r.map_or(t, |r| r.union(t)));
        }
        r
    }

    /// Coordinates where `self` and `other` hold different tiles. Shared tiles
    /// compare by pointer, which makes diffing two undo states nearly free.
    pub fn diff(&self, other: &TileGrid, out: &mut Vec<TileCoord>) {
        debug_assert_eq!((self.cols, self.rows), (other.cols, other.rows));
        for (i, (a, b)) in self.tiles.iter().zip(&other.tiles).enumerate() {
            let same = match (a, b) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            };
            if !same {
                out.push(TileCoord::new(i as u32 % self.cols, i as u32 / self.cols));
            }
        }
    }

    /// Drops tiles that became fully transparent.
    pub fn prune(&mut self, coords: impl IntoIterator<Item = TileCoord>) {
        for c in coords {
            let i = self.idx(c);
            if self.tiles[i].as_ref().is_some_and(|t| t.is_empty()) {
                self.tiles[i] = None;
            }
        }
    }

    /// Reads one pixel (transparent outside the grid).
    pub fn pixel(&self, x: i64, y: i64) -> Px {
        let t = TILE as i64;
        match self.get_checked(x.div_euclid(t), y.div_euclid(t)) {
            Some(tile) => tile.get(x.rem_euclid(t) as usize, y.rem_euclid(t) as usize),
            None => [0; 4],
        }
    }

    /// Builds the tile at `c` as it would appear if the whole grid were
    /// translated by `(dx, dy)` pixels. Returns `None` if it would be empty.
    pub fn shifted_tile(&self, c: TileCoord, dx: i64, dy: i64) -> Option<Tile> {
        let t = TILE as i64;
        if dx.rem_euclid(t) == 0 && dy.rem_euclid(t) == 0 {
            return self
                .get_checked(c.x as i64 - dx.div_euclid(t), c.y as i64 - dy.div_euclid(t))
                .map(|a| (**a).clone());
        }
        // Source pixel rectangle.
        let sx = c.x as i64 * t - dx;
        let sy = c.y as i64 * t - dy;
        let (tx0, ty0) = (sx.div_euclid(t), sy.div_euclid(t));
        let (ox, oy) = (sx.rem_euclid(t) as usize, sy.rem_euclid(t) as usize);
        let srcs = [
            self.get_checked(tx0, ty0),
            self.get_checked(tx0 + 1, ty0),
            self.get_checked(tx0, ty0 + 1),
            self.get_checked(tx0 + 1, ty0 + 1),
        ];
        if srcs.iter().all(|s| s.is_none()) {
            return None;
        }
        let mut out = Tile::filled([0; 4]);
        for y in 0..TILE {
            let syy = oy + y;
            let (row_t, ry) = if syy < TILE {
                (0, syy)
            } else {
                (2, syy - TILE)
            };
            // Left part from tile row_t, right part from row_t+1.
            let split = TILE - ox;
            if let Some(src) = srcs[row_t] {
                out.px[y * TILE..y * TILE + split]
                    .copy_from_slice(&src.px[ry * TILE + ox..ry * TILE + TILE]);
            }
            if ox > 0
                && let Some(src) = srcs[row_t + 1]
            {
                out.px[y * TILE + split..y * TILE + TILE]
                    .copy_from_slice(&src.px[ry * TILE..ry * TILE + ox]);
            }
        }
        if out.is_empty() { None } else { Some(out) }
    }

    /// Returns a new grid translated by `(dx, dy)` pixels; content moved
    /// outside the canvas is discarded.
    pub fn translated(&self, dx: i64, dy: i64) -> TileGrid {
        let mut g = TileGrid {
            cols: self.cols,
            rows: self.rows,
            tiles: vec![None; self.tiles.len()],
        };
        for c in self.full_rect().iter() {
            if let Some(t) = self.shifted_tile(c, dx, dy) {
                g.set(c, Some(Arc::new(t)));
            }
        }
        g
    }

    /// Approximate heap memory used by the tiles of this grid that are not
    /// shared with anyone else.
    pub fn unique_bytes(&self) -> usize {
        self.tiles
            .iter()
            .flatten()
            .filter(|t| Arc::strong_count(t) == 1)
            .count()
            * TILE_BYTES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_and_cow() {
        let mut g = TileGrid::new(200, 100);
        assert_eq!((g.cols(), g.rows()), (4, 2));
        assert!(g.is_empty());
        g.get_mut_or_insert(TileCoord::new(1, 1)).px[0] = [255; 4];
        let snapshot = g.clone();
        g.get_mut_or_insert(TileCoord::new(1, 1)).px[1] = [255; 4];
        assert_eq!(snapshot.get(TileCoord::new(1, 1)).unwrap().px[1], [0; 4]);
        let mut d = vec![];
        g.diff(&snapshot, &mut d);
        assert_eq!(d, vec![TileCoord::new(1, 1)]);
    }

    #[test]
    fn shift_roundtrip() {
        let mut g = TileGrid::new(256, 256);
        for y in 0..256 {
            for x in 0..256 {
                let t = g.get_mut_or_insert(TileCoord::new(x / 64, y / 64));
                t.px[(y as usize % 64) * 64 + x as usize % 64] = [x as u8, y as u8, 0, 255];
            }
        }
        let s = g.translated(37, -5);
        assert_eq!(s.pixel(37, 0), [0, 5, 0, 255]);
        assert_eq!(s.pixel(100, 100), [63, 105, 0, 255]);
        assert_eq!(s.pixel(10, 10), [0; 4]);
        let back = s.translated(-37, 5);
        assert_eq!(back.pixel(100, 100), [100, 100, 0, 255]);
    }
}
