//! Vulkan Video HEVC/H.264 encoder and on-demand OpenH264 fallback readback.
#![allow(unsafe_code)]

use std::io::Cursor;

use ash::vk::native;
use ash::{util::read_spv, vk};

use super::{super::VulkanFrame, Codec};

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

pub(super) struct Encoder {
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

impl Encoder {
    pub(super) fn new_for_frame(
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

    fn new_with_codec(
        frame: &VulkanFrame,
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: f32,
        codec: Codec,
    ) -> Result<Self, Error> {
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            return Err("Vulkan video encoding requires a non-zero, even-sized frame".into());
        }
        unsafe {
            let device = frame.device.clone();
            let source_view = device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(frame.image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(frame.format)
                    .subresource_range(color_range()),
                None,
            )?;
            let luma = create_image(
                &frame.instance,
                &device,
                frame.physical_device,
                width,
                height,
                vk::Format::R8_UNORM,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC,
            )?;
            let chroma = create_image(
                &frame.instance,
                &device,
                frame.physical_device,
                width / 2,
                height / 2,
                vk::Format::R8G8_UNORM,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC,
            )?;
            let sampler = device.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::NEAREST)
                    .min_filter(vk::Filter::NEAREST)
                    .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                    .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                    .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                None,
            )?;
            let bindings = [
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(1)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(2)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
            ];
            let descriptor_layout = device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                None,
            )?;
            let pool_sizes = [
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                    descriptor_count: 1,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::STORAGE_IMAGE,
                    descriptor_count: 2,
                },
            ];
            let descriptor_pool = device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(1)
                    .pool_sizes(&pool_sizes),
                None,
            )?;
            let descriptor_set = device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(descriptor_pool)
                    .set_layouts(&[descriptor_layout]),
            )?[0];
            let source_info = [vk::DescriptorImageInfo::default()
                .sampler(sampler)
                .image_view(source_view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let luma_info = [vk::DescriptorImageInfo::default()
                .image_view(luma.view)
                .image_layout(vk::ImageLayout::GENERAL)];
            let chroma_info = [vk::DescriptorImageInfo::default()
                .image_view(chroma.view)
                .image_layout(vk::ImageLayout::GENERAL)];
            device.update_descriptor_sets(
                &[
                    vk::WriteDescriptorSet::default()
                        .dst_set(descriptor_set)
                        .dst_binding(0)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(&source_info),
                    vk::WriteDescriptorSet::default()
                        .dst_set(descriptor_set)
                        .dst_binding(1)
                        .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                        .image_info(&luma_info),
                    vk::WriteDescriptorSet::default()
                        .dst_set(descriptor_set)
                        .dst_binding(2)
                        .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                        .image_info(&chroma_info),
                ],
                &[],
            );
            let pipeline_layout = device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default().set_layouts(&[descriptor_layout]),
                None,
            )?;
            let shader_words = read_spv(&mut Cursor::new(include_bytes!("rgba_to_nv12.comp.spv")))?;
            let shader = device.create_shader_module(
                &vk::ShaderModuleCreateInfo::default().code(&shader_words),
                None,
            )?;
            let pipeline = device
                .create_compute_pipelines(
                    vk::PipelineCache::null(),
                    &[vk::ComputePipelineCreateInfo::default()
                        .stage(
                            vk::PipelineShaderStageCreateInfo::default()
                                .stage(vk::ShaderStageFlags::COMPUTE)
                                .module(shader)
                                .name(c"main"),
                        )
                        .layout(pipeline_layout)],
                    None,
                )
                .map_err(|(_, error)| error)?[0];
            device.destroy_shader_module(shader, None);
            let command_pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(frame.conversion_queue_family),
                None,
            )?;
            let command = device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            let conversion_fence = device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?;
            let mut timeline_type = vk::SemaphoreTypeCreateInfo::default()
                .semaphore_type(vk::SemaphoreType::TIMELINE)
                .initial_value(0);
            let conversion_done = device.create_semaphore(
                &vk::SemaphoreCreateInfo::default().push_next(&mut timeline_type),
                None,
            )?;
            let video = VideoState::new(frame, width, height, bitrate, frame_rate, codec)
                .map_err(|error| format!("Vulkan Video initialization failed: {error}"))?;
            Ok(Self {
                codec,
                device,
                conversion_queue: frame.conversion_queue,
                command_pool,
                command,
                conversion_fence,
                conversion_fence_in_flight: false,
                conversion_done,
                conversion_value: 0,
                descriptor_pool,
                descriptor_layout,
                descriptor_set,
                pipeline_layout,
                pipeline,
                sampler,
                source_view,
                luma,
                chroma,
                video,
                width,
                height,
                output_initialized: false,
                input_initialized: false,
                synchronization_announced: false,
            })
        }
    }

    pub(super) fn codec(&self) -> Codec {
        self.codec
    }

    pub(super) fn encode_frame(
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

    pub(super) fn has_in_flight_work(&self) -> bool {
        self.conversion_fence_in_flight || self.video.fence_in_flight
    }

    fn convert(&mut self, frame: &VulkanFrame) -> Result<(), Error> {
        unsafe {
            if self.conversion_fence_in_flight {
                wait_for_fence(
                    &self.device,
                    self.conversion_fence,
                    "previous Vulkan conversion",
                )?;
                self.conversion_fence_in_flight = false;
            }
            self.device.reset_fences(&[self.conversion_fence])?;
            self.device
                .reset_command_pool(self.command_pool, vk::CommandPoolResetFlags::empty())?;
            self.device.begin_command_buffer(
                self.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            let source = image_barrier()
                .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .old_layout(frame.layout)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .image(frame.image)
                .subresource_range(color_range());
            let outputs = [
                image_barrier()
                    .src_access_mask(vk::AccessFlags::empty())
                    .dst_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .old_layout(if self.output_initialized {
                        vk::ImageLayout::GENERAL
                    } else {
                        vk::ImageLayout::UNDEFINED
                    })
                    .new_layout(vk::ImageLayout::GENERAL)
                    .image(self.luma.image)
                    .subresource_range(color_range()),
                image_barrier()
                    .src_access_mask(vk::AccessFlags::empty())
                    .dst_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .old_layout(if self.output_initialized {
                        vk::ImageLayout::GENERAL
                    } else {
                        vk::ImageLayout::UNDEFINED
                    })
                    .new_layout(vk::ImageLayout::GENERAL)
                    .image(self.chroma.image)
                    .subresource_range(color_range()),
            ];
            self.device.cmd_pipeline_barrier(
                self.command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[source, outputs[0], outputs[1]],
            );
            self.device.cmd_bind_pipeline(
                self.command,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline,
            );
            self.device.cmd_bind_descriptor_sets(
                self.command,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline_layout,
                0,
                &[self.descriptor_set],
                &[],
            );
            self.device.cmd_dispatch(
                self.command,
                self.width.div_ceil(16),
                self.height.div_ceil(16),
                1,
            );
            let planes_to_copy = [
                image_barrier()
                    .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .old_layout(vk::ImageLayout::GENERAL)
                    .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .image(self.luma.image)
                    .subresource_range(color_range()),
                image_barrier()
                    .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .old_layout(vk::ImageLayout::GENERAL)
                    .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .image(self.chroma.image)
                    .subresource_range(color_range()),
                image_barrier()
                    .src_access_mask(if self.input_initialized {
                        vk::AccessFlags::MEMORY_READ
                    } else {
                        vk::AccessFlags::empty()
                    })
                    .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .old_layout(if self.input_initialized {
                        vk::ImageLayout::VIDEO_ENCODE_SRC_KHR
                    } else {
                        vk::ImageLayout::UNDEFINED
                    })
                    .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .image(self.video.input.image)
                    .subresource_range(color_range()),
            ];
            self.device.cmd_pipeline_barrier(
                self.command,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &planes_to_copy,
            );
            copy_plane(
                &self.device,
                self.command,
                self.luma.image,
                self.video.input.image,
                vk::ImageAspectFlags::PLANE_0,
                self.width,
                self.height,
            );
            copy_plane(
                &self.device,
                self.command,
                self.chroma.image,
                self.video.input.image,
                vk::ImageAspectFlags::PLANE_1,
                self.width / 2,
                self.height / 2,
            );
            let planes_back = [
                image_barrier()
                    .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .dst_access_mask(vk::AccessFlags::empty())
                    .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .image(self.luma.image)
                    .subresource_range(color_range()),
                image_barrier()
                    .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .dst_access_mask(vk::AccessFlags::empty())
                    .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .image(self.chroma.image)
                    .subresource_range(color_range()),
                image_barrier()
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::empty())
                    .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .new_layout(vk::ImageLayout::VIDEO_ENCODE_SRC_KHR)
                    .image(self.video.input.image)
                    .subresource_range(color_range()),
            ];
            self.device.cmd_pipeline_barrier(
                self.command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &planes_back,
            );
            let source_back = image_barrier()
                .src_access_mask(vk::AccessFlags::SHADER_READ)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .new_layout(frame.layout)
                .image(frame.image)
                .subresource_range(color_range());
            self.device.cmd_pipeline_barrier(
                self.command,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[source_back],
            );
            self.device.end_command_buffer(self.command)?;
            let commands = [self.command];
            let waits = [frame.ready];
            let values = [frame.ready_value];
            self.conversion_value += 1;
            let signals = [self.conversion_done];
            let signal_values = [self.conversion_value];
            let stages = [vk::PipelineStageFlags::ALL_COMMANDS];
            let mut timeline = vk::TimelineSemaphoreSubmitInfo::default()
                .wait_semaphore_values(&values)
                .signal_semaphore_values(&signal_values);
            let submit = vk::SubmitInfo::default()
                .push_next(&mut timeline)
                .wait_semaphores(&waits)
                .wait_dst_stage_mask(&stages)
                .command_buffers(&commands)
                .signal_semaphores(&signals);
            if !self.synchronization_announced {
                tracing::debug!(
                    "polymer: Vulkan WebRTC conversion waiting for ready timeline value {}",
                    frame.ready_value
                );
            }
            self.device
                .queue_submit(self.conversion_queue, &[submit], self.conversion_fence)?;
            self.conversion_fence_in_flight = true;
            wait_for_fence(
                &self.device,
                self.conversion_fence,
                "Vulkan RGBA-to-NV12 conversion",
            )?;
            self.conversion_fence_in_flight = false;
            if !self.synchronization_announced {
                tracing::debug!("polymer: Vulkan WebRTC conversion completed");
                self.synchronization_announced = true;
            }
            self.output_initialized = true;
            self.input_initialized = true;
            Ok(())
        }
    }
}

