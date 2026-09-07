//! Native D3D11 swapchain presentation and shared image allocation.
#![allow(unsafe_code)]

mod image_ring;
pub mod swapchain;

pub use image_ring::{D3d11ImageRing, D3d11ImageRingDesc, D3d11ImageSlot};

pub const BACKEND_NAME: &str = "d3d11";
