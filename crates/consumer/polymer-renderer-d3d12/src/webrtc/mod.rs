//! Hardware WebRTC output for this backend.
//!
//! The companion window compositor prepares GPU frame copies during its existing
//! composition pass; [`Renderer::present`] submits them to the shared publisher.
//! Encoding sessions and native resource leases remain owned by this backend.

mod encoder;
mod frame;
use frame::*;
use parking_lot::Mutex;
use polymer_renderer::webrtc::{FRAME_CHANNEL_CAPACITY, Publisher, VideoSwapchain};
use polymer_renderer::{CompositingPlan, ReceivedSurface, RenderWindow, Renderer};
use std::{collections::HashMap, error::Error, sync::Arc};
pub mod publication;
use crate::D3d12Compositor;
use polymer_protocol::AdapterLuid;
pub use publication::Publication;
use windows::Win32::Graphics::{
    Direct3D12::{
        D3D12_RESOURCE_STATES, ID3D12CommandQueue, ID3D12Device, ID3D12Fence, ID3D12Resource,
    },
    Dxgi::Common::DXGI_FORMAT,
};
pub struct WebRtcCompositor {
    swapchain: VideoSwapchain,
    publisher: Option<Arc<Publisher>>,
    surfaces: HashMap<
        u64,
        (
            polymer_ipc::StreamReady,
            u8,
            Option<polymer_protocol::FrameReady>,
        ),
    >,
    d3d12: Option<D3d12WebRtcState>,
}

struct D3d12WebRtcState {
    adapter: AdapterLuid,
    completion_fence: ID3D12Fence,
    completion_value: u64,
    encoders: HashMap<u64, Arc<Mutex<encoder::Session>>>,
    stream_completion: HashMap<u64, u64>,
}

impl WebRtcCompositor {
    pub fn new_d3d12(compositor: &D3d12Compositor) -> windows::core::Result<Self> {
        Self::new_d3d12_device(compositor.adapter_luid(), compositor.d3d12_device())
    }

    pub fn new_d3d12_device(
        adapter: AdapterLuid,
        device: &ID3D12Device,
    ) -> windows::core::Result<Self> {
        let completion_fence = unsafe {
            device.CreateFence(
                0,
                windows::Win32::Graphics::Direct3D12::D3D12_FENCE_FLAG_NONE,
            )?
        };
        Ok(Self {
            swapchain: VideoSwapchain::new(FRAME_CHANNEL_CAPACITY),
            publisher: None,
            surfaces: HashMap::new(),
            d3d12: Some(D3d12WebRtcState {
                adapter,
                completion_fence,
                completion_value: 0,
                encoders: HashMap::new(),
                stream_completion: HashMap::new(),
            }),
        })
    }

    pub fn available(&self) -> usize {
        self.swapchain.available()
    }

    pub(crate) fn d3d12_matches(&self, adapter: AdapterLuid) -> bool {
        self.d3d12
            .as_ref()
            .is_some_and(|state| state.adapter == adapter)
    }

