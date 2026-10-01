use super::*;

impl VulkanCompositor {
    pub(crate) fn import_stream(
        &mut self,
        connection: u64,
        stream: &StreamReady,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        if !self.mode.accepts(connection) {
            return Err(
                format!("connection {connection} does not belong to {:?}", self.mode).into(),
            );
        }
        self.disconnect_stream(connection)?;
        let imported = self.receiver.import_stream(stream)?;
        let quarter_turns = quarter_turns % 4;
        let cache = (stream.slot_count == 1)
            .then(|| {
                SurfaceCache::new(
                    &self.instance,
                    &self.device,
                    self.physical_device,
                    &imported,
                )
            })
            .transpose()?;
        let mut images: Vec<_> = (0..stream.slot_count)
            .map(|slot| imported.image(slot).expect("imported slot"))
            .collect();
        if let Some(cache) = &cache {
            images.push(cache.image);
        }
        let (sample_views, sample_sets) = unsafe {
            create_surface_samples(
                &self.device,
                &images,
                imported.format,
                &self.composition_pipeline,
            )?
        };
        self.surfaces.push(ImportedVulkanSurface {
            connection,
            surface_id: stream.surface_id.0,
            target_window: stream.source_window,
            imported,
            sample_views,
            sample_sets,
            flip_y: stream.bottom_up,
            quarter_turns,
            frames: Default::default(),
            cache,
        });
        self.surfaces
            .sort_by_key(|surface| surface_sort_key(surface.surface_id));
        Ok(())
    }

    pub(crate) fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        if let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.connection == connection)
        {
            surface.offer(frame);
        }
    }

    pub(crate) fn received_surfaces(&self) -> Vec<ReceivedSurface> {
        self.surfaces
            .iter()
            .filter_map(|surface| {
                let frame = surface.frames.frame()?;
                let image_extent = polymer_renderer::renderer::image_extent(
                    surface.imported.width,
                    surface.imported.height,
                    surface.quarter_turns,
                );
                Some(ReceivedSurface {
                    connection: surface.connection,
                    surface_id: surface.surface_id,
                    target_window: surface.target_window,
                    image_extent,
                    quarter_turns: surface.quarter_turns,
                    flip_y: surface.flip_y,
                    frame,
                })
            })
            .collect()
    }

    pub(crate) fn has_pending_frame(&self) -> bool {
        self.surfaces
            .iter()
            .any(|surface| surface.frames.has_pending())
    }

    pub(crate) fn disconnect_stream(&mut self, connection: u64) -> Result<(), vk::Result> {
        let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.connection == connection)
        else {
            return Ok(());
        };
        unsafe {
            self.device.device_wait_idle()?;
        }
        self.surfaces[index].retire(
            &self.device,
            self.queue,
            self.command_buffers[0],
            self.queue_family,
        )?;
        let mut surface = self.surfaces.remove(index);
        unsafe {
            destroy_surface_samples(
                &self.device,
                self.composition_pipeline.descriptor_pool,
                &mut surface.sample_views,
                &mut surface.sample_sets,
            );
        };
        Ok(())
    }

    pub(super) fn release_imported_streams(&mut self) -> Result<(), vk::Result> {
        unsafe {
            self.device.device_wait_idle()?;
        }
        for surface in &self.surfaces {
            surface.retire(
                &self.device,
                self.queue,
                self.command_buffers[0],
                self.queue_family,
            )?;
        }
        for mut surface in self.surfaces.drain(..) {
            unsafe {
                destroy_surface_samples(
                    &self.device,
                    self.composition_pipeline.descriptor_pool,
                    &mut surface.sample_views,
                    &mut surface.sample_sets,
                );
            };
        }
        Ok(())
    }
}

pub(super) unsafe fn create_surface_samples(
    device: &ash::Device,
    images: &[vk::Image],
    format: vk::Format,
    pipeline: &CompositionPipeline,
) -> Result<(Vec<vk::ImageView>, Vec<vk::DescriptorSet>), vk::Result> {
    unsafe {
        let mut views = Vec::with_capacity(images.len());
        for &image in images {
            match device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(color_range()),
                None,
            ) {
                Ok(view) => views.push(view),
                Err(error) => {
                    for view in views {
                        device.destroy_image_view(view, None);
                    }
                    return Err(error);
                }
            }
        }
        let layouts = vec![pipeline.descriptor_set_layout; views.len()];
        let sets = match device.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(pipeline.descriptor_pool)
                .set_layouts(&layouts),
        ) {
            Ok(sets) => sets,
            Err(error) => {
                for view in views {
                    device.destroy_image_view(view, None);
                }
                return Err(error);
            }
        };
        let images = views
            .iter()
            .map(|&view| {
                vk::DescriptorImageInfo::default()
                    .sampler(pipeline.sampler)
                    .image_view(view)
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            })
            .collect::<Vec<_>>();
        let writes = sets
            .iter()
            .zip(&images)
            .map(|(&set, image)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(std::slice::from_ref(image))
            })
            .collect::<Vec<_>>();
        device.update_descriptor_sets(&writes, &[]);
        Ok((views, sets))
    }
}

pub(super) unsafe fn destroy_surface_samples(
    device: &ash::Device,
    pool: vk::DescriptorPool,
    views: &mut Vec<vk::ImageView>,
    sets: &mut Vec<vk::DescriptorSet>,
) {
    unsafe {
        if !sets.is_empty() {
            let _ = device.free_descriptor_sets(pool, sets);
            sets.clear();
        }
        for view in views.drain(..) {
            device.destroy_image_view(view, None);
        }
    }
}

pub(crate) unsafe fn create_vulkan_video_cache(
    instance: &ash::Instance,
    device: &ash::Device,
    physical_device: vk::PhysicalDevice,
    width: u32,
    height: u32,
    format: vk::Format,
    graphics_family: u32,
    video_family: u32,
) -> Result<VulkanVideoCache, vk::Result> {
    let families = [graphics_family, video_family];
    let image = unsafe {
        device.create_image(
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
                .usage(
                    vk::ImageUsageFlags::TRANSFER_DST
                        | vk::ImageUsageFlags::TRANSFER_SRC
                        | vk::ImageUsageFlags::SAMPLED,
                )
                .sharing_mode(vk::SharingMode::CONCURRENT)
                .queue_family_indices(&families)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        )?
    };
    let requirements = unsafe { device.get_image_memory_requirements(image) };
    let properties = unsafe { instance.get_physical_device_memory_properties(physical_device) };
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
    let memory = unsafe {
        device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type),
            None,
        )?
    };
    unsafe { device.bind_image_memory(image, memory, 0)? };
    Ok(VulkanVideoCache {
        image,
        memory,
        initialized: false,
    })
}
