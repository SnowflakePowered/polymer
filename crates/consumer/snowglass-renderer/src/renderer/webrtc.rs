//! Video swapchain composition for WebRTC outputs.

use crate::webrtc::{FRAME_CHANNEL_CAPACITY, VideoFrame};
use ash::vk;
use snowglass_protocol::AdapterLuid;
use std::collections::{HashMap, VecDeque};
use windows::Win32::Graphics::{
    Direct3D12::{
        D3D12_RESOURCE_STATES, ID3D12CommandQueue, ID3D12Device, ID3D12Fence, ID3D12Resource,
    },
    Dxgi::Common::DXGI_FORMAT,
};

use super::d3d12::D3d12Compositor;

/// A bounded, non-blocking swapchain of frames ready for video encoding.
///
/// Unlike a window swapchain this has no native presentation surface. A
/// backend compositor presents an imported surface by submitting a
/// [`VideoFrame`], and the WebRTC publisher drains frames independently.
pub struct VideoSwapchain {
    capacity: usize,
    frames: VecDeque<VideoFrame>,
}

impl VideoSwapchain {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            frames: VecDeque::with_capacity(capacity.max(1)),
        }
    }

    pub fn available(&self) -> usize {
        self.capacity.saturating_sub(self.frames.len())
    }

    /// Presents a frame without blocking GPU composition.
    ///
    /// Returns `false` when the encoder has not drained an earlier frame.
    pub fn present(&mut self, frame: VideoFrame) -> bool {
        if self.frames.len() >= self.capacity {
            return false;
        }
        self.frames.push_back(frame);
        true
    }

    pub fn drain(&mut self) -> Vec<VideoFrame> {
        self.frames.drain(..).collect()
    }
}

/// Dedicated compositor for video outputs.
///
/// D3D12 and Vulkan window compositors only prepare backend-native frames;
/// this component owns their video presentation queue and hands completed
/// frames to the WebRTC publishing layer.
pub struct WebRtcCompositor {
    swapchain: VideoSwapchain,
    d3d12: Option<D3d12WebRtcState>,
    vulkan: Option<VulkanWebRtcState>,
}

struct D3d12WebRtcState {
    adapter: AdapterLuid,
    completion_fence: ID3D12Fence,
    completion_value: u64,
    stream_completion: HashMap<u64, u64>,
}

pub(crate) struct VulkanVideoCache {
    pub(crate) image: vk::Image,
    pub(crate) memory: vk::DeviceMemory,
    pub(crate) initialized: bool,
}

pub(crate) struct VulkanVideoReadback {
    pub(crate) buffer: vk::Buffer,
    pub(crate) memory: vk::DeviceMemory,
    pub(crate) pending: Option<snowglass_protocol::FrameReady>,
}

pub(crate) struct VulkanWebRtcStream {
    pub(crate) readbacks: Vec<VulkanVideoReadback>,
    pub(crate) cache: Option<VulkanVideoCache>,
    pub(crate) completion_value: u64,
}

pub(crate) struct VulkanWebRtcState {
    pub(crate) physical_device: vk::PhysicalDevice,
    pub(crate) device: ash::Device,
    pub(crate) queue: Option<vk::Queue>,
    pub(crate) queue_family: Option<u32>,
    pub(crate) conversion_queue: Option<vk::Queue>,
    pub(crate) conversion_queue_family: u32,
    pub(crate) ready: Option<vk::Semaphore>,
    pub(crate) completion: Option<vk::Semaphore>,
    pub(crate) ready_value: u64,
    pub(crate) completion_value: u64,
    pub(crate) streams: HashMap<u64, VulkanWebRtcStream>,
}

impl Drop for VulkanWebRtcState {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            for (_, mut stream) in self.streams.drain() {
                for readback in stream.readbacks.drain(..) {
                    self.device.destroy_buffer(readback.buffer, None);
                    self.device.free_memory(readback.memory, None);
                }
                if let Some(cache) = stream.cache.take() {
                    self.device.destroy_image(cache.image, None);
                    self.device.free_memory(cache.memory, None);
                }
            }
            if let Some(semaphore) = self.ready.take() {
                self.device.destroy_semaphore(semaphore, None);
            }
            if let Some(semaphore) = self.completion.take() {
                self.device.destroy_semaphore(semaphore, None);
            }
        }
    }
}