impl VideoState {
    unsafe fn new(
        frame: &VulkanFrame,
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: f32,
        codec: Codec,
    ) -> Result<Self, Error> {
        if codec == Codec::Hevc {
            return unsafe { Self::new_hevc(frame, width, height, bitrate, frame_rate) };
        }
        unsafe {
            let video_instance = ash::khr::video_queue::Instance::new(
                // The loader only retains function pointers and the instance handle.
                &ash::Entry::load()?,
                &frame.instance,
            );
            let video = ash::khr::video_queue::Device::new(&frame.instance, &frame.device);
            let encode = ash::khr::video_encode_queue::Device::new(&frame.instance, &frame.device);
            let mut h264_profile = vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(
                native::StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE,
            );
            let profile = vk::VideoProfileInfoKHR::default()
                .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
                .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
                .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .push_next(&mut h264_profile);

            let mut h264_capabilities = vk::VideoEncodeH264CapabilitiesKHR::default();
            let mut encode_capabilities = vk::VideoEncodeCapabilitiesKHR::default();
            let mut capabilities = vk::VideoCapabilitiesKHR::default()
                .push_next(&mut h264_capabilities)
                .push_next(&mut encode_capabilities);
            vk_result((video_instance
                .fp()
                .get_physical_device_video_capabilities_khr)(
                frame.physical_device,
                &profile,
                &mut capabilities,
            ))
            .map_err(|error| format!("query H.264 capabilities: {error}"))?;
            let extent = vk::Extent2D { width, height };
            if width < capabilities.min_coded_extent.width
                || height < capabilities.min_coded_extent.height
                || width > capabilities.max_coded_extent.width
                || height > capabilities.max_coded_extent.height
            {
                return Err("frame extent is outside Vulkan Video H.264 capabilities".into());
            }
            let std_header_version = capabilities.std_header_version;
            let bitstream_alignment = capabilities.min_bitstream_buffer_size_alignment.max(1);
            let bitstream_offset_alignment =
                capabilities.min_bitstream_buffer_offset_alignment.max(1);
            if !encode_capabilities
                .supported_encode_feedback_flags
                .contains(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN)
            {
                return Err("driver cannot report Vulkan Video bitstream byte counts".into());
            }
            let rate_control_mode = if encode_capabilities
                .rate_control_modes
                .contains(vk::VideoEncodeRateControlModeFlagsKHR::CBR)
            {
                vk::VideoEncodeRateControlModeFlagsKHR::CBR
            } else if encode_capabilities
                .rate_control_modes
                .contains(vk::VideoEncodeRateControlModeFlagsKHR::VBR)
            {
                vk::VideoEncodeRateControlModeFlagsKHR::VBR
            } else {
                vk::VideoEncodeRateControlModeFlagsKHR::DISABLED
            };

            let profiles = [profile];
            let mut profile_list = vk::VideoProfileListInfoKHR::default().profiles(&profiles);
            let format_info = vk::PhysicalDeviceVideoFormatInfoKHR::default()
                .image_usage(
                    vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR | vk::ImageUsageFlags::TRANSFER_DST,
                )
                .push_next(&mut profile_list);
            let mut format_count = 0;
            vk_result((video_instance
                .fp()
                .get_physical_device_video_format_properties_khr)(
                frame.physical_device,
                &format_info,
                &mut format_count,
                std::ptr::null_mut(),
            ))
            .map_err(|error| format!("count H.264 input formats: {error}"))?;
            if format_count == 0 {
                return Err("driver exposes no H.264 encode input format".into());
            }
            let mut formats = vec![vk::VideoFormatPropertiesKHR::default(); format_count as usize];
            vk_result((video_instance
                .fp()
                .get_physical_device_video_format_properties_khr)(
                frame.physical_device,
                &format_info,
                &mut format_count,
                formats.as_mut_ptr(),
            ))
            .map_err(|error| format!("query H.264 input formats: {error}"))?;
            let input_format = formats
                .iter()
                .find(|properties| properties.format == vk::Format::G8_B8R8_2PLANE_420_UNORM)
                .ok_or("driver does not expose two-plane NV12 H.264 input")?
                .format;

            let mut h264_session = vk::VideoEncodeH264SessionCreateInfoKHR::default()
                .use_max_level_idc(true)
                .max_level_idc(native::StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_1);
            let session_info = vk::VideoSessionCreateInfoKHR::default()
                .queue_family_index(frame.queue_family)
                .video_profile(&profile)
                .picture_format(input_format)
                .max_coded_extent(extent)
                .reference_picture_format(input_format)
                .max_dpb_slots(0)
                .max_active_reference_pictures(0)
                .std_header_version(&std_header_version)
                .push_next(&mut h264_session);
            let mut session = vk::VideoSessionKHR::null();
            vk_result((video.fp().create_video_session_khr)(
                frame.device.handle(),
                &session_info,
                std::ptr::null(),
                &mut session,
            ))
            .map_err(|error| format!("create H.264 video session: {error}"))?;

            let memory_properties = frame
                .instance
                .get_physical_device_memory_properties(frame.physical_device);
            let mut requirement_count = 0;
            vk_result((video.fp().get_video_session_memory_requirements_khr)(
                frame.device.handle(),
                session,
                &mut requirement_count,
                std::ptr::null_mut(),
            ))
            .map_err(|error| format!("count video session memory requirements: {error}"))?;
            let mut requirements =
                vec![vk::VideoSessionMemoryRequirementsKHR::default(); requirement_count as usize];
            vk_result((video.fp().get_video_session_memory_requirements_khr)(
                frame.device.handle(),
                session,
                &mut requirement_count,
                requirements.as_mut_ptr(),
            ))
            .map_err(|error| format!("query video session memory requirements: {error}"))?;
            let mut session_memory = Vec::with_capacity(requirements.len());
            let mut bindings = Vec::with_capacity(requirements.len());
            for requirement in &requirements {
                let memory_type = find_memory_type(
                    &memory_properties,
                    requirement.memory_requirements.memory_type_bits,
                    vk::MemoryPropertyFlags::DEVICE_LOCAL,
                )
                .or_else(|_| {
                    find_memory_type(
                        &memory_properties,
                        requirement.memory_requirements.memory_type_bits,
                        vk::MemoryPropertyFlags::empty(),
                    )
                })
                .map_err(|error| format!("find video session memory type: {error}"))?;
                let memory = frame.device.allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(requirement.memory_requirements.size)
                        .memory_type_index(memory_type),
                    None,
                )?;
                bindings.push(
                    vk::BindVideoSessionMemoryInfoKHR::default()
                        .memory_bind_index(requirement.memory_bind_index)
                        .memory(memory)
                        .memory_offset(0)
                        .memory_size(requirement.memory_requirements.size),
                );
                session_memory.push(memory);
            }
            vk_result((video.fp().bind_video_session_memory_khr)(
                frame.device.handle(),
                session,
                bindings.len() as u32,
                bindings.as_ptr(),
            ))
            .map_err(|error| format!("bind video session memory: {error}"))?;

            let (sps, pps) = h264_parameter_structures(width, height);
            let sps_list = [sps];
            let pps_list = [pps];
            let add = vk::VideoEncodeH264SessionParametersAddInfoKHR::default()
                .std_sp_ss(&sps_list)
                .std_pp_ss(&pps_list);
            let mut h264_parameters = vk::VideoEncodeH264SessionParametersCreateInfoKHR::default()
                .max_std_sps_count(1)
                .max_std_pps_count(1)
                .parameters_add_info(&add);
            let parameters_info = vk::VideoSessionParametersCreateInfoKHR::default()
                .video_session(session)
                .push_next(&mut h264_parameters);
            let mut parameters = vk::VideoSessionParametersKHR::null();
            vk_result((video.fp().create_video_session_parameters_khr)(
                frame.device.handle(),
                &parameters_info,
                std::ptr::null(),
                &mut parameters,
            ))
            .map_err(|error| format!("create H.264 session parameters: {error}"))?;
            let parameter_sets = encoded_parameter_sets(&encode, &frame.device, parameters)
                .map_err(|error| format!("encode H.264 parameter sets: {error}"))?;
            let input = create_video_image(
                frame,
                width,
                height,
                input_format,
                vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR | vk::ImageUsageFlags::TRANSFER_DST,
                &profiles,
            )
            .map_err(|error| format!("create H.264 input image: {error}"))?;
            let bitstream_offset =
                align_up(parameter_sets.len() as u64, bitstream_offset_alignment);
            let bitstream_size = align_up(
                bitstream_offset + width as u64 * height as u64 * 3 + 64 * 1024,
                bitstream_alignment,
            );
            let bitstream = create_video_buffer(frame, bitstream_size, &profiles)
                .map_err(|error| format!("create H.264 bitstream buffer: {error}"))?;
            let mut feedback = vk::QueryPoolVideoEncodeFeedbackCreateInfoKHR::default()
                .encode_feedback_flags(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN);
            let mut query_profile = profile;
            let query_info = vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR)
                .query_count(1)
                .push_next(&mut query_profile)
                .push_next(&mut feedback);
            let query_pool = frame
                .device
                .create_query_pool(&query_info, None)
                .map_err(|error| format!("create video feedback query: {error}"))?;
            let command_pool = frame.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(frame.queue_family),
                None,
            )?;
            let command = frame.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            let fence = frame.device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?;

            Ok(Self {
                codec: Codec::H264,
                dpb: None,
                device: frame.device.clone(),
                video,
                encode,
                session,
                parameters,
                session_memory,
                input,
                parameter_sets,
                queue: frame.queue,
                command_pool,
                command,
                fence,
                fence_in_flight: false,
                bitstream,
                bitstream_offset,
                query_pool,
                width,
                height,
                bitrate: (bitrate as u64).min(encode_capabilities.max_bitrate),
                frame_rate: frame_rate.round().max(1.0) as u32,
                rate_control_mode,
                initialized: false,
            })
        }
    }

    unsafe fn new_hevc(
        frame: &VulkanFrame,
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: f32,
    ) -> Result<Self, Error> {
        unsafe {
            let video_instance =
                ash::khr::video_queue::Instance::new(&ash::Entry::load()?, &frame.instance);
            let video = ash::khr::video_queue::Device::new(&frame.instance, &frame.device);
            let encode = ash::khr::video_encode_queue::Device::new(&frame.instance, &frame.device);
            let mut hevc_profile = vk::VideoEncodeH265ProfileInfoKHR::default()
                .std_profile_idc(native::StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN);
            let profile = vk::VideoProfileInfoKHR::default()
                .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H265)
                .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
                .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .push_next(&mut hevc_profile);

            let mut hevc_capabilities = vk::VideoEncodeH265CapabilitiesKHR::default();
            let mut encode_capabilities = vk::VideoEncodeCapabilitiesKHR::default();
            let mut capabilities = vk::VideoCapabilitiesKHR::default()
                .push_next(&mut hevc_capabilities)
                .push_next(&mut encode_capabilities);
            vk_result((video_instance
                .fp()
                .get_physical_device_video_capabilities_khr)(
                frame.physical_device,
                &profile,
                &mut capabilities,
            ))
            .map_err(|error| format!("query HEVC capabilities: {error}"))?;
            let extent = vk::Extent2D { width, height };
            if width < capabilities.min_coded_extent.width
                || height < capabilities.min_coded_extent.height
                || width > capabilities.max_coded_extent.width
                || height > capabilities.max_coded_extent.height
            {
                return Err("frame extent is outside Vulkan Video HEVC capabilities".into());
            }
            let bitstream_alignment = capabilities.min_bitstream_buffer_size_alignment.max(1);
            let bitstream_offset_alignment =
                capabilities.min_bitstream_buffer_offset_alignment.max(1);
            let std_header_version = capabilities.std_header_version;
            if !encode_capabilities
                .supported_encode_feedback_flags
                .contains(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN)
            {
                return Err("driver cannot report Vulkan Video HEVC byte counts".into());
            }
            let rate_control_mode = preferred_rate_control(&encode_capabilities);

            let profiles = [profile];
            let mut profile_list = vk::VideoProfileListInfoKHR::default().profiles(&profiles);
            let format_info = vk::PhysicalDeviceVideoFormatInfoKHR::default()
                .image_usage(
                    vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR | vk::ImageUsageFlags::TRANSFER_DST,
                )
                .push_next(&mut profile_list);
            let mut format_count = 0;
            vk_result((video_instance
                .fp()
                .get_physical_device_video_format_properties_khr)(
                frame.physical_device,
                &format_info,
                &mut format_count,
                std::ptr::null_mut(),
            ))
            .map_err(|error| format!("count HEVC input formats: {error}"))?;
            if format_count == 0 {
                return Err("driver exposes no HEVC encode input format".into());
            }
            let mut formats = vec![vk::VideoFormatPropertiesKHR::default(); format_count as usize];
            vk_result((video_instance
                .fp()
                .get_physical_device_video_format_properties_khr)(
                frame.physical_device,
                &format_info,
                &mut format_count,
                formats.as_mut_ptr(),
            ))
            .map_err(|error| format!("query HEVC input formats: {error}"))?;
            let input_format = formats
                .iter()
                .find(|properties| properties.format == vk::Format::G8_B8R8_2PLANE_420_UNORM)
                .ok_or("driver does not expose two-plane NV12 HEVC input")?
                .format;

            let mut hevc_session = vk::VideoEncodeH265SessionCreateInfoKHR::default()
                .use_max_level_idc(true)
                .max_level_idc(native::StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_5_1);
            let session_info = vk::VideoSessionCreateInfoKHR::default()
                .queue_family_index(frame.queue_family)
                .video_profile(&profile)
                .picture_format(input_format)
                .max_coded_extent(extent)
                .reference_picture_format(input_format)
                .max_dpb_slots(1)
                .max_active_reference_pictures(0)
                .std_header_version(&std_header_version)
                .push_next(&mut hevc_session);
            let mut session = vk::VideoSessionKHR::null();
            vk_result((video.fp().create_video_session_khr)(
                frame.device.handle(),
                &session_info,
                std::ptr::null(),
                &mut session,
            ))
            .map_err(|error| format!("create HEVC video session: {error}"))?;
            let session_memory = bind_session_memory(frame, &video, session)?;

            let blocks = HevcBlockSizes::from_capabilities(&hevc_capabilities)?;
            let (profile_tier, mut vps, mut sps, pps) =
                hevc_parameter_structures(width, height, blocks);
            let dec_pic_buf: native::StdVideoH265DecPicBufMgr = std::mem::zeroed();
            vps.pProfileTierLevel = &raw const profile_tier;
            vps.pDecPicBufMgr = &raw const dec_pic_buf;
            sps.pProfileTierLevel = &raw const profile_tier;
            sps.pDecPicBufMgr = &raw const dec_pic_buf;
            let vps_list = [vps];
            let sps_list = [sps];
            let pps_list = [pps];
            let add = vk::VideoEncodeH265SessionParametersAddInfoKHR::default()
                .std_vp_ss(&vps_list)
                .std_sp_ss(&sps_list)
                .std_pp_ss(&pps_list);
            let mut hevc_parameters = vk::VideoEncodeH265SessionParametersCreateInfoKHR::default()
                .max_std_vps_count(1)
                .max_std_sps_count(1)
                .max_std_pps_count(1)
                .parameters_add_info(&add);
            let parameters_info = vk::VideoSessionParametersCreateInfoKHR::default()
                .video_session(session)
                .push_next(&mut hevc_parameters);
            let mut parameters = vk::VideoSessionParametersKHR::null();
            vk_result((video.fp().create_video_session_parameters_khr)(
                frame.device.handle(),
                &parameters_info,
                std::ptr::null(),
                &mut parameters,
            ))
            .map_err(|error| format!("create HEVC session parameters: {error}"))?;
            let parameter_sets = encoded_hevc_parameter_sets(&encode, &frame.device, parameters)
                .map_err(|error| format!("encode HEVC parameter sets: {error}"))?;
            let input = create_video_image(
                frame,
                width,
                height,
                input_format,
                vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR | vk::ImageUsageFlags::TRANSFER_DST,
                &profiles,
            )
            .map_err(|error| format!("create HEVC input image: {error}"))?;
            let dpb = create_video_image(
                frame,
                width,
                height,
                input_format,
                vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR,
                &profiles,
            )
            .map_err(|error| format!("create HEVC reconstructed image: {error}"))?;
            let bitstream_offset =
                align_up(parameter_sets.len() as u64, bitstream_offset_alignment);
            let bitstream_size = align_up(
                bitstream_offset + width as u64 * height as u64 * 3 + 64 * 1024,
                bitstream_alignment,
            );
            let bitstream = create_video_buffer(frame, bitstream_size, &profiles)
                .map_err(|error| format!("create HEVC bitstream buffer: {error}"))?;
            let mut feedback = vk::QueryPoolVideoEncodeFeedbackCreateInfoKHR::default()
                .encode_feedback_flags(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN);
            let mut query_profile = profile;
            let query_info = vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR)
                .query_count(1)
                .push_next(&mut query_profile)
                .push_next(&mut feedback);
            let query_pool = frame.device.create_query_pool(&query_info, None)?;
            let command_pool = frame.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(frame.queue_family),
                None,
            )?;
            let command = frame.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            let fence = frame.device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?;

            Ok(Self {
                codec: Codec::Hevc,
                dpb: Some(dpb),
                device: frame.device.clone(),
                video,
                encode,
                session,
                parameters,
                session_memory,
                input,
                parameter_sets,
                queue: frame.queue,
                command_pool,
                command,
                fence,
                fence_in_flight: false,
                bitstream,
                bitstream_offset,
                query_pool,
                width,
                height,
                bitrate: (bitrate as u64).min(encode_capabilities.max_bitrate),
                frame_rate: frame_rate.round().max(1.0) as u32,
                rate_control_mode,
                initialized: false,
            })
        }
    }

    fn encode_frame(
        &mut self,
        device: &ash::Device,
        frame: &mut VulkanFrame,
        conversion_done: vk::Semaphore,
        conversion_value: u64,
    ) -> Result<Vec<u8>, Error> {
        if self.codec == Codec::Hevc {
            return self.encode_hevc_frame(device, frame, conversion_done, conversion_value);
        }
        unsafe {
            if self.fence_in_flight {
                wait_for_fence(device, self.fence, "previous Vulkan video encode")?;
                self.fence_in_flight = false;
            }
            device.reset_fences(&[self.fence])?;
            device.reset_command_pool(self.command_pool, vk::CommandPoolResetFlags::empty())?;
            let mapped = device.map_memory(
                self.bitstream.memory,
                0,
                self.bitstream.size,
                vk::MemoryMapFlags::empty(),
            )?;
            std::ptr::copy_nonoverlapping(
                self.parameter_sets.as_ptr(),
                mapped.cast::<u8>(),
                self.parameter_sets.len(),
            );
            device.unmap_memory(self.bitstream.memory);

            device.begin_command_buffer(
                self.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            device.cmd_reset_query_pool(self.command, self.query_pool, 0, 1);
            let mut h264_rate_control = vk::VideoEncodeH264RateControlInfoKHR::default()
                .gop_frame_count(1)
                .idr_period(1)
                .consecutive_b_frame_count(0)
                .temporal_layer_count(1);
            let mut h264_rate_control_layer = vk::VideoEncodeH264RateControlLayerInfoKHR::default();
            let rate_control_layers = [vk::VideoEncodeRateControlLayerInfoKHR::default()
                .average_bitrate(self.bitrate)
                .max_bitrate(self.bitrate)
                .frame_rate_numerator(self.frame_rate)
                .frame_rate_denominator(1)
                .push_next(&mut h264_rate_control_layer)];
            let mut rate_control = vk::VideoEncodeRateControlInfoKHR::default()
                .rate_control_mode(self.rate_control_mode)
                .layers(
                    if self.rate_control_mode == vk::VideoEncodeRateControlModeFlagsKHR::DISABLED {
                        &[]
                    } else {
                        &rate_control_layers
                    },
                )
                .virtual_buffer_size_in_ms(1000)
                .initial_virtual_buffer_size_in_ms(500);
            let mut begin = vk::VideoBeginCodingInfoKHR::default()
                .video_session(self.session)
                .video_session_parameters(self.parameters);
            if self.initialized {
                begin = begin
                    .push_next(&mut rate_control)
                    .push_next(&mut h264_rate_control);
            }
            (self.video.fp().cmd_begin_video_coding_khr)(self.command, &begin);
            if !self.initialized {
                let control_flags = vk::VideoCodingControlFlagsKHR::RESET
                    | vk::VideoCodingControlFlagsKHR::ENCODE_RATE_CONTROL;
                let control = vk::VideoCodingControlInfoKHR::default()
                    .flags(control_flags)
                    .push_next(&mut rate_control)
                    .push_next(&mut h264_rate_control);
                (self.video.fp().cmd_control_video_coding_khr)(self.command, &control);
            }
            device.cmd_begin_query(
                self.command,
                self.query_pool,
                0,
                vk::QueryControlFlags::empty(),
            );

            let mut picture: native::StdVideoEncodeH264PictureInfo = std::mem::zeroed();
            picture.flags.set_IdrPicFlag(1);
            picture.seq_parameter_set_id = 0;
            picture.pic_parameter_set_id = 0;
            picture.idr_pic_id = 0;
            picture.primary_pic_type =
                native::StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR;
            let mut slice: native::StdVideoEncodeH264SliceHeader = std::mem::zeroed();
            slice.slice_type = native::StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_I;
            slice.cabac_init_idc = native::StdVideoH264CabacInitIdc_STD_VIDEO_H264_CABAC_INIT_IDC_0;
            slice.disable_deblocking_filter_idc = native::StdVideoH264DisableDeblockingFilterIdc_STD_VIDEO_H264_DISABLE_DEBLOCKING_FILTER_IDC_DISABLED;
            let constant_qp =
                if self.rate_control_mode == vk::VideoEncodeRateControlModeFlagsKHR::DISABLED {
                    26
                } else {
                    0
                };
            let slices = [vk::VideoEncodeH264NaluSliceInfoKHR::default()
                .constant_qp(constant_qp)
                .std_slice_header(&slice)];
            let mut h264_picture = vk::VideoEncodeH264PictureInfoKHR::default()
                .nalu_slice_entries(&slices)
                .std_picture_info(&picture);
            let picture_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_extent(vk::Extent2D {
                    width: self.width,
                    height: self.height,
                })
                .base_array_layer(0)
                .image_view_binding(self.input.view);
            let encode_info = vk::VideoEncodeInfoKHR::default()
                .dst_buffer(self.bitstream.buffer)
                .dst_buffer_offset(self.bitstream_offset)
                .dst_buffer_range(self.bitstream.size - self.bitstream_offset)
                .src_picture_resource(picture_resource)
                .preceding_externally_encoded_bytes(self.parameter_sets.len() as u32)
                .push_next(&mut h264_picture);
            (self.encode.fp().cmd_encode_video_khr)(self.command, &encode_info);
            device.cmd_end_query(self.command, self.query_pool, 0);
            let end = vk::VideoEndCodingInfoKHR::default();
            (self.video.fp().cmd_end_video_coding_khr)(self.command, &end);
            device.end_command_buffer(self.command)?;

            let waits = [conversion_done];
            let wait_values = [conversion_value];
            let stages = [vk::PipelineStageFlags::ALL_COMMANDS];
            let signals = [frame.completion];
            let signal_values = [frame.completion_value];
            let commands = [self.command];
            let mut timeline = vk::TimelineSemaphoreSubmitInfo::default()
                .wait_semaphore_values(&wait_values)
                .signal_semaphore_values(&signal_values);
            let submit = vk::SubmitInfo::default()
                .push_next(&mut timeline)
                .wait_semaphores(&waits)
                .wait_dst_stage_mask(&stages)
                .command_buffers(&commands)
                .signal_semaphores(&signals);
            device.queue_submit(self.queue, &[submit], self.fence)?;
            self.fence_in_flight = true;
            frame.mark_completion_submitted();
            if !self.initialized {
                tracing::debug!(
                    "polymer: Vulkan Video Encode waiting for conversion timeline value {conversion_value}"
                );
            }
            if let Err(error) = wait_for_fence(device, self.fence, "Vulkan Video Encode") {
                // Do not leave the compositor waiting forever if the driver
                // accepted an encode submission but never completes it.
                let _ = device.signal_semaphore(
                    &vk::SemaphoreSignalInfo::default()
                        .semaphore(frame.completion)
                        .value(frame.completion_value),
                );
                return Err(error);
            }
            self.fence_in_flight = false;
            if !self.initialized {
                tracing::debug!("polymer: Vulkan Video Encode completed");
            }
            self.initialized = true;

            let mut written = [0u64];
            device.get_query_pool_results(
                self.query_pool,
                0,
                &mut written,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
            )?;
            let encoded_size = written[0] as usize;
            let encoded_end = (self.bitstream_offset as usize)
                .checked_add(encoded_size)
                .ok_or("Vulkan encoded size overflow")?;
            if encoded_size == 0 || encoded_end > self.bitstream.size as usize {
                return Err("Vulkan encoder returned an invalid bitstream size".into());
            }
            let mapped = device.map_memory(
                self.bitstream.memory,
                0,
                encoded_end as u64,
                vk::MemoryMapFlags::empty(),
            )?;
            let mut bytes = Vec::with_capacity(self.parameter_sets.len() + encoded_size);
            bytes.extend_from_slice(&self.parameter_sets);
            bytes.extend_from_slice(std::slice::from_raw_parts(
                mapped.cast::<u8>().add(self.bitstream_offset as usize),
                encoded_size,
            ));
            device.unmap_memory(self.bitstream.memory);
            Ok(bytes)
        }
    }

    fn encode_hevc_frame(
        &mut self,
        device: &ash::Device,
        frame: &mut VulkanFrame,
        conversion_done: vk::Semaphore,
        conversion_value: u64,
    ) -> Result<Vec<u8>, Error> {
        unsafe {
            if self.fence_in_flight {
                wait_for_fence(device, self.fence, "previous Vulkan HEVC encode")?;
                self.fence_in_flight = false;
            }
            device.reset_fences(&[self.fence])?;
            device.reset_command_pool(self.command_pool, vk::CommandPoolResetFlags::empty())?;
            let mapped = device.map_memory(
                self.bitstream.memory,
                0,
                self.bitstream.size,
                vk::MemoryMapFlags::empty(),
            )?;
            std::ptr::copy_nonoverlapping(
                self.parameter_sets.as_ptr(),
                mapped.cast::<u8>(),
                self.parameter_sets.len(),
            );
            device.unmap_memory(self.bitstream.memory);
            device.begin_command_buffer(
                self.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            device.cmd_reset_query_pool(self.command, self.query_pool, 0, 1);
            let dpb = self.dpb.as_ref().expect("HEVC encoder has a DPB image");
            // The reconstructed picture is rewritten from scratch every frame
            // (IDR-only stream), so the previous contents can be discarded.
            let dpb_barrier = image_barrier()
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::VIDEO_ENCODE_DPB_KHR)
                .image(dpb.image)
                .subresource_range(color_range());
            device.cmd_pipeline_barrier(
                self.command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[dpb_barrier],
            );
            let dpb_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_extent(vk::Extent2D {
                    width: self.width,
                    height: self.height,
                })
                .image_view_binding(dpb.view);
            let mut hevc_rate_control = vk::VideoEncodeH265RateControlInfoKHR::default()
                .gop_frame_count(1)
                .idr_period(1)
                .consecutive_b_frame_count(0)
                .sub_layer_count(1);
            let mut hevc_rate_control_layer = vk::VideoEncodeH265RateControlLayerInfoKHR::default();
            let rate_control_layers = [vk::VideoEncodeRateControlLayerInfoKHR::default()
                .average_bitrate(self.bitrate)
                .max_bitrate(self.bitrate)
                .frame_rate_numerator(self.frame_rate)
                .frame_rate_denominator(1)
                .push_next(&mut hevc_rate_control_layer)];
            let mut rate_control = vk::VideoEncodeRateControlInfoKHR::default()
                .rate_control_mode(self.rate_control_mode)
                .layers(
                    if self.rate_control_mode == vk::VideoEncodeRateControlModeFlagsKHR::DISABLED {
                        &[]
                    } else {
                        &rate_control_layers
                    },
                )
                .virtual_buffer_size_in_ms(1000)
                .initial_virtual_buffer_size_in_ms(500);
            // Bind the DPB resource without an active slot: every frame is an
            // IDR that re-activates slot 0 through the setup reference slot.
            let begin_slots = [vk::VideoReferenceSlotInfoKHR::default()
                .slot_index(-1)
                .picture_resource(&dpb_resource)];
            let mut begin = vk::VideoBeginCodingInfoKHR::default()
                .video_session(self.session)
                .video_session_parameters(self.parameters)
                .reference_slots(&begin_slots);
            if self.initialized {
                begin = begin
                    .push_next(&mut rate_control)
                    .push_next(&mut hevc_rate_control);
            }
            (self.video.fp().cmd_begin_video_coding_khr)(self.command, &begin);
            if !self.initialized {
                let control = vk::VideoCodingControlInfoKHR::default()
                    .flags(
                        vk::VideoCodingControlFlagsKHR::RESET
                            | vk::VideoCodingControlFlagsKHR::ENCODE_RATE_CONTROL,
                    )
                    .push_next(&mut rate_control)
                    .push_next(&mut hevc_rate_control);
                (self.video.fp().cmd_control_video_coding_khr)(self.command, &control);
            }
            device.cmd_begin_query(
                self.command,
                self.query_pool,
                0,
                vk::QueryControlFlags::empty(),
            );
            let mut picture: native::StdVideoEncodeH265PictureInfo = std::mem::zeroed();
            picture.flags.set_IrapPicFlag(1);
            picture.flags.set_is_reference(1);
            picture.pic_type = native::StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_IDR;
            let mut slice: native::StdVideoEncodeH265SliceSegmentHeader = std::mem::zeroed();
            slice.flags.set_first_slice_segment_in_pic_flag(1);
            slice.slice_type = native::StdVideoH265SliceType_STD_VIDEO_H265_SLICE_TYPE_I;
            slice.MaxNumMergeCand = 5;
            let constant_qp =
                if self.rate_control_mode == vk::VideoEncodeRateControlModeFlagsKHR::DISABLED {
                    26
                } else {
                    0
                };
            let slices = [vk::VideoEncodeH265NaluSliceSegmentInfoKHR::default()
                .constant_qp(constant_qp)
                .std_slice_segment_header(&slice)];
            let mut hevc_picture = vk::VideoEncodeH265PictureInfoKHR::default()
                .nalu_slice_segment_entries(&slices)
                .std_picture_info(&picture);
            let picture_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_extent(vk::Extent2D {
                    width: self.width,
                    height: self.height,
                })
                .image_view_binding(self.input.view);
            let mut reference: native::StdVideoEncodeH265ReferenceInfo = std::mem::zeroed();
            reference.pic_type = native::StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_IDR;
            let mut dpb_slot =
                vk::VideoEncodeH265DpbSlotInfoKHR::default().std_reference_info(&reference);
            let setup_slot = vk::VideoReferenceSlotInfoKHR::default()
                .slot_index(0)
                .picture_resource(&dpb_resource)
                .push_next(&mut dpb_slot);
            let encode_info = vk::VideoEncodeInfoKHR::default()
                .dst_buffer(self.bitstream.buffer)
                .dst_buffer_offset(self.bitstream_offset)
                .dst_buffer_range(self.bitstream.size - self.bitstream_offset)
                .src_picture_resource(picture_resource)
                .setup_reference_slot(&setup_slot)
                .preceding_externally_encoded_bytes(self.parameter_sets.len() as u32)
                .push_next(&mut hevc_picture);
            (self.encode.fp().cmd_encode_video_khr)(self.command, &encode_info);
            device.cmd_end_query(self.command, self.query_pool, 0);
            (self.video.fp().cmd_end_video_coding_khr)(
                self.command,
                &vk::VideoEndCodingInfoKHR::default(),
            );
            device.end_command_buffer(self.command)?;
            let waits = [conversion_done];
            let wait_values = [conversion_value];
            let stages = [vk::PipelineStageFlags::ALL_COMMANDS];
            let signals = [frame.completion];
            let signal_values = [frame.completion_value];
            let commands = [self.command];
            let mut timeline = vk::TimelineSemaphoreSubmitInfo::default()
                .wait_semaphore_values(&wait_values)
                .signal_semaphore_values(&signal_values);
            let submit = vk::SubmitInfo::default()
                .push_next(&mut timeline)
                .wait_semaphores(&waits)
                .wait_dst_stage_mask(&stages)
                .command_buffers(&commands)
                .signal_semaphores(&signals);
            if !self.initialized {
                tracing::debug!("polymer: Vulkan HEVC encode submitting first frame");
            }
            device.queue_submit(self.queue, &[submit], self.fence)?;
            self.fence_in_flight = true;
            frame.mark_completion_submitted();
            if let Err(error) = wait_for_fence(device, self.fence, "Vulkan HEVC Encode") {
                let _ = device.signal_semaphore(
                    &vk::SemaphoreSignalInfo::default()
                        .semaphore(frame.completion)
                        .value(frame.completion_value),
                );
                return Err(error);
            }
            self.fence_in_flight = false;
            if !self.initialized {
                tracing::debug!("polymer: Vulkan HEVC encode first frame completed");
            }
            self.initialized = true;
            // The encode fence has signaled, so the query must be resolvable.
            // WITH_STATUS instead of WAIT: a failed encode operation leaves
            // the feedback query unavailable and WAIT would block forever.
            let mut written = [[0u64; 2]];
            device.get_query_pool_results(
                self.query_pool,
                0,
                &mut written,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WITH_STATUS_KHR,
            )?;
            let status = written[0][1] as i64;
            if status <= 0 {
                return Err(format!("Vulkan HEVC encode feedback status {status}").into());
            }
            let encoded_size = written[0][0] as usize;
            let encoded_end = (self.bitstream_offset as usize)
                .checked_add(encoded_size)
                .ok_or("Vulkan HEVC encoded size overflow")?;
            if encoded_size == 0 || encoded_end > self.bitstream.size as usize {
                return Err("Vulkan HEVC encoder returned an invalid bitstream size".into());
            }
            let mapped = device.map_memory(
                self.bitstream.memory,
                0,
                encoded_end as u64,
                vk::MemoryMapFlags::empty(),
            )?;
            let mut bytes = Vec::with_capacity(self.parameter_sets.len() + encoded_size);
            bytes.extend_from_slice(&self.parameter_sets);
            bytes.extend_from_slice(std::slice::from_raw_parts(
                mapped.cast::<u8>().add(self.bitstream_offset as usize),
                encoded_size,
            ));
            device.unmap_memory(self.bitstream.memory);
            Ok(bytes)
        }
    }
}

