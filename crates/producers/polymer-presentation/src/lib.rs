//! Shared presentation, image publication, and swapchain lifecycle.
pub mod geometry;

#[cfg(feature = "producer")]
pub mod producer;
pub mod registry;
pub mod scheduling;

pub use polymer_protocol::configuration;

pub use geometry::Extent2D;

pub mod swapchain;
