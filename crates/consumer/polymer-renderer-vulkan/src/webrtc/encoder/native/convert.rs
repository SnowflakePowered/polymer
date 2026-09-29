use super::*;

impl Encoder {
    pub(super) fn convert(&mut self, frame: &VulkanFrame) -> Result<(), Error> {
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

pub(super) unsafe fn copy_plane(
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
