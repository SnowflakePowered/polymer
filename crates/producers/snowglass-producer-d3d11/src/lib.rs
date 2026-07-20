//! D3D11 swapchain capture and shared NT-handle texture export.
#![allow(unsafe_code)]

mod capture;
mod image_ring;
mod swapchain;
mod tracking;

pub use capture::{SwapchainCapture, claim};
pub use image_ring::{D3d11ImageRing, D3d11ImageRingDesc, D3d11ImageSlot, D3d11RingMode};
pub use swapchain::{D3d11CaptureRegion, D3d11CopySubmission, D3d11SwapchainCapture};
pub use tracking::Tracking;

pub(crate) fn d3d11_log(message: impl std::fmt::Display) {
    use std::io::Write;
    eprintln!("{message}");
    let Some(path) = snowglass_hook::config::d3d11_log_file() else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{message}");
    }
}

pub const BACKEND_NAME: &str = "d3d11";
