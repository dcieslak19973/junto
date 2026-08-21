//! junto-live — the live session plane.

pub mod doc;
pub mod frame;
pub mod validate;

pub use doc::LiveDoc;
pub use frame::Frame;
pub use validate::validate_annotation_update;
