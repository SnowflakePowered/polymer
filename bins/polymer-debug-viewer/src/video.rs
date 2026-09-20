use polymer_renderer::{CompositingPlan, ReceivedSurface, RenderWindow, Renderer};
use std::{error::Error, sync::Arc};
pub enum WebRtcCompositor {
    D3d12(polymer_renderer_d3d12::webrtc::WebRtcCompositor),
    Vulkan(polymer_renderer_vulkan::webrtc::WebRtcCompositor),
}
impl WebRtcCompositor {
    pub fn set_publisher(&mut self, publisher: Arc<polymer_renderer::webrtc::Publisher>) {
        match self {
            Self::D3d12(value) => value.set_publisher(publisher),
            Self::Vulkan(value) => value.set_publisher(publisher),
        }
    }
}
impl Renderer for WebRtcCompositor {
    fn import_surface_offer(
        &mut self,
        offer: &polymer_runtime::SurfaceOffer,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        match self {
            Self::D3d12(value) => Renderer::import_surface_offer(value, offer, quarter_turns),
            Self::Vulkan(value) => Renderer::import_surface_offer(value, offer, quarter_turns),
        }
    }
    fn disconnect_stream(&mut self, connection: u64) -> Result<(), Box<dyn Error>> {
        match self {
            Self::D3d12(value) => Renderer::disconnect_stream(value, connection),
            Self::Vulkan(value) => Renderer::disconnect_stream(value, connection),
        }
    }
    fn set_latest(&mut self, connection: u64, frame: polymer_protocol::FrameReady) {
        match self {
            Self::D3d12(value) => Renderer::set_latest(value, connection, frame),
            Self::Vulkan(value) => Renderer::set_latest(value, connection, frame),
        }
    }
    fn received_surfaces(&self) -> Vec<ReceivedSurface> {
        match self {
            Self::D3d12(value) => Renderer::received_surfaces(value),
            Self::Vulkan(value) => Renderer::received_surfaces(value),
        }
    }
    fn has_pending_frame(&self) -> bool {
        match self {
            Self::D3d12(value) => Renderer::has_pending_frame(value),
            Self::Vulkan(value) => Renderer::has_pending_frame(value),
        }
    }
    fn recreate_swapchain(&mut self, window: &impl RenderWindow) -> Result<bool, Box<dyn Error>> {
        match self {
            Self::D3d12(value) => Renderer::recreate_swapchain(value, window),
            Self::Vulkan(value) => Renderer::recreate_swapchain(value, window),
        }
    }
    fn present(&mut self, plan: &CompositingPlan) -> Result<bool, Box<dyn Error>> {
        match self {
            Self::D3d12(value) => Renderer::present(value, plan),
            Self::Vulkan(value) => Renderer::present(value, plan),
        }
    }
}
