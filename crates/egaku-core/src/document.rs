//! The document: a canvas size, a paper colour and a tree of layers.
//!
//! Layers are addressed by a stable [`LayerId`] rather than by position, since
//! groups make positions two-dimensional. Every layer that isn't a group owns
//! a tile grid holding what it looks like: painted pixels for raster layers,
//! and a rendered cache for text and image objects. The compositor therefore
//! never needs to know what kind of layer it is looking at.

use std::sync::Arc;

use crate::blend::BlendMode;
use crate::object::{ImageObject, TextObject};
use crate::selection::Selection;
use crate::tile::{TileCoord, TileGrid};

pub type LayerId = u64;

/// What a layer is, beyond its pixels.
#[derive(Clone)]
pub enum Content {
    /// Freely paintable pixels.
    Raster,
    /// A container. Children are composited in isolation, then blended
    /// onto what is below with the group's opacity and mode.
    Group { children: Vec<Layer>, expanded: bool },
    /// Editable text; the tiles are a rendering of it.
    Text(Arc<TextObject>),
    /// A placed image that can be transformed without losing quality; the
    /// tiles are a resampling of the untouched original.
    Image(Arc<ImageObject>),
}

/// Coarse layer kind, for display and for deciding what an edit means.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Raster,
    Group,
    Text,
    Image,
}

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
    /// Clipped to the alpha of the nearest unclipped layer below it.
    pub clip: bool,
    /// Pixels of the current animation frame. Copy-on-write so that undo
    /// snapshots and duplicates share them.
    pub tiles: Arc<TileGrid>,
    pub content: Content,
    /// Pixels of every animation frame, indexed by frame. The entry for the
    /// current frame is stale: `tiles` is authoritative for it. Only raster
    /// layers animate; for other kinds this is empty.
    pub cels: Vec<Option<Arc<TileGrid>>>,
}

impl Layer {
    pub fn opacity_u8(&self) -> u8 {
        (self.opacity.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    }

    /// Whether this layer contributes anything to the composite.
    pub fn is_shown(&self) -> bool {
        self.visible && self.opacity_u8() > 0
    }

    pub fn kind(&self) -> Kind {
        match self.content {
            Content::Raster => Kind::Raster,
            Content::Group { .. } => Kind::Group,
            Content::Text(_) => Kind::Text,
            Content::Image(_) => Kind::Image,
        }
    }

    pub fn is_group(&self) -> bool {
        matches!(self.content, Content::Group { .. })
    }

    /// Raster layers are the only ones the brush can touch directly.
    pub fn is_paintable(&self) -> bool {
        matches!(self.content, Content::Raster)
    }

    pub fn children(&self) -> &[Layer] {
        match &self.content {
            Content::Group { children, .. } => children,
            _ => &[],
        }
    }

    pub fn children_mut(&mut self) -> Option<&mut Vec<Layer>> {
        match &mut self.content {
            Content::Group { children, .. } => Some(children),
            _ => None,
        }
    }

    /// True when everything except pixel content matches.
    pub fn same_props(&self, o: &Layer) -> bool {
        self.id == o.id
            && self.visible == o.visible
            && self.opacity_u8() == o.opacity_u8()
            && self.blend == o.blend
            && self.alpha_lock == o.alpha_lock
            && self.clip == o.clip
    }

    pub fn tiles_mut(&mut self) -> &mut TileGrid {
        Arc::make_mut(&mut self.tiles)
    }

    /// This layer and all its descendants, depth first, parents first.
    pub fn walk(&self, f: &mut impl FnMut(&Layer)) {
        f(self);
        for c in self.children() {
            c.walk(f);
        }
    }

    pub fn walk_mut(&mut self, f: &mut impl FnMut(&mut Layer)) {
        f(self);
        if let Some(ch) = self.children_mut() {
            for c in ch {
                c.walk_mut(f);
            }
        }
    }
}

/// The complete, undoable state of a document.
#[derive(Clone)]
pub struct DocState {
    /// Root layers, bottom to top.
    pub layers: Vec<Layer>,
    pub active: LayerId,
    /// Opaque background colour, or `None` for a transparent canvas.
    pub paper: Option<[u8; 3]>,
    /// The selected region, or `None` when nothing is selected (which means
    /// edits apply everywhere).
    pub selection: Option<Arc<Selection>>,
    /// Number of animation frames (at least 1).
    pub frames: usize,
    /// The frame being shown and edited.
    pub frame: usize,
    /// Playback speed in frames per second.
    pub fps: u32,
    /// Canvas size; kept here too so undoing a resize restores it.
    pub size: (u32, u32),
}

/// Where in the tree a layer lives.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The containing group, or `None` for the root.
    pub parent: Option<LayerId>,
    /// Index among the siblings, bottom first.
    pub index: usize,
}