fn preferred_rate_control(
    capabilities: &vk::VideoEncodeCapabilitiesKHR<'_>,
) -> vk::VideoEncodeRateControlModeFlagsKHR {
    if capabilities
        .rate_control_modes
        .contains(vk::VideoEncodeRateControlModeFlagsKHR::CBR)
    {
        vk::VideoEncodeRateControlModeFlagsKHR::CBR
    } else if capabilities
        .rate_control_modes
        .contains(vk::VideoEncodeRateControlModeFlagsKHR::VBR)
    {
        vk::VideoEncodeRateControlModeFlagsKHR::VBR
    } else {
        vk::VideoEncodeRateControlModeFlagsKHR::DISABLED
    }
}

unsafe fn bind_session_memory(
    frame: &VulkanFrame,
    video: &ash::khr::video_queue::Device,
    session: vk::VideoSessionKHR,
) -> Result<Vec<vk::DeviceMemory>, Error> {
    unsafe {
        let mut count = 0;
        vk_result((video.fp().get_video_session_memory_requirements_khr)(
            frame.device.handle(),
            session,
            &mut count,
            std::ptr::null_mut(),
        ))?;
        let mut requirements =
            vec![vk::VideoSessionMemoryRequirementsKHR::default(); count as usize];
        vk_result((video.fp().get_video_session_memory_requirements_khr)(
            frame.device.handle(),
            session,
            &mut count,
            requirements.as_mut_ptr(),
        ))?;
        let properties = frame
            .instance
            .get_physical_device_memory_properties(frame.physical_device);
        let mut memories = Vec::with_capacity(requirements.len());
        let mut bindings = Vec::with_capacity(requirements.len());
        for requirement in requirements {
            let memory_type = find_memory_type(
                &properties,
                requirement.memory_requirements.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .or_else(|_| {
                find_memory_type(
                    &properties,
                    requirement.memory_requirements.memory_type_bits,
                    vk::MemoryPropertyFlags::empty(),
                )
            })?;
            let memory = frame.device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirement.memory_requirements.size)
                    .memory_type_index(memory_type),
                None,
            )?;
            bindings.push(
                vk::BindVideoSessionMemoryInfoKHR::default()
                    .memory_bind_index(requirement.memory_bind_index)
                    .memory(memory)
                    .memory_size(requirement.memory_requirements.size),
            );
            memories.push(memory);
        }
        vk_result((video.fp().bind_video_session_memory_khr)(
            frame.device.handle(),
            session,
            bindings.len() as u32,
            bindings.as_ptr(),
        ))?;
        Ok(memories)
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

impl HevcBlockSizes {
    fn from_capabilities(capabilities: &vk::VideoEncodeH265CapabilitiesKHR) -> Result<Self, Error> {
        let ctb_log2 = if capabilities
            .ctb_sizes
            .contains(vk::VideoEncodeH265CtbSizeFlagsKHR::TYPE_64)
        {
            6
        } else if capabilities
            .ctb_sizes
            .contains(vk::VideoEncodeH265CtbSizeFlagsKHR::TYPE_32)
        {
            5
        } else if capabilities
            .ctb_sizes
            .contains(vk::VideoEncodeH265CtbSizeFlagsKHR::TYPE_16)
        {
            4
        } else {
            return Err("driver reports no HEVC coding tree block size".into());
        };
        let tu_flags = [
            (vk::VideoEncodeH265TransformBlockSizeFlagsKHR::TYPE_4, 2),
            (vk::VideoEncodeH265TransformBlockSizeFlagsKHR::TYPE_8, 3),
            (vk::VideoEncodeH265TransformBlockSizeFlagsKHR::TYPE_16, 4),
            (vk::VideoEncodeH265TransformBlockSizeFlagsKHR::TYPE_32, 5),
        ];
        let min_tu_log2 = tu_flags
            .iter()
            .find(|(flag, _)| capabilities.transform_block_sizes.contains(*flag))
            .map(|(_, log2)| *log2)
            .ok_or("driver reports no HEVC transform block size")?;
        let max_tu_log2 = tu_flags
            .iter()
            .rev()
            .find(|(flag, _)| capabilities.transform_block_sizes.contains(*flag))
            .map(|(_, log2)| *log2)
            .ok_or("driver reports no HEVC transform block size")?;
        Ok(Self {
            ctb_log2,
            min_tu_log2,
            max_tu_log2,
        })
    }
}

fn hevc_parameter_structures(
    width: u32,
    height: u32,
    blocks: HevcBlockSizes,
) -> (
    native::StdVideoH265ProfileTierLevel,
    native::StdVideoH265VideoParameterSet,
    native::StdVideoH265SequenceParameterSet,
    native::StdVideoH265PictureParameterSet,
) {
    let mut profile: native::StdVideoH265ProfileTierLevel = unsafe { std::mem::zeroed() };
    profile.flags.set_general_progressive_source_flag(1);
    profile.flags.set_general_frame_only_constraint_flag(1);
    profile.general_profile_idc = native::StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN;
    profile.general_level_idc = native::StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_5_1;

    let mut vps: native::StdVideoH265VideoParameterSet = unsafe { std::mem::zeroed() };
    vps.flags.set_vps_temporal_id_nesting_flag(1);

    // pic_width/height_in_luma_samples must be multiples of MinCbSizeY (8);
    // the conformance window crops the alignment padding back off.
    let coded_width = width.div_ceil(8) * 8;
    let coded_height = height.div_ceil(8) * 8;
    let mut sps: native::StdVideoH265SequenceParameterSet = unsafe { std::mem::zeroed() };
    sps.flags.set_sps_temporal_id_nesting_flag(1);
    sps.flags.set_strong_intra_smoothing_enabled_flag(1);
    sps.chroma_format_idc =
        native::StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_420;
    sps.pic_width_in_luma_samples = coded_width;
    sps.pic_height_in_luma_samples = coded_height;
    if coded_width != width || coded_height != height {
        sps.flags.set_conformance_window_flag(1);
        sps.conf_win_right_offset = (coded_width - width) / 2;
        sps.conf_win_bottom_offset = (coded_height - height) / 2;
    }
    sps.log2_max_pic_order_cnt_lsb_minus4 = 4;
    sps.log2_min_luma_coding_block_size_minus3 = 0;
    sps.log2_diff_max_min_luma_coding_block_size = (blocks.ctb_log2 - 3) as u8;
    sps.log2_min_luma_transform_block_size_minus2 = (blocks.min_tu_log2 - 2) as u8;
    sps.log2_diff_max_min_luma_transform_block_size =
        (blocks.max_tu_log2 - blocks.min_tu_log2) as u8;
    sps.max_transform_hierarchy_depth_inter = (blocks.ctb_log2 - blocks.min_tu_log2).min(3) as u8;
    sps.max_transform_hierarchy_depth_intra = (blocks.ctb_log2 - blocks.min_tu_log2).min(3) as u8;

    let mut pps: native::StdVideoH265PictureParameterSet = unsafe { std::mem::zeroed() };
    pps.flags.set_deblocking_filter_control_present_flag(1);
    pps.log2_parallel_merge_level_minus2 = 0;
    (profile, vps, sps, pps)
}

fn h264_parameter_structures(
    width: u32,
    height: u32,
) -> (
    native::StdVideoH264SequenceParameterSet,
    native::StdVideoH264PictureParameterSet,
) {
    let coded_width = width.div_ceil(16) * 16;
    let coded_height = height.div_ceil(16) * 16;
    let mut sps: native::StdVideoH264SequenceParameterSet = unsafe { std::mem::zeroed() };
    sps.flags.set_frame_mbs_only_flag(1);
    sps.flags.set_direct_8x8_inference_flag(1);
    if coded_width != width || coded_height != height {
        sps.flags.set_frame_cropping_flag(1);
    }
    sps.profile_idc = native::StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE;
    sps.level_idc = native::StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_1;
    sps.chroma_format_idc =
        native::StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420;
    sps.log2_max_frame_num_minus4 = 0;
    sps.pic_order_cnt_type = native::StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_0;
    sps.log2_max_pic_order_cnt_lsb_minus4 = 0;
    sps.max_num_ref_frames = 0;
    sps.pic_width_in_mbs_minus1 = coded_width / 16 - 1;
    sps.pic_height_in_map_units_minus1 = coded_height / 16 - 1;
    sps.frame_crop_right_offset = (coded_width - width) / 2;
    sps.frame_crop_bottom_offset = (coded_height - height) / 2;

    let mut pps: native::StdVideoH264PictureParameterSet = unsafe { std::mem::zeroed() };
    pps.flags.set_entropy_coding_mode_flag(0);
    pps.flags.set_deblocking_filter_control_present_flag(1);
    (sps, pps)
}

unsafe fn encoded_parameter_sets(
    encode: &ash::khr::video_encode_queue::Device,
    device: &ash::Device,
    parameters: vk::VideoSessionParametersKHR,
) -> Result<Vec<u8>, Error> {
    unsafe {
        let mut h264_get = vk::VideoEncodeH264SessionParametersGetInfoKHR::default()
            .write_std_sps(true)
            .write_std_pps(true)
            .std_sps_id(0)
            .std_pps_id(0);
        let get = vk::VideoEncodeSessionParametersGetInfoKHR::default()
            .video_session_parameters(parameters)
            .push_next(&mut h264_get);
        let mut size = 0;
        vk_result((encode.fp().get_encoded_video_session_parameters_khr)(
            device.handle(),
            &get,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
        ))?;
        let mut bytes = vec![0; size];
        vk_result((encode.fp().get_encoded_video_session_parameters_khr)(
            device.handle(),
            &get,
            std::ptr::null_mut(),
            &mut size,
            bytes.as_mut_ptr().cast(),
        ))?;
        bytes.truncate(size);
        Ok(bytes)
    }
}

unsafe fn encoded_hevc_parameter_sets(
    encode: &ash::khr::video_encode_queue::Device,
    device: &ash::Device,
    parameters: vk::VideoSessionParametersKHR,
) -> Result<Vec<u8>, Error> {
    unsafe {
        let mut hevc_get = vk::VideoEncodeH265SessionParametersGetInfoKHR::default()
            .write_std_vps(true)
            .write_std_sps(true)
            .write_std_pps(true)
            .std_vps_id(0)
            .std_sps_id(0)
            .std_pps_id(0);
        let get = vk::VideoEncodeSessionParametersGetInfoKHR::default()
            .video_session_parameters(parameters)
            .push_next(&mut hevc_get);
        let mut size = 0;
        vk_result((encode.fp().get_encoded_video_session_parameters_khr)(
            device.handle(),
            &get,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
        ))?;
        let mut bytes = vec![0; size];
        vk_result((encode.fp().get_encoded_video_session_parameters_khr)(
            device.handle(),
            &get,
            std::ptr::null_mut(),
            &mut size,
            bytes.as_mut_ptr().cast(),
        ))?;
        bytes.truncate(size);
        Ok(bytes)
    }
}

unsafe fn create_video_image(
    frame: &VulkanFrame,
    width: u32,
    height: u32,
    format: vk::Format,
    usage: vk::ImageUsageFlags,
    profiles: &[vk::VideoProfileInfoKHR<'_>],
) -> Result<Image, Error> {
    unsafe {
        let families = [frame.conversion_queue_family, frame.queue_family];
        let mut profile_list = vk::VideoProfileListInfoKHR::default().profiles(profiles);
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .sharing_mode(vk::SharingMode::CONCURRENT)
            .queue_family_indices(&families)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut profile_list);
        let image = frame.device.create_image(&info, None)?;
        let requirements = frame.device.get_image_memory_requirements(image);
        let properties = frame
            .instance
            .get_physical_device_memory_properties(frame.physical_device);
        let memory_type = find_memory_type(
            &properties,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )?;
        let memory = frame.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type),
            None,
        )?;
        frame.device.bind_image_memory(image, memory, 0)?;
        let view = frame.device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .subresource_range(color_range()),
            None,
        )?;
        Ok(Image {
            image,
            memory,
            view,
        })
    }
}

