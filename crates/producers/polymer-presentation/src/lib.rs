//! Shared presentation, image publication, and swapchain lifecycle.
pub mod frame;
pub mod geometry;

#[cfg(feature = "producer")]
pub mod producer;
pub mod registry;
pub mod scheduling;

pub use polymer_protocol::configuration;

pub use frame::{CapturedFrame, FramePlacement};
pub use geometry::{Extent2D, Rect};

pub mod swapchain;
