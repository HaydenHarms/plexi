//! Plexi's document model, ink geometry, `.plexi` file format and exporters.
//!
//! Nothing here depends on the UI, so it can be tested headless and reused by the
//! command-line exporter.

pub mod cutout;
pub mod export;
pub mod fonts;
pub mod format;
pub mod geom;
pub mod gif;
pub mod handwriting;
pub mod ink;
pub mod model;
pub mod pdf_import;

pub use geom::Rect;
pub use model::*;
