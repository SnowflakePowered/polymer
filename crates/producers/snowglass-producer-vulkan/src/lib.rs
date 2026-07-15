//! Vulkan swapchain capture and Win32 external-memory export.
//!
//! Unsafe Vulkan calls are contained in this backend instead of leaking into
//! the API-neutral Snowglass crates.

mod environment;
mod image_ring;
mod swapchain;

pub use environment::{
    REQUIRED_DEVICE_EXTENSIONS, VulkanAdapter, VulkanEnvironment, VulkanProbeError,
    format_api_version,
};
pub use image_ring::ExportImageRingDesc;
#[cfg(windows)]
pub use image_ring::{ExportImageRing, ExportImageSlot};
pub use swapchain::{SetDeviceLoaderData, SwapchainCopyEngine};

pub const BACKEND_NAME: &str = "vulkan";
