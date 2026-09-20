use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::MTLDevice;
use polymer_renderer::webrtc::Publisher;
use polymer_renderer::{
    CompositingPlan, Extent, Renderer, SurfaceContent, SurfacePlacement, layout::Rect,
};
use polymer_renderer_metal::{MetalCompositor, webrtc::WebRtcCompositor};
use polymer_runtime::SurfaceOffer;
use std::{collections::HashMap, error::Error};
use std::{collections::HashSet, net::SocketAddr, sync::Arc};

pub(crate) struct Stream {
    pub renderer: WebRtcCompositor,
    extent: Extent,
    surface: u64,
}
pub(crate) struct Surfaces {
    pub streams: HashMap<u64, Stream>,
    pub device: Retained<ProtocolObject<dyn MTLDevice>>,
    publication: Option<(Arc<Publisher>, HashSet<u32>)>,
    targets: HashMap<u64, u32>,
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
            publication: None,
            targets: HashMap::new(),
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
        let mut renderer =
            WebRtcCompositor::new(MetalCompositor::for_layer(self.device.clone(), extent)?);
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
        self.targets.remove(&connection);
        if let Some((publisher, _)) = &self.publication {
            publisher.remove_stream(connection);
        }
        if let Some(stream) = self.streams.remove(&connection) {
            stream.renderer.layer().removeFromSuperlayer();
        }
    }
    pub fn register(&mut self, connection: u64, target: u32) -> Result<(), Box<dyn Error>> {
        let stream = self
            .streams
            .get_mut(&connection)
            .ok_or("surface is not imported")?;
        self.targets.insert(connection, target);
        if let Some((publisher, targets)) = &self.publication {
            if targets.contains(&target) {
                publisher.add_stream(connection, target);
                stream.renderer.set_publisher(Some(publisher.clone()));
            }
        }
        Ok(())
    }
    pub fn start_webrtc(
        &mut self,
        bind: SocketAddr,
        targets: &[u32],
        streams: &[(u64, u32)],
    ) -> Result<(), Box<dyn Error>> {
        let selected = targets.iter().copied().collect::<HashSet<_>>();
        if selected.is_empty() || selected.len() != targets.len() {
            return Err("WebRTC targets must be nonempty and unique".into());
        }
        let publisher = Arc::new(Publisher::start(bind, targets)?);
        for &(connection, target) in streams {
            self.targets.insert(connection, target);
        }
        for (&connection, stream) in &mut self.streams {
            let target = self
                .targets
                .get(&connection)
                .copied()
                .filter(|t| selected.contains(t));
            stream.renderer.set_publisher(target.map(|target| {
                publisher.add_stream(connection, target);
                publisher.clone()
            }));
        }
        self.publication = Some((publisher, selected));
        Ok(())
    }
    pub fn stop_webrtc(&mut self) {
        for stream in self.streams.values_mut() {
            stream.renderer.set_publisher(None);
        }
        self.publication = None;
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
