use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::MTLDevice;
use polymer_renderer::{
    CompositingPlan, Extent, Renderer, SurfaceContent, SurfacePlacement, layout::Rect,
};
use polymer_renderer_metal::MetalCompositor;
use polymer_runtime::SurfaceOffer;
use std::{collections::HashMap, error::Error, sync::Arc};

pub(crate) struct Stream {
    pub renderer: MetalCompositor,
    extent: Extent,
    surface: u64,
}
pub(crate) struct Surfaces {
    pub streams: HashMap<u64, Stream>,
    pub device: Retained<ProtocolObject<dyn MTLDevice>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}
impl Surfaces {
    pub fn new(
        device: Retained<ProtocolObject<dyn MTLDevice>>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            device,
            wake,
            streams: HashMap::new(),
        }
    }
    pub fn import(&mut self, offer: &SurfaceOffer, turns: u8) -> Result<(), Box<dyn Error>> {
        let stream = offer.stream();
        let (w, h) = if turns % 2 == 1 {
            (stream.height, stream.width)
        } else {
            (stream.width, stream.height)
        };
        let extent = Extent::new(w.max(1), h.max(1));
        let mut renderer = MetalCompositor::for_layer(self.device.clone(), extent)?;
        renderer.set_wake(self.wake.clone());
        renderer.import_surface_offer(offer, turns)?;
        self.remove(offer.connection_id());
        self.streams.insert(
            offer.connection_id(),
            Stream {
                renderer,
                extent,
                surface: stream.surface_id.0,
            },
        );
        Ok(())
    }
    pub fn clear(&mut self) {
        for connection in self.streams.keys().copied().collect::<Vec<_>>() {
            self.remove(connection);
        }
    }
    pub fn remove(&mut self, connection: u64) {
        if let Some(stream) = self.streams.remove(&connection) {
            stream.renderer.layer().removeFromSuperlayer();
        }
    }
    pub fn present(&mut self, corners: &[(u64, f32)], scale: f64) -> Result<Option<bool>, String> {
        let mut presented = false;
        for (&connection, stream) in &mut self.streams {
            let radius = corners
                .iter()
                .find(|(id, _)| *id == connection)
                .map_or(0., |(_, r)| *r);
            stream
                .renderer
                .layer()
                .setCornerRadius(radius as f64 / scale);
            stream.renderer.layer().setMasksToBounds(radius > 0.);
            if !stream.renderer.has_pending_frame() {
                continue;
            }
            let rect = Rect::new(0, 0, stream.extent.width, stream.extent.height);
            let mut placement = SurfacePlacement::new(connection, stream.surface, rect, 0.);
            placement.content = SurfaceContent::fill(rect);
            presented |= stream
                .renderer
                .present(&CompositingPlan::new([0.; 4], vec![placement]))
                .map_err(|e| e.to_string())?;
        }
        Ok(presented.then_some(true))
    }
}
impl Drop for Surfaces {
    fn drop(&mut self) {
        for stream in self.streams.values() {
            stream.renderer.layer().removeFromSuperlayer();
        }
    }
}