unsafe fn create_video_buffer(
    frame: &VulkanFrame,
    size: u64,
    profiles: &[vk::VideoProfileInfoKHR<'_>],
) -> Result<Buffer, Error> {
    unsafe {
        let mut profile_list = vk::VideoProfileListInfoKHR::default().profiles(profiles);
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(vk::BufferUsageFlags::VIDEO_ENCODE_DST_KHR)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut profile_list);
        let buffer = frame.device.create_buffer(&info, None)?;
        let requirements = frame.device.get_buffer_memory_requirements(buffer);
        let properties = frame
            .instance
            .get_physical_device_memory_properties(frame.physical_device);
        let memory_type = find_memory_type(
            &properties,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )?;
        let memory = frame.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type),
            None,
        )?;
        frame.device.bind_buffer_memory(buffer, memory, 0)?;
        Ok(Buffer {
            buffer,
            memory,
            size,
        })
    }
}

fn find_memory_type(
    properties: &vk::PhysicalDeviceMemoryProperties,
    bits: u32,
    flags: vk::MemoryPropertyFlags,
) -> Result<u32, vk::Result> {
    properties.memory_types[..properties.memory_type_count as usize]
        .iter()
        .enumerate()
        .find(|(index, memory)| bits & (1 << index) != 0 && memory.property_flags.contains(flags))
        .map(|(index, _)| index as u32)
        .ok_or(vk::Result::ERROR_FEATURE_NOT_PRESENT)
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

fn align_up(value: u64, alignment: u64) -> u64 {
    value.div_ceil(alignment) * alignment
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

unsafe fn create_image(
    instance: &ash::Instance,
    device: &ash::Device,
    physical_device: vk::PhysicalDevice,
    width: u32,
    height: u32,
    format: vk::Format,
    usage: vk::ImageUsageFlags,
) -> Result<Image, Error> {
    unsafe {
        let image = device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(usage)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        )?;
        let requirements = device.get_image_memory_requirements(image);
        let properties = instance.get_physical_device_memory_properties(physical_device);
        let memory_type = properties.memory_types[..properties.memory_type_count as usize]
            .iter()
            .enumerate()
            .find(|(index, memory)| {
                requirements.memory_type_bits & (1 << index) != 0
                    && memory
                        .property_flags
                        .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            })
            .map(|(index, _)| index as u32)
            .ok_or(vk::Result::ERROR_FEATURE_NOT_PRESENT)?;
        let memory = device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type),
            None,
        )?;
        device.bind_image_memory(image, memory, 0)?;
        let view = device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .subresource_range(color_range()),
            None,
        )?;
        Ok(Image {
            image,
            memory,
            view,
        })
    }
}

