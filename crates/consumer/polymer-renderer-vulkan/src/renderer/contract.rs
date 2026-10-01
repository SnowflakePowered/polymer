use super::*;
use polymer_protocol::FrameReady;
use polymer_renderer::{CompositingPlan, ReceivedSurface, RenderWindow, Renderer};
use polymer_runtime::SurfaceOffer;
use std::error::Error;
#[cfg(windows)]
impl Renderer for VulkanCompositor {
    fn import_surface_offer(
        &mut self,
        offer: &SurfaceOffer,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        VulkanCompositor::import_stream(
            self,
            offer.connection_id(),
            offer.stream(),
            quarter_turns,
        )?;
        Ok(())
    }

    fn disconnect_stream(&mut self, connection: u64) -> Result<(), Box<dyn Error>> {
        VulkanCompositor::disconnect_stream(self, connection).map_err(Into::into)
    }

    fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        VulkanCompositor::set_latest(self, connection, frame);
    }

    fn received_surfaces(&self) -> Vec<ReceivedSurface> {
        VulkanCompositor::received_surfaces(self)
    }

    fn has_pending_frame(&self) -> bool {
        VulkanCompositor::has_pending_frame(self)
    }

    fn recreate_swapchain(&mut self, window: &impl RenderWindow) -> Result<bool, Box<dyn Error>> {
        VulkanCompositor::recreate_swapchain(self, window)
    }

    fn present(&mut self, plan: &CompositingPlan) -> Result<bool, Box<dyn Error>> {
        VulkanCompositor::present(self, plan)
    }
}
