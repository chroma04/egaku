//! Undo history as a list of whole-document snapshots.
//!
//! Because layers share tiles copy-on-write, a snapshot is just a vector of
//! layer handles: taking one costs nothing, and holding one costs only the
//! tiles that were later overwritten. This "memento" design means every
//! operation is undoable by construction, with no per-command inverse code to
//! get wrong. History is bounded by an approximate memory budget instead of a
//! fixed step count, so small edits keep a long history and huge ones don't
//! exhaust memory.

use crate::document::{DocState, Layer};
use crate::tile::{TILE_BYTES, TileCoord};

pub struct Snapshot {
    pub state: DocState,
    pub label: String,
    /// Identifies the document state this snapshot represents.
    pub state_id: u64,
    cost: usize,
}

pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    budget: usize,
    max_steps: usize,
}

impl Default for History {
    fn default() -> Self {
        History::new(1536 << 20, 1000)
    }
}

/// What changed between two document states, for display invalidation.
pub enum Changes {
    All,
    Tiles(Vec<TileCoord>),
}

/// Every layer in preorder with its parent id.
fn flat(s: &DocState) -> Vec<(&Layer, Option<u64>)> {
    fn go<'a>(ls: &'a [Layer], parent: Option<u64>, out: &mut Vec<(&'a Layer, Option<u64>)>) {
        for l in ls {
            out.push((l, parent));
            go(l.children(), Some(l.id), out);
        }
    }
    let mut out = vec![];
    go(&s.layers, None, &mut out);
    out
}

/// Compares two states. Unchanged layers share their grids, so this is cheap.
pub fn diff_states(a: &DocState, b: &DocState) -> Changes {
    if a.paper != b.paper || a.size != b.size {
        return Changes::All;
    }
    let (fa, fb) = (flat(a), flat(b));
    if fa.len() != fb.len()
        || fa
            .iter()
            .zip(&fb)
            .any(|((la, pa), (lb, pb))| la.id != lb.id || pa != pb)
    {
        return Changes::All;
    }
    let mut out = vec![];
    for ((la, _), (lb, _)) in fa.iter().zip(&fb) {
        let looks_different = la.blend != lb.blend
            || la.opacity_u8() != lb.opacity_u8()
            || la.visible != lb.visible
            || la.clip != lb.clip;
        if looks_different {
            if la.is_group() {
                return Changes::All;
            }
            // Affects every tile the layer occupies in either state.
            for l in [la, lb] {
                out.extend(l.tiles.iter().map(|(c, _)| c));
            }
            continue;
        }
        if !std::sync::Arc::ptr_eq(&la.tiles, &lb.tiles) {
            la.tiles.diff(&lb.tiles, &mut out);
        }
    }
    out.sort_unstable();
    out.dedup();
    Changes::Tiles(out)
}

/// Approximate memory a snapshot of `before` keeps alive once the document
/// has moved on to `after`.
pub fn snapshot_cost(before: &DocState, after: &DocState) -> usize {
    let mut cost = 256;
    let after_flat = flat(after);
    let by_id: std::collections::HashMap<u64, &Layer> =
        after_flat.iter().map(|(l, _)| (l.id, *l)).collect();
    if let (Some(s), false) = (&before.selection, {
        match (&before.selection, &after.selection) {
            (Some(x), Some(y)) => std::sync::Arc::ptr_eq(x, y),
            (None, None) => true,
            _ => false,
        }
    }) {
        cost += s.coords().count() * crate::tile::TILE_PX;
    }
    for (l, _) in flat(before) {
        cost += 64 + l.name.len();
        match by_id.get(&l.id) {
            Some(a) if std::sync::Arc::ptr_eq(&a.tiles, &l.tiles) => {}
            Some(a) if (a.tiles.cols(), a.tiles.rows()) != (l.tiles.cols(), l.tiles.rows()) => {
                cost += l.tiles.unique_bytes();
            }
            Some(a) => {
                let mut d = vec![];
                l.tiles.diff(&a.tiles, &mut d);
                cost += d.len() * TILE_BYTES + (l.tiles.cols() * l.tiles.rows()) as usize * 8;
            }
            None => cost += unique_layer_bytes(l, after),
        }
        // Cels of other frames that the new state no longer shares.
        for (i, cel) in l.cels.iter().enumerate() {
            let Some(cel) = cel else { continue };
            let shared = by_id
                .get(&l.id)
                .and_then(|a| a.cels.get(i).cloned().flatten())
                .is_some_and(|x| std::sync::Arc::ptr_eq(&x, cel));
            if !shared {
                cost += cel.unique_bytes();
            }
        }
    }
    cost
}

fn unique_layer_bytes(l: &Layer, after: &DocState) -> usize {
    // A deleted layer's tiles may still be shared (e.g. after a merge).
    let af = flat(after);
    let shared = |c: TileCoord| {
        af.iter()
            .any(|(a, _)| match (a.tiles.get(c), l.tiles.get(c)) {
                (Some(x), Some(y)) => std::sync::Arc::ptr_eq(x, y),
                _ => false,
            })
    };
    l.tiles.iter().filter(|(c, _)| !shared(*c)).count() * TILE_BYTES
}

impl History {
    pub fn new(budget: usize, max_steps: usize) -> Self {
        History {
            undo: vec![],
            redo: vec![],
            budget,
            max_steps,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    pub fn len(&self) -> (usize, usize) {
        (self.undo.len(), self.redo.len())
    }

    pub fn memory(&self) -> usize {
        self.undo.iter().chain(&self.redo).map(|s| s.cost).sum()
    }

    /// Records the state before an edit. `cost` is from [`snapshot_cost`].
    pub fn push(&mut self, state: DocState, label: impl Into<String>, state_id: u64, cost: usize) {
        self.redo.clear();
        self.undo.push(Snapshot {
            state,
            label: label.into(),
            state_id,
            cost,
        });
        self.trim();
    }

    fn trim(&mut self) {
        let mut used = self.memory();
        let mut drop = 0;
        while self.undo.len() - drop > 1
            && (used > self.budget || self.undo.len() - drop > self.max_steps)
        {
            used -= self.undo[drop].cost;
            drop += 1;
        }
        self.undo.drain(..drop);
    }

    /// Swaps `current` for the previous state; returns it, or gives `current`
    /// back unchanged if there is nothing to undo.
    pub fn undo(&mut self, current: DocState, current_id: u64) -> Result<Snapshot, DocState> {
        let Some(s) = self.undo.pop() else {
            return Err(current);
        };
        self.redo.push(Snapshot {
            state: current,
            label: s.label.clone(),
            state_id: current_id,
            cost: s.cost,
        });
        Ok(s)
    }

    pub fn redo(&mut self, current: DocState, current_id: u64) -> Result<Snapshot, DocState> {
        let Some(s) = self.redo.pop() else {
            return Err(current);
        };
        self.undo.push(Snapshot {
            state: current,
            label: s.label.clone(),
            state_id: current_id,
            cost: s.cost,
        });
        Ok(s)
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}