unsafe fn destroy_image(device: &ash::Device, image: &Image) {
    unsafe {
        device.destroy_image_view(image.view, None);
        device.destroy_image(image.image, None);
        device.free_memory(image.memory, None);
    }
}

unsafe fn destroy_buffer(device: &ash::Device, buffer: &Buffer) {
    unsafe {
        device.destroy_buffer(buffer.buffer, None);
        device.free_memory(buffer.memory, None);
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

unsafe fn copy_plane(
    device: &ash::Device,
    command: vk::CommandBuffer,
    source: vk::Image,
    destination: vk::Image,
    destination_aspect: vk::ImageAspectFlags,
    width: u32,
    height: u32,
) {
    unsafe {
        device.cmd_copy_image(
            command,
            source,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            destination,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[vk::ImageCopy::default()
                .src_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .dst_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: destination_aspect,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })],
        );
    }
}

pub(super) fn readback_rgba(
    frame: &mut VulkanFrame,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, Error> {
    unsafe {
        let device = frame.device.clone();
        let size = width as u64 * height as u64 * 4;
        let buffer = device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(size)
                .usage(vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE),
            None,
        )?;
        let requirements = device.get_buffer_memory_requirements(buffer);
        let properties = frame
            .instance
            .get_physical_device_memory_properties(frame.physical_device);
        let memory_type = properties.memory_types[..properties.memory_type_count as usize]
            .iter()
            .enumerate()
            .find(|(index, memory)| {
                requirements.memory_type_bits & (1 << index) != 0
                    && memory.property_flags.contains(
                        vk::MemoryPropertyFlags::HOST_VISIBLE
                            | vk::MemoryPropertyFlags::HOST_COHERENT,
                    )
            })
            .map(|(index, _)| index as u32)
            .ok_or(vk::Result::ERROR_FEATURE_NOT_PRESENT)?;
        let memory = device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type),
            None,
        )?;
        device.bind_buffer_memory(buffer, memory, 0)?;
        let pool = device.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                .queue_family_index(frame.conversion_queue_family),
            None,
        )?;
        let command = device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )?[0];
        device.begin_command_buffer(
            command,
            &vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;
        device.cmd_copy_image_to_buffer(
            command,
            frame.image,
            frame.layout,
            buffer,
            &[vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })],
        );
        device.end_command_buffer(command)?;
        let commands = [command];
        let waits = [frame.ready];
        let wait_values = [frame.ready_value];
        let signals = [frame.completion];
        let signal_values = [frame.completion_value];
        let stages = [vk::PipelineStageFlags::TRANSFER];
        let mut timeline = vk::TimelineSemaphoreSubmitInfo::default()
            .wait_semaphore_values(&wait_values)
            .signal_semaphore_values(&signal_values);
        let submit = vk::SubmitInfo::default()
            .push_next(&mut timeline)
            .wait_semaphores(&waits)
            .wait_dst_stage_mask(&stages)
            .command_buffers(&commands)
            .signal_semaphores(&signals);
        let fence = device.create_fence(&vk::FenceCreateInfo::default(), None)?;
        device.queue_submit(frame.conversion_queue, &[submit], fence)?;
        frame.mark_completion_submitted();
        device.wait_for_fences(&[fence], true, u64::MAX)?;

        let mapped = device.map_memory(memory, 0, size, vk::MemoryMapFlags::empty())?;
        let bytes = std::slice::from_raw_parts(mapped.cast::<u8>(), size as usize);
        let rgba = pixels_to_rgba(frame.format, bytes)?;
        device.unmap_memory(memory);
        device.destroy_fence(fence, None);
        device.destroy_command_pool(pool, None);
        device.destroy_buffer(buffer, None);
        device.free_memory(memory, None);
        Ok(rgba)
    }
}

