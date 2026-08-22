//! D3D11 swapchain capture and shared NT-handle texture export.
#![allow(unsafe_code)]

mod capture;
mod image_ring;
mod swapchain;
mod tracking;

pub use capture::{SwapchainCapture, claim};
pub use image_ring::{D3d11ImageRing, D3d11ImageRingDesc, D3d11ImageSlot, D3d11RingMode};
pub use swapchain::{D3d11CaptureRegion, D3d11CopySubmission, D3d11SwapchainCapture};
pub use tracking::{Presentation, TrackedResource, Tracking};

pub const BACKEND_NAME: &str = "d3d11";
