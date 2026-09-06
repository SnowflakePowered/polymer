use ash::vk;

pub(super) struct WsiImage {
    device: ash::Device,
    pub(super) image: vk::Image,
    memory: vk::DeviceMemory,
    upload: vk::Buffer,
    upload_memory: vk::DeviceMemory,
    crop: vk::Extent2D,
}

impl WsiImage {
    unsafe fn allocate(
        device: &ash::Device,
        properties: &vk::PhysicalDeviceMemoryProperties,
        requirements: vk::MemoryRequirements,
        flags: vk::MemoryPropertyFlags,
    ) -> vk::DeviceMemory {
        let index = (0..properties.memory_type_count)
            .find(|index| {
                requirements.memory_type_bits & (1 << index) != 0
                    && properties.memory_types[*index as usize]
                        .property_flags
                        .contains(flags)
            })
            .unwrap();
        unsafe {
            device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(index),
                None,
            )
        }
        .unwrap()
    }

    pub(super) unsafe fn new(
        device: &ash::Device,
        properties: &vk::PhysicalDeviceMemoryProperties,
        format: vk::Format,
        red: bool,
    ) -> Self {
        unsafe {
            let image = device
                .create_image(
                    &vk::ImageCreateInfo::default()
                        .image_type(vk::ImageType::TYPE_2D)
                        .format(format)
                        .extent(vk::Extent3D {
                            width: 64,
                            height: 64,
                            depth: 1,
                        })
                        .mip_levels(1)
                        .array_layers(1)
                        .samples(vk::SampleCountFlags::TYPE_1)
                        .tiling(vk::ImageTiling::OPTIMAL)
                        .usage(
                            vk::ImageUsageFlags::TRANSFER_SRC
                                | vk::ImageUsageFlags::TRANSFER_DST
                                | vk::ImageUsageFlags::COLOR_ATTACHMENT,
                        ),
                    None,
                )
                .unwrap();
            let memory = Self::allocate(
                device,
                properties,
                device.get_image_memory_requirements(image),
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            );
            device.bind_image_memory(image, memory, 0).unwrap();
            let upload = device
                .create_buffer(
                    &vk::BufferCreateInfo::default()
                        .size(64 * 64 * 4)
                        .usage(vk::BufferUsageFlags::TRANSFER_SRC),
                    None,
                )
                .unwrap();
            let upload_memory = Self::allocate(
                device,
                properties,
                device.get_buffer_memory_requirements(upload),
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            );
            device.bind_buffer_memory(upload, upload_memory, 0).unwrap();
            let crop = if red {
                vk::Extent2D {
                    width: 32,
                    height: 48,
                }
            } else {
                vk::Extent2D {
                    width: 24,
                    height: 32,
                }
            };
            let bgra = matches!(
                format,
                vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB
            );
            let blue = if bgra {
                [255u8, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
            let color = if !red {
                [0, 255, 0, 255]
            } else if bgra {
                [0, 0, 255, 255]
            } else {
                [255, 0, 0, 255]
            };
            let mut pixels = vec![blue; 64 * 64];
            for y in 8..8 + crop.height {
                for x in 4..4 + crop.width {
                    pixels[(y * 64 + x) as usize] = color;
                }
            }
            let mapped = device
                .map_memory(upload_memory, 0, 64 * 64 * 4, vk::MemoryMapFlags::empty())
                .unwrap();
            std::ptr::copy_nonoverlapping(
                pixels.as_ptr().cast::<u8>(),
                mapped.cast::<u8>(),
                64 * 64 * 4,
            );
            device.unmap_memory(upload_memory);
            Self {
                device: device.clone(),
                image,
                memory,
                upload,
                upload_memory,
                crop,
            }
        }
    }

    pub(super) unsafe fn blit(
        &self,
        command: vk::CommandBuffer,
        destination: vk::Image,
        extent: vk::Extent2D,
    ) {
        unsafe {
            let range = vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1);
            let layers = vk::ImageSubresourceLayers::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .layer_count(1);
            let mut barrier = vk::ImageMemoryBarrier::default()
                .image(self.image)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .subresource_range(range);
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
            self.device.cmd_copy_buffer_to_image(
                command,
                self.upload,
                self.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::BufferImageCopy::default()
                    .image_subresource(layers)
                    .image_extent(vk::Extent3D {
                        width: 64,
                        height: 64,
                        depth: 1,
                    })],
            );
            barrier.old_layout = vk::ImageLayout::TRANSFER_DST_OPTIMAL;
            barrier.new_layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;
            barrier.src_access_mask = vk::AccessFlags::TRANSFER_WRITE;
            barrier.dst_access_mask = vk::AccessFlags::TRANSFER_READ;
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
            self.device.cmd_blit_image(
                command,
                self.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                destination,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::ImageBlit::default()
                    .src_subresource(layers)
                    .dst_subresource(layers)
                    .src_offsets([
                        vk::Offset3D { x: 4, y: 8, z: 0 },
                        vk::Offset3D {
                            x: 4 + self.crop.width as i32,
                            y: 8 + self.crop.height as i32,
                            z: 1,
                        },
                    ])
                    .dst_offsets([
                        vk::Offset3D::default(),
                        vk::Offset3D {
                            x: extent.width as i32,
                            y: extent.height as i32,
                            z: 1,
                        },
                    ])],
                vk::Filter::NEAREST,
            );
        }
    }
}

impl Drop for WsiImage {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_image(self.image, None);
            self.device.free_memory(self.memory, None);
            self.device.destroy_buffer(self.upload, None);
            self.device.free_memory(self.upload_memory, None);
        }
    }
}
