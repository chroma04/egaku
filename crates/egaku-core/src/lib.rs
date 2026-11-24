//! egaku-core: everything in egaku that isn't GUI.
//!
//! * [`tile`]: sparse copy-on-write 64×64 tile storage
//! * [`blend`]: layer blend modes
//! * [`brush`]: dab-based brush engine with a 16-bit stroke mask
//! * [`tip`]: brush tips made from images
//! * [`render`]: cached, incrementally updated composite and mip pyramid
//! * [`history`]: snapshot-based undo
//! * [`editor`]: the editing API tying it all together
//! * [`selection`]: sparse selection masks
//! * [`transform`]: affine resampling with a mip pyramid
//! * [`object`]: text and placed-image layers
//! * [`adjust`]: adjustments and filters
//! * [`io`]: OpenRaster, PNG and image import/export

pub mod adjust;
pub mod blend;
pub mod brush;
pub mod color;
pub mod document;
pub mod editor;
pub mod fill;
pub mod history;
pub mod io;
pub mod object;
pub mod render;
pub mod selection;
pub mod thumb;
pub mod tile;
pub mod tip;
pub mod transform;

pub use document::{Document, Layer};
pub use editor::Editor;
