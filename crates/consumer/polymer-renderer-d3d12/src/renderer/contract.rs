use super::*;
use polymer_protocol::FrameReady;
use polymer_renderer::{CompositingPlan, ReceivedSurface, RenderWindow, Renderer};
use polymer_runtime::SurfaceOffer;
use std::error::Error;
#[cfg(windows)]
impl Renderer for D3d12Compositor {
    fn import_surface_offer(
        &mut self,
        offer: &SurfaceOffer,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        D3d12Compositor::import_stream(self, offer.connection_id(), offer.stream(), quarter_turns)?;
        Ok(())
    }

    fn disconnect_stream(&mut self, connection: u64) -> Result<(), Box<dyn Error>> {
        D3d12Compositor::disconnect_stream(self, connection)
    }

    fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        D3d12Compositor::set_latest(self, connection, frame);
    }

    fn received_surfaces(&self) -> Vec<ReceivedSurface> {
        D3d12Compositor::received_surfaces(self)
    }

    fn has_pending_frame(&self) -> bool {
        D3d12Compositor::has_pending_frame(self)
    }

    fn recreate_swapchain(&mut self, window: &impl RenderWindow) -> Result<bool, Box<dyn Error>> {
        self.resize(window)
    }

    fn begin_frame(&mut self) {
        D3d12Compositor::begin_frame(self);
    }

    fn present(&mut self, plan: &CompositingPlan) -> Result<bool, Box<dyn Error>> {
        D3d12Compositor::present(self, plan)
    }
}