impl WebRtcCompositor {
    pub fn new_d3d12(compositor: &D3d12Compositor) -> windows::core::Result<Self> {
        let completion_fence = unsafe {
            compositor.d3d12_device().CreateFence(
                0,
                windows::Win32::Graphics::Direct3D12::D3D12_FENCE_FLAG_NONE,
            )?
        };
        Ok(Self {
            swapchain: VideoSwapchain::new(FRAME_CHANNEL_CAPACITY),
            d3d12: Some(D3d12WebRtcState {
                adapter: compositor.adapter_luid(),
                completion_fence,
                completion_value: 0,
                stream_completion: HashMap::new(),
            }),
            vulkan: None,
        })
    }

    #[expect(clippy::too_many_arguments)]
    pub(crate) fn new_vulkan(
        physical_device: vk::PhysicalDevice,
        device: ash::Device,
        queue: Option<vk::Queue>,
        queue_family: Option<u32>,
        conversion_queue: Option<vk::Queue>,
        conversion_queue_family: u32,
        ready: Option<vk::Semaphore>,
        completion: Option<vk::Semaphore>,
    ) -> Self {
        Self {
            swapchain: VideoSwapchain::new(FRAME_CHANNEL_CAPACITY),
            d3d12: None,
            vulkan: Some(VulkanWebRtcState {
                physical_device,
                device,
                queue,
                queue_family,
                conversion_queue,
                conversion_queue_family,
                ready,
                completion,
                ready_value: 0,
                completion_value: 0,
                streams: HashMap::new(),
            }),
        }
    }

    pub fn available(&self) -> usize {
        self.swapchain.available()
    }

    pub fn present(&mut self, frame: VideoFrame) -> bool {
        self.swapchain.present(frame)
    }

    pub fn take_frames(&mut self) -> Vec<VideoFrame> {
        self.swapchain.drain()
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
        let _ = self.swapchain.present(VideoFrame {
            connection,
            width,
            height,
            data: crate::webrtc::VideoFrameData::D3d12(crate::webrtc::D3d12Frame {
                device,
                resource,
                state,
                format,
                ready_fence,
                ready_value,
                completion_fence: d3d12.completion_fence.clone(),
                completion_value,
                completion_submitted: false,
            }),
        });
    }

    pub fn disconnect_stream(&mut self, connection: u64) {
        if let Some(d3d12) = &mut self.d3d12 {
            d3d12.stream_completion.remove(&connection);
        }
        if let Some(vulkan) = &mut self.vulkan
            && let Some(mut stream) = vulkan.streams.remove(&connection)
        {
            unsafe {
                let _ = vulkan.device.device_wait_idle();
                for readback in stream.readbacks.drain(..) {
                    vulkan.device.destroy_buffer(readback.buffer, None);
                    vulkan.device.free_memory(readback.memory, None);
                }
                if let Some(cache) = stream.cache.take() {
                    vulkan.device.destroy_image(cache.image, None);
                    vulkan.device.free_memory(cache.memory, None);
                }
            }
        }
    }

    pub(crate) fn vulkan_parts_mut(
        &mut self,
    ) -> Option<(&mut VideoSwapchain, &mut VulkanWebRtcState)> {
        self.vulkan
            .as_mut()
            .map(|vulkan| (&mut self.swapchain, vulkan))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webrtc::VideoFrameData;

    fn frame(connection: u64) -> VideoFrame {
        VideoFrame {
            connection,
            width: 1,
            height: 1,
            data: VideoFrameData::Rgba(vec![0; 4]),
        }
    }

    #[test]
    fn video_swapchain_is_bounded_and_non_blocking() {
        let mut swapchain = VideoSwapchain::new(1);
        assert!(swapchain.present(frame(1)));
        assert!(!swapchain.present(frame(2)));
        assert_eq!(swapchain.drain()[0].connection, 1);
        assert_eq!(swapchain.available(), 1);
    }
}
