//! Hardware WebRTC output for this backend.
//!
//! The companion window compositor prepares GPU frame copies during its existing
//! composition pass; [`Renderer::present`] submits them to the shared publisher.
//! Encoding sessions and native resource leases remain owned by this backend.

pub(crate) mod encoder;
mod frame;
use ash::vk;
pub(crate) use frame::*;
use parking_lot::Mutex;
use polymer_renderer::webrtc::{FRAME_CHANNEL_CAPACITY, Publisher, VideoSwapchain};
use polymer_renderer::{CompositingPlan, ReceivedSurface, RenderWindow, Renderer};
use std::{collections::HashMap, error::Error, sync::Arc};
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
    vulkan: Option<VulkanWebRtcState>,
}

pub(crate) struct VulkanVideoCache {
    pub(crate) image: vk::Image,
    pub(crate) memory: vk::DeviceMemory,
    pub(crate) initialized: bool,
}

pub(crate) struct VulkanWebRtcStream {
    pub(crate) cache: Option<VulkanVideoCache>,
    pub(crate) completion_value: u64,
}

pub(crate) struct VulkanWebRtcState {
    pub(crate) instance: ash::Instance,
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
    pub(crate) encoders: HashMap<u64, Arc<Mutex<encoder::Session>>>,
    pub(crate) streams: HashMap<u64, VulkanWebRtcStream>,
}

impl Drop for VulkanWebRtcState {
    fn drop(&mut self) {
        unsafe {
            self.encoders.clear();
            let _ = self.device.device_wait_idle();
            for (_, mut stream) in self.streams.drain() {
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
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn new_vulkan(
        instance: ash::Instance,
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
            publisher: None,
            surfaces: HashMap::new(),
            vulkan: Some(VulkanWebRtcState {
                instance,
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
                encoders: HashMap::new(),
            }),
        }
    }

    pub fn available(&self) -> usize {
        self.swapchain.available()
    }

    pub fn disconnect_stream(&mut self, connection: u64) {
        self.swapchain.disconnect_stream(connection);
        self.surfaces.remove(&connection);
        if let Some(publisher) = &self.publisher {
            publisher.remove_stream(connection);
            publisher.flush();
        }
        if let Some(vulkan) = &mut self.vulkan {
            vulkan.encoders.remove(&connection);
        }
        if let Some(vulkan) = &mut self.vulkan
            && let Some(mut stream) = vulkan.streams.remove(&connection)
        {
            unsafe {
                let _ = vulkan.device.device_wait_idle();
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
        self.import_video_stream(offer.connection_id(), offer.stream())?;
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

impl WebRtcCompositor {
    fn import_video_stream(
        &mut self,
        connection: u64,
        stream: &polymer_ipc::StreamReady,
    ) -> Result<(), Box<dyn Error>> {
        let video = self
            .vulkan
            .as_mut()
            .ok_or("Vulkan video context unavailable")?;
        if video.streams.contains_key(&connection) {
            return Ok(());
        }
        let family = video
            .queue_family
            .ok_or("Vulkan hardware video encoding unavailable")?;
        let format = polymer_consumer_vulkan::format_for_stream(stream)?;
        let cache = unsafe {
            crate::renderer::compositor::create_vulkan_video_cache(
                &video.instance,
                &video.device,
                video.physical_device,
                stream.width,
                stream.height,
                format,
                video.conversion_queue_family,
                family,
            )?
        };
        video.streams.insert(
            connection,
            VulkanWebRtcStream {
                cache: Some(cache),
                completion_value: 0,
            },
        );
        Ok(())
    }
}

// Vulkan frames borrow the compositor's device and cache images. Drain the
// worker before tearing these down, including frames still awaiting encoding.
impl Drop for WebRtcCompositor {
    fn drop(&mut self) {
        self.swapchain.clear();
        if let Some(publisher) = &self.publisher {
            publisher.flush();
        }
    }
}
