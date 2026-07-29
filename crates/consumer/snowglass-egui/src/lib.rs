//! Egui GPU painters and widgets owned by Snowglass viewer frontends.

pub mod d3d12;
pub mod debug;
pub mod vulkan;

pub use d3d12::D3d12EguiPainter;
pub use debug::{
    DebugOverlayModel, InputAxisDiagnostics, InputDiagnostics, InputSlotDiagnostics,
    draw_debug_overlay, draw_input_debug_window, draw_statistics_graph,
};
pub use vulkan::VulkanEguiPainter;
