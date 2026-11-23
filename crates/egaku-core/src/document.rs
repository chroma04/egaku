//! The document: a canvas size, a paper colour and a stack of layers.

use std::sync::Arc;

use crate::blend::{BlendMode, blend_tile};
use crate::tile::{TileCoord, TileGrid};

pub type LayerId = u64;

#[derive(Clone)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    /// 0..=1
    pub opacity: f32,
    pub blend: BlendMode,
    /// Painting only recolours existing pixels; alpha is preserved.
    pub alpha_lock: bool,
    /// Copy-on-write so that undo snapshots and duplicates share pixels.
    pub tiles: Arc<TileGrid>,
}

impl Layer {
    pub fn opacity_u8(&self) -> u8 {
        (self.opacity.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    }

    /// Whether this layer contributes anything to the composite.
    pub fn is_shown(&self) -> bool {
        self.visible && self.opacity_u8() > 0
    }

    /// True when everything except pixel content matches.
    pub fn same_props(&self, o: &Layer) -> bool {
        self.id == o.id
            && self.visible == o.visible
            && self.opacity_u8() == o.opacity_u8()
            && self.blend == o.blend
            && self.alpha_lock == o.alpha_lock
    }

    pub fn tiles_mut(&mut self) -> &mut TileGrid {
        Arc::make_mut(&mut self.tiles)
    }
}

/// The complete, undoable state of a document.
#[derive(Clone)]
pub struct DocState {
    /// Bottom to top.
    pub layers: Vec<Layer>,
    pub active: usize,
    /// Opaque background colour, or `None` for a transparent canvas.
    pub paper: Option<[u8; 3]>,
}

#[derive(Clone)]
pub struct Document {
    pub width: u32,
    pub height: u32,
    pub state: DocState,
    next_id: LayerId,
}

/// Largest supported canvas edge, in pixels.
pub const MAX_CANVAS: u32 = 32768;

impl Document {
    /// Creates a document with a single empty layer.
    pub fn new(width: u32, height: u32, paper: Option<[u8; 3]>) -> Self {
        let mut d = Document::empty(width, height, paper);
        let l = d.new_layer("Layer 1");
        d.state.layers.push(l);
        d
    }

    /// Creates a document with no layers at all (for loaders).
    pub fn empty(width: u32, height: u32, paper: Option<[u8; 3]>) -> Self {
        assert!(width > 0 && height > 0 && width <= MAX_CANVAS && height <= MAX_CANVAS);
        Document {
            width,
            height,
            state: DocState {
                layers: vec![],
                active: 0,
                paper,
            },
            next_id: 1,
        }
    }

    pub fn layers(&self) -> &[Layer] {
        &self.state.layers
    }

    pub fn active(&self) -> usize {
        self.state.active
    }

    pub fn active_layer(&self) -> &Layer {
        &self.state.layers[self.state.active]
    }

    pub fn active_layer_mut(&mut self) -> &mut Layer {
        &mut self.state.layers[self.state.active]
    }

    /// Makes a new, empty layer (not yet inserted).
    pub fn new_layer(&mut self, name: impl Into<String>) -> Layer {
        let id = self.next_id;
        self.next_id += 1;
        Layer {
            id,
            name: name.into(),
            visible: true,
            opacity: 1.0,
            blend: BlendMode::Normal,
            alpha_lock: false,
            tiles: Arc::new(TileGrid::new(self.width, self.height)),
        }
    }

    pub fn fresh_id(&mut self) -> LayerId {
        self.next_id += 1;
        self.next_id - 1
    }

    /// A name like "Layer 7" that isn't taken yet.
    pub fn next_layer_name(&self) -> String {
        let n = self
            .state
            .layers
            .iter()
            .filter_map(|l| l.name.strip_prefix("Layer ")?.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        format!("Layer {}", n.max(self.state.layers.len() as u32) + 1)
    }

    /// Composites the whole document into one tile.
    pub fn compose_tile(&self, c: TileCoord, out: &mut crate::tile::Tile) {
        out.px.fill(self.paper_px());
        for l in &self.state.layers {
            if l.is_shown()
                && let Some(t) = l.tiles.get(c)
            {
                blend_tile(out, t, l.opacity_u8(), l.blend);
            }
        }
    }

    pub fn paper_px(&self) -> crate::tile::Px {
        match self.state.paper {
            Some([r, g, b]) => [r, g, b, 255],
            None => [0; 4],
        }
    }

    pub fn grid_dims(&self) -> (u32, u32) {
        (
            crate::tile::tiles_for(self.width),
            crate::tile::tiles_for(self.height),
        )
    }

    /// Approximate memory held by layer pixels (shared tiles counted once per
    /// layer that references them).
    pub fn pixel_bytes(&self) -> usize {
        self.state
            .layers
            .iter()
            .map(|l| l.tiles.tile_count() * crate::tile::TILE_BYTES)
            .sum()
    }
}