#[derive(Clone)]
pub struct Document {
    pub width: u32,
    pub height: u32,
    pub state: DocState,
    next_id: LayerId,
    empty: Arc<TileGrid>,
}

/// Largest supported canvas edge, in pixels.
pub const MAX_CANVAS: u32 = 32768;

impl Document {
    /// Creates a document with a single empty layer.
    pub fn new(width: u32, height: u32, paper: Option<[u8; 3]>) -> Self {
        let mut d = Document::empty(width, height, paper);
        let l = d.new_layer("Layer 1");
        d.state.active = l.id;
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
                selection: None,
                frames: 1,
                frame: 0,
                fps: 12,
                size: (width, height),
            },
            next_id: 1,
            empty: Arc::new(TileGrid::new(width, height)),
        }
    }

    /// Changes the canvas size (layers must be resized separately).
    pub fn set_size(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.state.size = (width, height);
        self.empty = Arc::new(TileGrid::new(width, height));
    }

    /// Brings the size fields in line with the state (after undo).
    pub fn sync_size(&mut self) {
        let (w, h) = self.state.size;
        if (w, h) != (self.width, self.height) {
            self.width = w;
            self.height = h;
            self.empty = Arc::new(TileGrid::new(w, h));
        }
    }

    /// A shared empty grid of the canvas size.
    pub fn empty_grid(&self) -> Arc<TileGrid> {
        self.empty.clone()
    }

    pub fn layers(&self) -> &[Layer] {
        &self.state.layers
    }

    pub fn active_id(&self) -> LayerId {
        self.state.active
    }

    pub fn active_layer(&self) -> &Layer {
        self.find(self.state.active)
            .expect("active layer always exists")
    }

    pub fn active_layer_mut(&mut self) -> &mut Layer {
        let id = self.state.active;
        self.find_mut(id).expect("active layer always exists")
    }

    pub fn find(&self, id: LayerId) -> Option<&Layer> {
        fn go(ls: &[Layer], id: LayerId) -> Option<&Layer> {
            for l in ls {
                if l.id == id {
                    return Some(l);
                }
                if let Some(f) = go(l.children(), id) {
                    return Some(f);
                }
            }
            None
        }
        go(&self.state.layers, id)
    }

    pub fn find_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        fn go(ls: &mut [Layer], id: LayerId) -> Option<&mut Layer> {
            for l in ls {
                if l.id == id {
                    return Some(l);
                }
                if let Some(ch) = l.children_mut()
                    && let Some(f) = go(ch, id)
                {
                    return Some(f);
                }
            }
            None
        }
        go(&mut self.state.layers, id)
    }

    /// Where a layer sits in the tree.
    pub fn place_of(&self, id: LayerId) -> Option<Place> {
        fn go(ls: &[Layer], parent: Option<LayerId>, id: LayerId) -> Option<Place> {
            for (i, l) in ls.iter().enumerate() {
                if l.id == id {
                    return Some(Place { parent, index: i });
                }
                if let Some(p) = go(l.children(), Some(l.id), id) {
                    return Some(p);
                }
            }
            None
        }
        go(&self.state.layers, None, id)
    }

    /// The ids of the groups containing a layer, outermost first.
    pub fn ancestors(&self, id: LayerId) -> Vec<LayerId> {
        let mut out = vec![];
        let mut cur = self.place_of(id).and_then(|p| p.parent);
        while let Some(g) = cur {
            out.push(g);
            cur = self.place_of(g).and_then(|p| p.parent);
        }
        out.reverse();
        out
    }

    /// The sibling list a place refers to.
    pub fn siblings(&self, parent: Option<LayerId>) -> &[Layer] {
        match parent {
            None => &self.state.layers,
            Some(g) => self.find(g).map_or(&[], |l| l.children()),
        }
    }

    pub fn siblings_mut(&mut self, parent: Option<LayerId>) -> Option<&mut Vec<Layer>> {
        match parent {
            None => Some(&mut self.state.layers),
            Some(g) => self.find_mut(g)?.children_mut(),
        }
    }

    /// Removes a layer from the tree and returns it with where it was.
    pub fn take(&mut self, id: LayerId) -> Option<(Layer, Place)> {
        let place = self.place_of(id)?;
        let l = self.siblings_mut(place.parent)?.remove(place.index);
        Some((l, place))
    }

    /// Inserts a layer at a place (the index is clamped).
    pub fn put(&mut self, layer: Layer, place: Place) {
        let sib = match self.siblings_mut(place.parent) {
            Some(s) => s,
            None => &mut self.state.layers,
        };
        let i = place.index.min(sib.len());
        sib.insert(i, layer);
    }

    /// Every layer, depth first with parents before children, bottom first.
    pub fn walk(&self, mut f: impl FnMut(&Layer)) {
        for l in &self.state.layers {
            l.walk(&mut f);
        }
    }

    pub fn walk_mut(&mut self, mut f: impl FnMut(&mut Layer)) {
        for l in &mut self.state.layers {
            l.walk_mut(&mut f);
        }
    }

    /// Number of layers in the whole tree.
    pub fn layer_count(&self) -> usize {
        let mut n = 0;
        self.walk(|_| n += 1);
        n
    }

    /// Whether a layer and all the groups around it are visible.
    pub fn is_effectively_visible(&self, id: LayerId) -> bool {
        self.find(id).is_some_and(|l| l.visible)
            && self
                .ancestors(id)
                .iter()
                .all(|g| self.find(*g).is_some_and(|l| l.visible))
    }

    /// Rows for a layer panel: top to bottom, with depth, skipping the
    /// contents of collapsed groups.
    pub fn display_rows(&self) -> Vec<(LayerId, usize)> {
        fn go(ls: &[Layer], depth: usize, out: &mut Vec<(LayerId, usize)>) {
            for l in ls.iter().rev() {
                out.push((l.id, depth));
                if let Content::Group {
                    children,
                    expanded: true,
                } = &l.content
                {
                    go(children, depth + 1, out);
                }
            }
        }
        let mut out = vec![];
        go(&self.state.layers, 0, &mut out);
        out
    }

    /// Makes a new, empty raster layer (not yet inserted).
    pub fn new_layer(&mut self, name: impl Into<String>) -> Layer {
        let id = self.fresh_id();
        Layer {
            id,
            name: name.into(),
            visible: true,
            opacity: 1.0,
            blend: BlendMode::Normal,
            alpha_lock: false,
            clip: false,
            tiles: self.empty.clone(),
            content: Content::Raster,
            cels: vec![None; self.state.frames],
        }
    }

    /// Makes a new, empty group (not yet inserted).
    pub fn new_group(&mut self, name: impl Into<String>) -> Layer {
        let mut l = self.new_layer(name);
        l.content = Content::Group {
            children: vec![],
            expanded: true,
        };
        l.cels.clear();
        l
    }

    pub fn fresh_id(&mut self) -> LayerId {
        self.next_id += 1;
        self.next_id - 1
    }

    /// Gives a layer (and its descendants) fresh ids, for duplicates.
    pub fn reassign_ids(&mut self, l: &mut Layer) {
        let mut next = self.next_id;
        l.walk_mut(&mut |l| {
            l.id = next;
            next += 1;
        });
        self.next_id = next;
    }

    /// A name like "Layer 7" (or "Group 2") that isn't taken yet.
    pub fn next_name(&self, prefix: &str) -> String {
        let mut n = 0;
        let mut count = 0;
        self.walk(|l| {
            if let Some(v) = l
                .name
                .strip_prefix(prefix)
                .and_then(|s| s.strip_prefix(' '))
                .and_then(|s| s.parse::<u32>().ok())
            {
                n = n.max(v);
            }
            if (prefix == "Layer") == !l.is_group() {
                count += 1;
            }
        });
        format!("{prefix} {}", n.max(count) + 1)
    }

    pub fn next_layer_name(&self) -> String {
        self.next_name("Layer")
    }

    /// Composites the whole document into one tile.
    pub fn compose_tile(&self, c: TileCoord, out: &mut crate::tile::Tile) {
        out.px.fill(self.paper_px());
        crate::render::compose_list(&self.state.layers, c, out, &crate::render::NoOverride);
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
        let mut n = 0;
        self.walk(|l| {
            n += l.tiles.tile_count();
            n += l
                .cels
                .iter()
                .flatten()
                .filter(|g| !Arc::ptr_eq(g, &l.tiles))
                .map(|g| g.tile_count())
                .sum::<usize>();
        });
        n * crate::tile::TILE_BYTES
    }

    // ---- animation ----------------------------------------------------------

    /// Shows a different frame: stores every raster layer's pixels into its
    /// cel for the current frame and loads the cel for `f`.
    pub fn set_frame(&mut self, f: usize) {
        let cur = self.state.frame;
        if f == cur || f >= self.state.frames {
            return;
        }
        let empty = self.empty.clone();
        self.walk_mut(|l| {
            if !l.is_paintable() {
                return;
            }
            l.cels.resize(l.cels.len().max(cur.max(f) + 1), None);
            let t = std::mem::replace(&mut l.tiles, empty.clone());
            l.cels[cur] = if t.is_empty() { None } else { Some(t) };
            l.tiles = l.cels[f].take().unwrap_or_else(|| empty.clone());
        });
        self.state.frame = f;
    }

    /// The pixels of a raster layer at an arbitrary frame.
    pub fn cel(&self, l: &Layer, f: usize) -> Option<Arc<TileGrid>> {
        if f == self.state.frame {
            Some(l.tiles.clone())
        } else {
            l.cels.get(f).cloned().flatten()
        }
    }
}
