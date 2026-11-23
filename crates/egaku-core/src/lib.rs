//! egaku-core: everything in egaku that isn't GUI.
//!
//! * [`tile`]: sparse copy-on-write 64×64 tile storage
//! * [`blend`]: layer blend modes
//! * [`brush`]: dab-based brush engine with a 16-bit stroke mask
//! * [`render`]: cached, incrementally updated composite and mip pyramid
//! * [`history`]: snapshot-based undo
//! * [`editor`]: the editing API tying it all together
//! * [`io`]: OpenRaster, PNG and image import/export

pub mod blend;
pub mod brush;
pub mod color;
pub mod document;
pub mod editor;
pub mod fill;
pub mod history;
pub mod io;
pub mod render;
pub mod thumb;
pub mod tile;

pub use document::{Document, Layer};
pub use editor::Editor;
