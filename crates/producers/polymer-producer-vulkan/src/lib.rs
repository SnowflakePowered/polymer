//! Vulkan swapchain capture and Win32 external-memory export.
//!
//! Unsafe Vulkan calls are contained in this backend instead of leaking into
//! the API-neutral Polymer crates.

#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
mod d3d12_interop;
mod environment;
mod image_ring;
mod swapchain;
#[cfg(windows)]
mod timeline;
pub mod tracking;
#[cfg(windows)]
pub mod virtual_swapchain;

pub use environment::{
    REQUIRED_DEVICE_EXTENSIONS, VulkanAdapter, VulkanEnvironment, VulkanProbeError,
    format_api_version,
};
#[cfg(windows)]
pub use image_ring::{ExportImageRing, ExportImageSlot};
pub use image_ring::{ExportImageRingDesc, ExternalHandleMode};
pub use polymer_presentation::Evidence;
pub use swapchain::{CopySubmission, SetDeviceLoaderData, SwapchainCopyEngine};
#[cfg(windows)]
pub use timeline::ExportTimelineSemaphore;

pub const BACKEND_NAME: &str = "vulkan";