    pub fn import_d3d12_stream(&mut self, connection: u64) -> Result<(), &'static str> {
        let Some(d3d12) = &mut self.d3d12 else {
            return Err("WebRTC compositor is not attached to D3D12");
        };
        d3d12.stream_completion.entry(connection).or_insert(0);
        Ok(())
    }

    pub(crate) fn accepts_d3d12_stream(&self, connection: u64) -> bool {
        self.d3d12
            .as_ref()
            .is_some_and(|d3d12| d3d12.stream_completion.contains_key(&connection))
    }

    pub(crate) fn d3d12_ready(&self) -> bool {
        self.d3d12.as_ref().is_some_and(|state| unsafe {
            state.completion_fence.GetCompletedValue() >= state.completion_value
        })
    }

    pub(crate) fn wait_d3d12_stream(
        &self,
        queue: &ID3D12CommandQueue,
        connection: u64,
    ) -> windows::core::Result<()> {
        let Some(state) = &self.d3d12 else {
            return Ok(());
        };
        let value = state
            .stream_completion
            .get(&connection)
            .copied()
            .unwrap_or_default();
        if value != 0 {
            unsafe { queue.Wait(&state.completion_fence, value) }
        } else {
            Ok(())
        }
    }

    #[expect(clippy::too_many_arguments)]
    pub(crate) fn present_d3d12(
        &mut self,
        connection: u64,
        device: ID3D12Device,
        resource: ID3D12Resource,
        state: D3D12_RESOURCE_STATES,
        format: DXGI_FORMAT,
        width: u32,
        height: u32,
        ready_fence: ID3D12Fence,
        ready_value: u64,
    ) {
        let Some(d3d12) = &mut self.d3d12 else {
            return;
        };
        if !d3d12.stream_completion.contains_key(&connection) {
            return;
        }
        d3d12.completion_value += 1;
        let completion_value = d3d12.completion_value;
        d3d12.stream_completion.insert(connection, completion_value);
        let session = d3d12.encoders.entry(connection).or_default().clone();
        let _ = self.swapchain.present(encoder::Session::frame(
            session,
            connection,
            width,
            height,
            D3d12Frame {
                device,
                resource,
                state,
                format,
                ready_fence,
                ready_value,
                completion_fence: d3d12.completion_fence.clone(),
                completion_value,
                completion_submitted: false,
            },
        ));
    }

    pub fn disconnect_stream(&mut self, connection: u64) {
        self.swapchain.disconnect_stream(connection);
        self.surfaces.remove(&connection);
        if let Some(publisher) = &self.publisher {
            publisher.remove_stream(connection);
        }
        if let Some(d3d12) = &mut self.d3d12 {
            d3d12.stream_completion.remove(&connection);
            d3d12.encoders.remove(&connection);
        }
    }
}

impl WebRtcCompositor {
    pub fn set_publisher(&mut self, publisher: Arc<Publisher>) {
        self.publisher = Some(publisher);
    }
}
impl Renderer for WebRtcCompositor {
    fn import_surface_offer(
        &mut self,
        offer: &polymer_runtime::SurfaceOffer,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        self.import_d3d12_stream(offer.connection_id())?;
        self.surfaces.insert(
            offer.connection_id(),
            (*offer.stream(), quarter_turns, None),
        );
        Ok(())
    }
    fn disconnect_stream(&mut self, connection: u64) -> Result<(), Box<dyn Error>> {
        WebRtcCompositor::disconnect_stream(self, connection);
        Ok(())
    }
    fn set_latest(&mut self, connection: u64, frame: polymer_protocol::FrameReady) {
        if let Some((_, _, latest)) = self.surfaces.get_mut(&connection) {
            *latest = Some(frame);
        }
    }
    fn received_surfaces(&self) -> Vec<ReceivedSurface> {
        self.surfaces
            .iter()
            .filter_map(|(&connection, (stream, turns, latest))| {
                let frame = (*latest)?;
                Some(ReceivedSurface {
                    connection,
                    surface_id: stream.surface_id.0,
                    target_window: stream.source_window,
                    image_extent: polymer_renderer::renderer::frame_image_extent(
                        frame,
                        stream.width,
                        stream.height,
                        *turns,
                    ),
                    quarter_turns: *turns,
                    flip_y: stream.bottom_up,
                    frame,
                })
            })
            .collect()
    }
    fn has_pending_frame(&self) -> bool {
        !self.swapchain.is_empty()
    }
    fn recreate_swapchain(&mut self, _: &impl RenderWindow) -> Result<bool, Box<dyn Error>> {
        Ok(false)
    }
    fn present(&mut self, _: &CompositingPlan) -> Result<bool, Box<dyn Error>> {
        let publisher = self
            .publisher
            .as_ref()
            .ok_or("WebRTC renderer has no publisher")?;
        let frames = self.swapchain.drain();
        let presented = !frames.is_empty();
        for frame in frames {
            publisher.publish(frame);
        }
        Ok(presented)
    }
}
