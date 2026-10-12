use super::*;

impl VulkanCompositor {
    pub fn present_with(
        &mut self,
        plan: &CompositingPlan,
        record: impl FnOnce(&mut VulkanRenderPass<'_>) -> Result<(), Box<dyn Error>>,
    ) -> Result<bool, Box<dyn Error>> {
        // SAFETY: frame fences protect command and semaphore reuse; acquisition
        // owns the destination image; the external timeline gates source reads.
        unsafe {
            let flight = self.frame_index as usize % FRAMES_IN_FLIGHT;
            self.device
                .wait_for_fences(&[self.fences[flight]], true, u64::MAX)?;

            let (image_index, acquire_suboptimal) = match self.swapchain_loader.acquire_next_image(
                self.swapchain,
                u64::MAX,
                self.image_available[flight],
                vk::Fence::null(),
            ) {
                Ok(acquired) => acquired,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return Ok(false),
                Err(error) => return Err(error.into()),
            };
            let command = self.command_buffers[flight];
            self.device
                .reset_command_buffer(command, vk::CommandBufferResetFlags::empty())?;
            self.device.begin_command_buffer(
                command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            let destination = self.swapchain_images[image_index as usize];
            let old_destination_layout = if self.swapchain_initialized[image_index as usize] {
                vk::ImageLayout::PRESENT_SRC_KHR
            } else {
                vk::ImageLayout::UNDEFINED
            };
            let to_destination = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(old_destination_layout)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .image(destination)
                .subresource_range(color_range());
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_destination],
            );
            {
                self.device.cmd_clear_color_image(
                    command,
                    destination,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &vk::ClearColorValue {
                        float32: plan.clear_color(),
                    },
                    &[color_range()],
                );
            }
            let mut surface_draws = Vec::new();
            for surface in self.surfaces.iter_mut() {
                if surface.frames.has_pending() {
                    surface.release_displayed(&self.device, command, self.queue_family);
                    surface.copy_single_slot(&self.device, command, self.queue_family);
                }
                let imported = &surface.imported;
                if surface.frames.frame().is_none() {
                    continue;
                }
                if let Some(source) = surface.image() {
                    let mut to_source = vk::ImageMemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                        .old_layout(vk::ImageLayout::GENERAL)
                        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(source)
                        .subresource_range(color_range());
                    if surface.cache.is_none()
                        && surface.frames.has_pending()
                        && imported.uses_external_queue_family()
                    {
                        to_source = to_source
                            .src_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
                            .dst_queue_family_index(self.queue_family);
                    }
                    self.device.cmd_pipeline_barrier(
                        command,
                        vk::PipelineStageFlags::ALL_COMMANDS,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &[to_source],
                    );
                    let placement = plan
                        .placements()
                        .find(|placement| {
                            placement.connection == surface.connection
                                && placement.surface_id == surface.surface_id
                        })
                        .copied();
                    let descriptor_set = surface.descriptor();
                    let drawable = placement.zip(descriptor_set).filter(|(placement, _)| {
                        let content = placement.content;
                        content.opacity.is_finite()
                            && content.opacity > 0.0
                            && content.destination[2] > content.destination[0]
                            && content.destination[3] > content.destination[1]
                    });
                    if let Some((placement, descriptor_set)) = drawable {
                        let source_to_shader = vk::ImageMemoryBarrier::default()
                            .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                            .dst_access_mask(vk::AccessFlags::SHADER_READ)
                            .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                            .image(source)
                            .subresource_range(color_range());
                        self.device.cmd_pipeline_barrier(
                            command,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::PipelineStageFlags::FRAGMENT_SHADER,
                            vk::DependencyFlags::empty(),
                            &[],
                            &[],
                            &[source_to_shader],
                        );
                        surface_draws.push(SurfaceDraw {
                            image: source,
                            descriptor_set,
                            placement,
                            push: SurfacePush {
                                source: placement.content.source,
                                active_uv: [1.0, 1.0],
                                opacity: placement.content.opacity.clamp(0.0, 1.0),
                                transform: u32::from(surface.quarter_turns)
                                    | if surface.flip_y { 0x100 } else { 0 },
                            },
                        });
                    } else {
                        let source_to_general = vk::ImageMemoryBarrier::default()
                            .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                            .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                            .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                            .new_layout(vk::ImageLayout::GENERAL)
                            .image(source)
                            .subresource_range(color_range());
                        self.device.cmd_pipeline_barrier(
                            command,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::PipelineStageFlags::ALL_COMMANDS,
                            vk::DependencyFlags::empty(),
                            &[],
                            &[],
                            &[source_to_general],
                        );
                    }
                }
            }
            let to_color = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .image(destination)
                .subresource_range(color_range());
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_color],
            );
            self.device.cmd_begin_render_pass(
                command,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(self.render_pass)
                    .framebuffer(self.framebuffers[image_index as usize])
                    .render_area(vk::Rect2D {
                        offset: vk::Offset2D::default(),
                        extent: self.extent,
                    }),
                vk::SubpassContents::INLINE,
            );
            draw_surface_content(
                &self.device,
                command,
                self.extent,
                &self.composition_pipeline,
                plan,
                &surface_draws,
            );
            record(&mut VulkanRenderPass {
                device: &self.device,
                command_buffer: command,
                extent: self.extent,
                frame_index: flight,
            })?;
            self.device.cmd_end_render_pass(command);
            for draw in &surface_draws {
                let to_general = vk::ImageMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::SHADER_READ)
                    .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                    .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .image(draw.image)
                    .subresource_range(color_range());
                self.device.cmd_pipeline_barrier(
                    command,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[to_general],
                );
            }
            let to_present = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .dst_access_mask(vk::AccessFlags::empty())
                .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .image(destination)
                .subresource_range(color_range());
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_present],
            );
            self.device.end_command_buffer(command)?;

            let waits = [self.image_available[flight]];
            let wait_values = [0];
            let wait_stages = [vk::PipelineStageFlags::TRANSFER];
            let commands = [command];
            // Presentation may continue using its wait semaphore after the
            // render fence signals. Indexing by acquired image guarantees that
            // the image has returned from presentation before reusing it.
            let render_complete = self.render_finished[image_index as usize];
            let mut signals = vec![render_complete];
            let mut signal_values = vec![0];
            for surface in &self.surfaces {
                if let Some(value) = surface.frames.release_value(surface.retention())
                    && let Some(semaphore) = surface.imported.consumer_timeline()
                {
                    signals.push(semaphore);
                    signal_values.push(value);
                }
            }
            let mut timeline_submit = vk::TimelineSemaphoreSubmitInfo::default()
                .wait_semaphore_values(&wait_values)
                .signal_semaphore_values(&signal_values);
            let submit = vk::SubmitInfo::default()
                .push_next(&mut timeline_submit)
                .wait_semaphores(&waits)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&commands)
                .signal_semaphores(&signals);
            self.device.reset_fences(&[self.fences[flight]])?;
            self.device
                .queue_submit(self.queue, &[submit], self.fences[flight])?;
            for surface in &mut self.surfaces {
                surface.commit();
            }
            let swapchains = [self.swapchain];
            let indices = [image_index];
            let present_suboptimal = match self.swapchain_loader.queue_present(
                self.queue,
                &vk::PresentInfoKHR::default()
                    .wait_semaphores(&signals[..1])
                    .swapchains(&swapchains)
                    .image_indices(&indices),
            ) {
                Ok(suboptimal) => suboptimal,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => true,
                Err(error) => return Err(error.into()),
            };
            self.swapchain_initialized[image_index as usize] = true;
            self.frame_index += 1;
            Ok(!(acquire_suboptimal || present_suboptimal))
        }
    }

    pub fn present(&mut self, plan: &CompositingPlan) -> Result<bool, Box<dyn Error>> {
        self.present_with(plan, |_| Ok(()))
    }
}

