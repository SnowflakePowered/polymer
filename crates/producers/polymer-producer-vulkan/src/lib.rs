//! Native Vulkan swapchain presentation and Win32 external-memory sharing.
//!
//! Unsafe Vulkan calls are contained in this backend instead of leaking into
//! the API-neutral Polymer crates.

#[cfg(windows)]
mod d3d12_interop;
#[cfg(windows)]
mod device;
#[cfg(windows)]
mod image_ring;
#[cfg(windows)]
pub mod swapchain;
#[cfg(windows)]
mod timeline;

#[cfg(windows)]
pub use device::{ExternalDevice, SetDeviceLoaderData};
#[cfg(windows)]
pub(crate) use image_ring::{ExportImageRing, ExportImageRingDesc};
#[cfg(windows)]
pub(crate) use timeline::ExportTimelineSemaphore;

pub const BACKEND_NAME: &str = "vulkan";
