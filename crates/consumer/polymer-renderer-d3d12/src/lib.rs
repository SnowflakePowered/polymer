//! D3D12 rendering backend.
#![cfg(windows)]
pub mod egui;
pub mod renderer;
pub use egui::D3d12EguiPainter;
pub use renderer::{D3d12Compositor, D3d12RenderPass, RasterSurface};
pub mod webrtc;
