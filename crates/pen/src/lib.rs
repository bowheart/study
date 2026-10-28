pub mod doc;
pub mod imports;
pub mod instance;
pub mod json;
pub mod layout;
pub mod scene;
pub mod style;
pub mod validate;
pub mod vars;

pub use doc::{Document, Node, NodeId, Props};

/// The schema version study reads and writes.
pub const VERSION: &str = "2.17";

/// Font used when a text node doesn't name one.
pub const DEFAULT_FONT: &str = "Inter";

/// Stroke alignment for shapes and frames that don't specify one.
pub const DEFAULT_STROKE_ALIGN: style::StrokeAlign = style::StrokeAlign::Inner;
