use ash::vk;
use polymer_consumer_vulkan::ImportedImageRing;
use polymer_protocol::FrameReady;

use super::compositor::color_range;
use polymer_renderer::renderer::frame_ownership::{Retention, SurfaceFrames};

pub(super) struct ImportedVulkanSurface {
    pub connection: u64,
    pub surface_id: u64,
    pub target_window: i64,
    pub imported: ImportedImageRing,
    pub sample_views: Vec<vk::ImageView>,
    pub sample_sets: Vec<vk::DescriptorSet>,
    pub flip_y: bool,
    pub quarter_turns: u8,
    pub frames: SurfaceFrames,
    pub cache: Option<SurfaceCache>,
}

impl ImportedVulkanSurface {
    pub fn offer(&mut self, frame: FrameReady) {
        if self.imported.image(frame.slot).is_none() {
            return;
        }
        self.frames.offer(frame);
    }

    /// A cached surface copies each frame out of the producer's slot.
    pub fn retention(&self) -> Retention {
        if self.cache.is_some() {
            Retention::Cached
        } else {
            Retention::Retained
        }
    }

    pub fn image(&self) -> Option<vk::Image> {
        let frame = self.frames.frame()?;
        self.cache
            .as_ref()
            .map(|cache| cache.image)
            .or_else(|| self.imported.image(frame.slot))
    }

    pub fn descriptor(&self) -> Option<vk::DescriptorSet> {
        let frame = self.frames.frame()?;
        if self.cache.is_some() {
            self.sample_sets.last().copied()
        } else {
            self.sample_sets.get(frame.slot as usize).copied()
        }
    }

    /// Record the old lease's queue-family handoff before acknowledging it.
    /// Retained images stay GENERAL and consumer-owned between redraws.
    pub fn release_displayed(&self, device: &ash::Device, command: vk::CommandBuffer, family: u32) {
        if self.cache.is_some() || !self.imported.uses_external_queue_family() {
            return;
        }
        if let Some(frame) = self.frames.displayed() {
            let barrier = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::MEMORY_READ)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(family)
                .dst_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
                .image(self.imported.image(frame.slot).expect("displayed slot"))
                .subresource_range(color_range());
            unsafe {
                device.cmd_pipeline_barrier(
                    command,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[barrier],
                )
            };
        }
    }

    pub fn copy_single_slot(&self, device: &ash::Device, command: vk::CommandBuffer, family: u32) {
        let (Some(cache), Some(frame)) = (&self.cache, self.frames.pending()) else {
            return;
        };
        let source = self.imported.image(frame.slot).expect("prepared slot");
        let mut acquire = vk::ImageMemoryBarrier::default()
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(source)
            .subresource_range(color_range());
        if self.imported.uses_external_queue_family() {
            acquire = acquire
                .src_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
                .dst_queue_family_index(family);
        }
        let destination = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::MEMORY_READ)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .old_layout(if cache.initialized {
                vk::ImageLayout::GENERAL
            } else {
                vk::ImageLayout::UNDEFINED
            })
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(cache.image)
            .subresource_range(color_range());
        let subresource = vk::ImageSubresourceLayers::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .layer_count(1);
        unsafe {
            device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[acquire, destination],
            );
            device.cmd_copy_image(
                command,
                source,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                cache.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::ImageCopy::default()
                    .src_subresource(subresource)
                    .dst_subresource(subresource)
                    .extent(vk::Extent3D {
                        width: self.imported.width,
                        height: self.imported.height,
                        depth: 1,
                    })],
            );
            let release = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(acquire.dst_queue_family_index)
                .dst_queue_family_index(acquire.src_queue_family_index)
                .image(source)
                .subresource_range(color_range());
            let cached = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(cache.image)
                .subresource_range(color_range());
            device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[release, cached],
            );
        }
    }

    pub fn commit(&mut self) {
        if self.frames.has_pending()
            && let Some(cache) = &mut self.cache
        {
            cache.initialized = true;
        }
        if let Some(ready) = self.frames.commit(self.retention()) {
            polymer_statistics::capture_latency(self.connection, self.surface_id, ready.elapsed());
        }
    }

    /// The caller has idled the device and supplies an unused command buffer.
    /// Return queue-family ownership before releasing the ring.
    pub fn retire(
        &self,
        device: &ash::Device,
        queue: vk::Queue,
        command: vk::CommandBuffer,
        family: u32,
    ) -> Result<(), vk::Result> {
        unsafe {
            device.reset_command_buffer(command, vk::CommandBufferResetFlags::empty())?;
            device.begin_command_buffer(
                command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            self.release_displayed(device, command, family);
            device.end_command_buffer(command)?;
            let commands = [command];
            let submit = vk::SubmitInfo::default().command_buffers(&commands);
            device.queue_submit(queue, &[submit], vk::Fence::null())?;
            device.queue_wait_idle(queue)?;
        }
        let last_received = self.frames.frame().map_or(0, |frame| frame.sync_value);
        self.imported.release_all_idle(last_received)
    }
}

/// Private persistence for producers with only one export slot.
pub(super) struct SurfaceCache {
    device: ash::Device,
    pub image: vk::Image,
    memory: vk::DeviceMemory,
    pub initialized: bool,
}

impl SurfaceCache {
    pub fn new(
        instance: &ash::Instance,
        device: &ash::Device,
        physical: vk::PhysicalDevice,
        imported: &ImportedImageRing,
    ) -> Result<Self, vk::Result> {
        let image = unsafe {
            device.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(imported.format)
                    .extent(vk::Extent3D {
                        width: imported.width,
                        height: imported.height,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(
                        vk::ImageUsageFlags::TRANSFER_DST
                            | vk::ImageUsageFlags::TRANSFER_SRC
                            | vk::ImageUsageFlags::SAMPLED,
                    )
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )?
        };
        let mut cache = Self {
            device: device.clone(),
            image,
            memory: vk::DeviceMemory::null(),
            initialized: false,
        };
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        let properties = unsafe { instance.get_physical_device_memory_properties(physical) };
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
        cache.memory = unsafe {
            device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(memory_type),
                None,
            )?
        };
        unsafe { device.bind_image_memory(image, cache.memory, 0)? };
        Ok(cache)
    }
}

impl Drop for SurfaceCache {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_image(self.image, None);
            self.device.free_memory(self.memory, None);
        }
    }
}
