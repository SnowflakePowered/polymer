//! Metal rendering backend.
#![cfg(target_os = "macos")]
pub mod egui;
pub mod renderer;
pub mod webrtc;
pub use egui::MetalEguiPainter;
pub use renderer::{MetalCompositor, MetalFrame, MetalRenderPass, MetalVertex};
