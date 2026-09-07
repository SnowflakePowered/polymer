use super::*;
impl Driver {
    pub(in crate::virtual_swapchain::gpu_tests) fn paint(&self, image: vk::Image, method: u8) {
        self.paint_synced(
            image,
            method,
            vk::ImageLayout::PRESENT_SRC_KHR,
            vk::Semaphore::null(),
            vk::Semaphore::null(),
        );
    }
    pub(in crate::virtual_swapchain::gpu_tests) fn paint_synced(
        &self,
        image: vk::Image,
        method: u8,
        initial: vk::ImageLayout,
        acquire: vk::Semaphore,
        rendered: vk::Semaphore,
    ) {
        unsafe {
            let pool = self
                .native
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(self.owner.acquire_queue.family),
                    None,
                )
                .unwrap();
            let command = self
                .native
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .unwrap()[0];
            self.native
                .begin_command_buffer(command, &vk::CommandBufferBeginInfo::default())
                .unwrap();
            let resources = if method == 1 {
                Some(self.render_pass(command, image))
            } else if method == 3 {
                Some(self.render_pass2(command, image))
            } else {
                let range = vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1);
                let transition = vk::ImageMemoryBarrier::default()
                    .image(image)
                    .old_layout(initial)
                    .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .subresource_range(range);
                self.barrier(command, method, &transition);

                self.native.cmd_clear_color_image(
                    command,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &vk::ClearColorValue {
                        float32: [1.0, 0.0, 0.0, 1.0],
                    },
                    &[range],
                );
                let transition = vk::ImageMemoryBarrier {
                    old_layout: vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    new_layout: vk::ImageLayout::PRESENT_SRC_KHR,
                    src_access_mask: vk::AccessFlags::TRANSFER_WRITE,
                    dst_access_mask: vk::AccessFlags::empty(),
                    ..transition
                };
                self.barrier(command, method, &transition);
                None
            };
            self.native.end_command_buffer(command).unwrap();
            let fence = self.native.create_fence(&Default::default(), None).unwrap();
            let commands = [command];
            let waits = (acquire != vk::Semaphore::null())
                .then_some(acquire)
                .into_iter()
                .collect::<Vec<_>>();
            let stages = vec![vk::PipelineStageFlags::ALL_COMMANDS; waits.len()];
            let signals = (rendered != vk::Semaphore::null())
                .then_some(rendered)
                .into_iter()
                .collect::<Vec<_>>();
            let info = vk::SubmitInfo::default()
                .command_buffers(&commands)
                .wait_semaphores(&waits)
                .wait_dst_stage_mask(&stages)
                .signal_semaphores(&signals);
            assert_eq!(
                queues::submit(self.owner.acquire_queue.handle, 1, &info, fence),
                vk::Result::SUCCESS
            );
            self.native
                .wait_for_fences(&[fence], true, 2_000_000_000)
                .unwrap();
            self.native.destroy_fence(fence, None);
            self.native.destroy_command_pool(pool, None);
            if let Some((pass, framebuffer, view)) = resources {
                self.hook::<vk::PFN_vkDestroyFramebuffer>(b"vkDestroyFramebuffer")(
                    self.device(),
                    framebuffer,
                    std::ptr::null(),
                );
                self.hook::<vk::PFN_vkDestroyImageView>(b"vkDestroyImageView")(
                    self.device(),
                    view,
                    std::ptr::null(),
                );
                self.hook::<vk::PFN_vkDestroyRenderPass>(b"vkDestroyRenderPass")(
                    self.device(),
                    pass,
                    std::ptr::null(),
                );
            }
        }
    }
    unsafe fn barrier(
        &self,
        command: vk::CommandBuffer,
        method: u8,
        barrier: &vk::ImageMemoryBarrier<'_>,
    ) {
        unsafe {
            if method == 2 {
                let images = [vk::ImageMemoryBarrier2::default()
                    .image(barrier.image)
                    .old_layout(barrier.old_layout)
                    .new_layout(barrier.new_layout)
                    .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .src_access_mask(vk::AccessFlags2::from_raw(
                        barrier.src_access_mask.as_raw() as u64
                    ))
                    .dst_access_mask(vk::AccessFlags2::from_raw(
                        barrier.dst_access_mask.as_raw() as u64
                    ))
                    .src_queue_family_index(barrier.src_queue_family_index)
                    .dst_queue_family_index(barrier.dst_queue_family_index)
                    .subresource_range(barrier.subresource_range)];
                self.hook::<vk::PFN_vkCmdPipelineBarrier2>(b"vkCmdPipelineBarrier2KHR")(
                    command,
                    &vk::DependencyInfo::default().image_memory_barriers(&images),
                );
            } else {
                self.hook::<vk::PFN_vkCmdPipelineBarrier>(b"vkCmdPipelineBarrier")(
                    command,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    1,
                    barrier,
                );
            }
        }
    }
    pub(in crate::virtual_swapchain::gpu_tests) fn present(
        &self,
        swapchain: vk::SwapchainKHR,
        index: u32,
    ) {
        let queue = self
            .owner
            .queues
            .values()
            .find(|queue| {
                queue.family == self.owner.acquire_queue.family
                    && queue.handle != self.owner.acquire_queue.handle
            })
            .unwrap_or(&self.owner.acquire_queue);
        let semaphore = unsafe {
            self.native
                .create_semaphore(&Default::default(), None)
                .unwrap()
        };
        let waits = [semaphore];
        let signal = vk::SubmitInfo::default().signal_semaphores(&waits);
        let handles = [swapchain];
        let indices = [index];
        let mut results = [vk::Result::ERROR_UNKNOWN];
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&waits)
            .swapchains(&handles)
            .image_indices(&indices)
            .results(&mut results);
        unsafe {
            assert_eq!(
                queues::submit(
                    self.owner.acquire_queue.handle,
                    1,
                    &signal,
                    vk::Fence::null()
                ),
                vk::Result::SUCCESS
            );
            assert_eq!(present::present(queue.handle, &info), vk::Result::SUCCESS);
            queue.drain().unwrap();
            self.native.destroy_semaphore(semaphore, None);
        }
        assert_eq!(results, [vk::Result::SUCCESS]);
    }
}
