//! Native viewer support for capture integration tests.
mod opaque;
mod wsi;
pub use opaque::OpaqueViewer;
pub use opaque::run_child;
pub use wsi::WsiViewer;
