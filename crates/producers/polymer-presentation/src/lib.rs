//! Shared presentation, image publication, and swapchain lifecycle.
pub mod frame;
pub mod geometry;

#[cfg(all(feature = "producer", windows))]
pub mod producer;
pub mod registry;
pub mod scheduling;

pub use polymer_presentation_config as configuration;

pub use frame::{CapturedFrame, FramePlacement};
pub use geometry::{Extent2D, Rect};

pub mod swapchain;
