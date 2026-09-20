use super::WebRtcCompositor;
use crate::D3d12Compositor;
use polymer_protocol::AdapterLuid;
use polymer_renderer::{
    CompositingPlan, Renderer,
    webrtc::{Publisher, Stats},
};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    net::SocketAddr,
};
/// Complete native WebRTC data plane for one D3D12 presentation domain.
///
/// Hosts control its lifetime, but stream registration, GPU video
/// composition, frame draining, encoding, and publication stay together so a
/// host cannot accidentally start signaling without feeding the negotiated
/// tracks.
pub struct Publication {
    publisher: std::sync::Arc<Publisher>,
    compositor: WebRtcCompositor,
    targets: HashSet<u32>,
    connections: HashMap<u64, u32>,
}

impl Publication {
    pub fn start_d3d12(
        bind: SocketAddr,
        targets: &[u32],
        compositor: &D3d12Compositor,
    ) -> Result<Self, Box<dyn Error>> {
        if targets.is_empty() {
            return Err("WebRTC publication requires at least one render target".into());
        }
        let requested = targets.to_vec();
        let targets = requested.iter().copied().collect::<HashSet<_>>();
        if requested.len() != targets.len() {
            return Err("WebRTC render-target indices must be unique".into());
        }
        let publisher = std::sync::Arc::new(Publisher::start(bind, &requested)?);
        let mut compositor = WebRtcCompositor::new_d3d12(compositor)?;
        compositor.set_publisher(publisher.clone());
        Ok(Self {
            publisher,
            compositor,
            targets,
            connections: HashMap::new(),
        })
    }

    pub fn start_d3d12_device(
        bind: SocketAddr,
        targets: &[u32],
        adapter: AdapterLuid,
        device: &windows::Win32::Graphics::Direct3D12::ID3D12Device,
    ) -> Result<Self, Box<dyn Error>> {
        if targets.is_empty() {
            return Err("WebRTC publication requires at least one render target".into());
        }
        let requested = targets.to_vec();
        let targets = requested.iter().copied().collect::<HashSet<_>>();
        if requested.len() != targets.len() {
            return Err("WebRTC render-target indices must be unique".into());
        }
        let publisher = std::sync::Arc::new(Publisher::start(bind, &requested)?);
        let mut compositor = WebRtcCompositor::new_d3d12_device(adapter, device)?;
        compositor.set_publisher(publisher.clone());
        Ok(Self {
            publisher,
            compositor,
            targets,
            connections: HashMap::new(),
        })
    }

    pub fn targets(&self) -> &HashSet<u32> {
        &self.targets
    }

    pub fn add_stream(
        &mut self,
        connection: u64,
        target_index: u32,
    ) -> Result<bool, Box<dyn Error>> {
        if !self.targets.contains(&target_index) {
            return Ok(false);
        }
        self.compositor.import_d3d12_stream(connection)?;
        self.publisher.add_stream(connection, target_index);
        self.connections.insert(connection, target_index);
        Ok(true)
    }

    pub fn remove_stream(&mut self, connection: u64) {
        self.connections.remove(&connection);
        self.compositor.disconnect_stream(connection);
        self.publisher.remove_stream(connection);
    }

    /// Rebuilds only the GPU half after a presentation-adapter change. The
    /// signaling server, peer sessions, and negotiated tracks remain alive.
    pub fn reattach_d3d12(&mut self, compositor: &D3d12Compositor) -> Result<(), Box<dyn Error>> {
        let mut replacement = WebRtcCompositor::new_d3d12(compositor)?;
        for &connection in self.connections.keys() {
            replacement.import_d3d12_stream(connection)?;
        }
        replacement.set_publisher(self.publisher.clone());
        self.compositor = replacement;
        Ok(())
    }

    pub fn reattach_d3d12_device(
        &mut self,
        adapter: AdapterLuid,
        device: &windows::Win32::Graphics::Direct3D12::ID3D12Device,
    ) -> Result<(), Box<dyn Error>> {
        let mut replacement = WebRtcCompositor::new_d3d12_device(adapter, device)?;
        for &connection in self.connections.keys() {
            replacement.import_d3d12_stream(connection)?;
        }
        replacement.set_publisher(self.publisher.clone());
        self.compositor = replacement;
        Ok(())
    }

    pub fn present_d3d12(
        &mut self,
        compositor: &mut D3d12Compositor,
        plan: &CompositingPlan,
    ) -> Result<bool, Box<dyn Error>> {
        let result = compositor.present_with_webrtc(plan, Some(&mut self.compositor));
        Renderer::present(&mut self.compositor, plan)?;
        result
    }

    pub fn stats(&self) -> Stats {
        self.publisher.stats()
    }
}