fn pixels_to_rgba(format: vk::Format, packed: &[u8]) -> Result<Vec<u8>, Error> {
    if format == vk::Format::A2B10G10R10_UNORM_PACK32 {
        let mut output = Vec::with_capacity(packed.len());
        for bytes in packed.chunks_exact(4) {
            let pixel = u32::from_le_bytes(bytes.try_into().expect("four-byte pixel"));
            output.extend_from_slice(&[
                ((pixel & 0x3ff) * 255 / 1023) as u8,
                (((pixel >> 10) & 0x3ff) * 255 / 1023) as u8,
                (((pixel >> 20) & 0x3ff) * 255 / 1023) as u8,
                255,
            ]);
        }
        return Ok(output);
    }
    if !matches!(
        format,
        vk::Format::R8G8B8A8_UNORM
            | vk::Format::R8G8B8A8_SRGB
            | vk::Format::B8G8R8A8_UNORM
            | vk::Format::B8G8R8A8_SRGB
    ) {
        return Err(format!(
            "OpenH264 fallback does not support Vulkan format {}",
            format.as_raw()
        )
        .into());
    }
    if matches!(
        format,
        vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB
    ) {
        let mut output = Vec::with_capacity(packed.len());
        for pixel in packed.chunks_exact(4) {
            output.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
        Ok(output)
    } else {
        Ok(packed.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h264_parameters_crop_to_the_requested_extent() {
        let (sps, _) = h264_parameter_structures(1920, 1080);
        assert_eq!(sps.pic_width_in_mbs_minus1, 119);
        assert_eq!(sps.pic_height_in_map_units_minus1, 67);
        assert_eq!(sps.frame_crop_right_offset, 0);
        assert_eq!(sps.frame_crop_bottom_offset, 4);
    }

    #[test]
    fn hevc_parameters_describe_main_profile_extent() {
        let blocks = HevcBlockSizes {
            ctb_log2: 5,
            min_tu_log2: 2,
            max_tu_log2: 5,
        };
        let (profile, _, sps, pps) = hevc_parameter_structures(1920, 1080, blocks);
        assert_eq!(
            profile.general_profile_idc,
            native::StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN
        );
        assert_eq!(sps.pic_width_in_luma_samples, 1920);
        assert_eq!(sps.pic_height_in_luma_samples, 1080);
        assert_eq!(sps.log2_diff_max_min_luma_coding_block_size, 2);
        assert_eq!(pps.pps_seq_parameter_set_id, 0);
    }

    #[test]
    fn hevc_parameters_align_and_crop_odd_extents() {
        let blocks = HevcBlockSizes {
            ctb_log2: 5,
            min_tu_log2: 2,
            max_tu_log2: 5,
        };
        let (_, _, sps, _) = hevc_parameter_structures(626, 632, blocks);
        assert_eq!(sps.pic_width_in_luma_samples, 632);
        assert_eq!(sps.pic_height_in_luma_samples, 632);
        assert_eq!(sps.flags.conformance_window_flag(), 1);
        assert_eq!(sps.conf_win_right_offset, 3);
        assert_eq!(sps.conf_win_bottom_offset, 0);
    }
}
