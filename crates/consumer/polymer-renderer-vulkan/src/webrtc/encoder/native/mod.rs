//! Vulkan Video HEVC/H.264 encoder.
#![allow(unsafe_code)]

use std::io::Cursor;

use ash::vk::native;
use ash::{util::read_spv, vk};

use super::super::VulkanFrame;
use polymer_renderer::webrtc::{Codec, NativeEncoder};

mod convert;
mod encode;
mod parameters;
mod session;
mod setup;
use parameters::*;
use setup::*;

type Error = Box<dyn std::error::Error + Send + Sync>;

const GPU_WAIT_TIMEOUT_NS: u64 = 5_000_000_000;
const DROP_WAIT_TIMEOUT_NS: u64 = 100_000_000;

struct Image {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
}

struct Buffer {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    size: u64,
}

struct VideoState {
    codec: Codec,
    device: ash::Device,
    video: ash::khr::video_queue::Device,
    encode: ash::khr::video_encode_queue::Device,
    session: vk::VideoSessionKHR,
    parameters: vk::VideoSessionParametersKHR,
    session_memory: Vec<vk::DeviceMemory>,
    input: Image,
    /// Reconstructed-picture slot. The H.265 encode path always provides a
    /// setup reference slot: encoding without one is legal for a zero-slot
    /// session, but NVIDIA's HEVC engine silently drops such submissions.
    dpb: Option<Image>,
    parameter_sets: Vec<u8>,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
    command: vk::CommandBuffer,
    fence: vk::Fence,
    fence_in_flight: bool,
    bitstream: Buffer,
    bitstream_offset: u64,
    query_pool: vk::QueryPool,
    width: u32,
    height: u32,
    bitrate: u64,
    frame_rate: u32,
    rate_control_mode: vk::VideoEncodeRateControlModeFlagsKHR,
    initialized: bool,
}

pub(crate) struct Encoder {
    codec: Codec,
    device: ash::Device,
    conversion_queue: vk::Queue,
    command_pool: vk::CommandPool,
    command: vk::CommandBuffer,
    conversion_fence: vk::Fence,
    conversion_fence_in_flight: bool,
    conversion_done: vk::Semaphore,
    conversion_value: u64,
    descriptor_pool: vk::DescriptorPool,
    descriptor_layout: vk::DescriptorSetLayout,
    descriptor_set: vk::DescriptorSet,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    sampler: vk::Sampler,
    source_view: vk::ImageView,
    luma: Image,
    chroma: Image,
    video: VideoState,
    width: u32,
    height: u32,
    output_initialized: bool,
    input_initialized: bool,
    synchronization_announced: bool,
}

impl NativeEncoder for Encoder {
    type Frame = VulkanFrame;
    const BACKEND: &'static str = "VULKAN Video Encode";

    fn new_for_frame(
        frame: &VulkanFrame,
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: f32,
        preferred: Codec,
    ) -> Result<Self, Error> {
        if preferred == Codec::H264 {
            return Self::new_with_codec(frame, width, height, bitrate, frame_rate, Codec::H264);
        }
        match Self::new_with_codec(frame, width, height, bitrate, frame_rate, Codec::Hevc) {
            Ok(encoder) => Ok(encoder),
            Err(hevc_error) => {
                tracing::warn!("polymer: Vulkan HEVC unavailable; trying H.264: {hevc_error}");
                Self::new_with_codec(frame, width, height, bitrate, frame_rate, Codec::H264)
            }
        }
    }

    fn codec(&self) -> Codec {
        self.codec
    }

    fn encode_frame(
        &mut self,
        frame: &mut VulkanFrame,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, Error> {
        if width != self.width || height != self.height {
            return Err("Vulkan encoder resolution changed".into());
        }
        self.convert(frame)?;
        self.video.encode_frame(
            &self.device,
            frame,
            self.conversion_done,
            self.conversion_value,
        )
    }

    fn has_in_flight_work(&self) -> bool {
        self.conversion_fence_in_flight || self.video.fence_in_flight
    }
}

/// Block-size configuration derived from `VideoEncodeH265CapabilitiesKHR`.
/// The SPS must describe exactly the block structure the hardware encodes
/// with, and picture dimensions must be multiples of the minimum coding block
/// size, cropping the remainder through the conformance window.
#[derive(Clone, Copy)]
struct HevcBlockSizes {
    ctb_log2: u32,
    min_tu_log2: u32,
    max_tu_log2: u32,
}

fn vk_result(result: vk::Result) -> Result<(), vk::Result> {
    if result == vk::Result::SUCCESS {
        Ok(())
    } else {
        Err(result)
    }
}

fn wait_for_fence(device: &ash::Device, fence: vk::Fence, operation: &str) -> Result<(), Error> {
    match unsafe { device.wait_for_fences(&[fence], true, GPU_WAIT_TIMEOUT_NS) } {
        Ok(()) => Ok(()),
        Err(vk::Result::TIMEOUT) => Err(format!("timed out waiting for {operation}").into()),
        Err(error) => Err(format!("{operation} failed: {error}").into()),
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            if self.conversion_fence_in_flight {
                if self
                    .device
                    .wait_for_fences(&[self.conversion_fence], true, DROP_WAIT_TIMEOUT_NS)
                    .is_err()
                {
                    // The queue may still reference the conversion resources.
                    // Leaking them is safer than hanging or destroying in-use objects.
                    return;
                }
            }
            self.device.destroy_fence(self.conversion_fence, None);
            self.device.destroy_semaphore(self.conversion_done, None);
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_pipeline(self.pipeline, None);
            self.device
                .destroy_pipeline_layout(self.pipeline_layout, None);
            self.device
                .destroy_descriptor_pool(self.descriptor_pool, None);
            self.device
                .destroy_descriptor_set_layout(self.descriptor_layout, None);
            self.device.destroy_sampler(self.sampler, None);
            self.device.destroy_image_view(self.source_view, None);
            destroy_image(&self.device, &self.luma);
            destroy_image(&self.device, &self.chroma);
        }
    }
}

impl Drop for VideoState {
    fn drop(&mut self) {
        unsafe {
            if self.fence_in_flight {
                if self
                    .device
                    .wait_for_fences(&[self.fence], true, DROP_WAIT_TIMEOUT_NS)
                    .is_err()
                {
                    // The queue may still reference the video resources.
                    return;
                }
            }
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_query_pool(self.query_pool, None);
            destroy_buffer(&self.device, &self.bitstream);
            destroy_image(&self.device, &self.input);
            if let Some(dpb) = &self.dpb {
                destroy_image(&self.device, dpb);
            }
            (self.video.fp().destroy_video_session_parameters_khr)(
                self.device.handle(),
                self.parameters,
                std::ptr::null(),
            );
            (self.video.fp().destroy_video_session_khr)(
                self.device.handle(),
                self.session,
                std::ptr::null(),
            );
            for memory in self.session_memory.drain(..) {
                self.device.free_memory(memory, None);
            }
        }
    }
}

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

fn image_barrier() -> vk::ImageMemoryBarrier<'static> {
    vk::ImageMemoryBarrier::default()
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
}