pub(super) unsafe fn draw_surface_content(
    device: &ash::Device,
    command: vk::CommandBuffer,
    extent: vk::Extent2D,
    pipeline: &CompositionPipeline,
    plan: &CompositingPlan,
    draws: &[SurfaceDraw],
) {
    unsafe {
        device.cmd_set_scissor(
            command,
            0,
            &[vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent,
            }],
        );
        let mut blending = None;
        for placement in plan.placements() {
            let Some(draw) = draws.iter().find(|draw| {
                draw.placement.connection == placement.connection
                    && draw.placement.surface_id == placement.surface_id
            }) else {
                continue;
            };
            let content = draw.placement.content;
            let blend = content.opacity < 1.0;
            if blending != Some(blend) {
                blending = Some(blend);
                device.cmd_bind_pipeline(
                    command,
                    vk::PipelineBindPoint::GRAPHICS,
                    if blend {
                        pipeline.blend_pipeline
                    } else {
                        pipeline.opaque_pipeline
                    },
                );
            }
            device.cmd_set_viewport(
                command,
                0,
                &[vk::Viewport {
                    x: content.destination[0],
                    y: content.destination[1],
                    width: content.destination[2] - content.destination[0],
                    height: content.destination[3] - content.destination[1],
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            device.cmd_bind_descriptor_sets(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                pipeline.pipeline_layout,
                0,
                &[draw.descriptor_set],
                &[],
            );
            device.cmd_push_constants(
                command,
                pipeline.pipeline_layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                std::slice::from_raw_parts(
                    (&draw.push as *const SurfacePush).cast::<u8>(),
                    size_of::<SurfacePush>(),
                ),
            );
            device.cmd_draw(command, 6, 1, 0, 0);
        }
    }
}
