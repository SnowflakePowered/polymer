//! Native Snowglass rendering and WebRTC publishing.

pub mod capture;
pub mod cli;
pub mod debug;
pub mod renderer;
pub mod webrtc;

pub use renderer::{
    CompositingPlan, D3d12Compositor, MappedPointer, Presentation, ReceivedSurface, RenderWindow,
    Renderer, SurfacePlacement, SwapchainMode, VideoSwapchain, VulkanCompositor, WebRtcCompositor,
};
pub use snowglass_input::mouse::layout::Extent;

pub fn fit_inside(
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> (u32, u32) {
    if source_width <= target_width && source_height <= target_height {
        return (source_width.max(1), source_height.max(1));
    }
    let scale = (target_width as f64 / source_width.max(1) as f64)
        .min(target_height as f64 / source_height.max(1) as f64);
    (
        (source_width as f64 * scale).floor().max(1.0) as u32,
        (source_height as f64 * scale).floor().max(1.0) as u32,
    )
}

pub fn fit_extent(
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> (i32, i32, i32, i32) {
    let scale = (target_width as f64 / source_width as f64)
        .min(target_height as f64 / source_height as f64);
    let width = (source_width as f64 * scale).round() as i32;
    let height = (source_height as f64 * scale).round() as i32;
    (
        (target_width as i32 - width) / 2,
        (target_height as i32 - height) / 2,
        width,
        height,
    )
}

pub fn surface_sort_key(surface_id: u64) -> (u8, u64) {
    const GRAPH_SURFACE_TAG: u64 = 0x5347_0000_0000_0000;
    const GRAPH_SURFACE_TAG_MASK: u64 = 0xffff_0000_0000_0000;
    if surface_id & GRAPH_SURFACE_TAG_MASK == GRAPH_SURFACE_TAG {
        (0, surface_id & !GRAPH_SURFACE_TAG_MASK)
    } else {
        (1, surface_id)
    }
}

pub const fn vertical_layout_index(index: u32, count: u32, flip_y: bool) -> u32 {
    if flip_y {
        count.saturating_sub(index + 1)
    } else {
        index
    }
}
