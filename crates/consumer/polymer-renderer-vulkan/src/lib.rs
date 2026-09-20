//! VULKAN rendering backend.
#![cfg(windows)]
pub mod egui;
pub mod renderer;
pub use egui::VulkanEguiPainter;
pub use renderer::{VulkanCompositor, VulkanRenderPass};
pub mod webrtc;
