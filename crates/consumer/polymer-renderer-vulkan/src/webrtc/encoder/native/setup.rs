use super::*;

impl Encoder {
    pub(super) fn new_with_codec(
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
            let shader_words =
                read_spv(&mut Cursor::new(include_bytes!("../rgba_to_nv12.comp.spv")))?;
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
}

pub(super) unsafe fn create_image(
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

pub(super) unsafe fn destroy_image(device: &ash::Device, image: &Image) {
    unsafe {
        device.destroy_image_view(image.view, None);
        device.destroy_image(image.image, None);
        device.free_memory(image.memory, None);
    }
}

pub(super) unsafe fn destroy_buffer(device: &ash::Device, buffer: &Buffer) {
    unsafe {
        device.destroy_buffer(buffer.buffer, None);
        device.free_memory(buffer.memory, None);
    }
}

pub(super) unsafe fn create_video_image(
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

pub(super) unsafe fn create_video_buffer(
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

pub(super) fn find_memory_type(
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

pub(super) unsafe fn bind_session_memory(
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
