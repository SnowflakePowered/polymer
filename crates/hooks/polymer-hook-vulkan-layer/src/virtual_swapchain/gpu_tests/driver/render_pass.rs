use super::*;
impl Driver {
    pub(super) unsafe fn render_pass(
        &self,
        command: vk::CommandBuffer,
        image: vk::Image,
    ) -> (vk::RenderPass, vk::Framebuffer, vk::ImageView) {
        unsafe {
            let attachments = [vk::AttachmentDescription::default()
                .format(vk::Format::R8G8B8A8_UNORM)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .initial_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .final_layout(vk::ImageLayout::PRESENT_SRC_KHR)];
            let color = [vk::AttachmentReference::default()
                .attachment(0)
                .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
            let subpasses = [vk::SubpassDescription::default()
                .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                .color_attachments(&color)];
            let mut pass = vk::RenderPass::null();
            assert_eq!(
                self.hook::<vk::PFN_vkCreateRenderPass>(b"vkCreateRenderPass")(
                    self.device(),
                    &vk::RenderPassCreateInfo::default()
                        .attachments(&attachments)
                        .subpasses(&subpasses),
                    std::ptr::null(),
                    &mut pass
                ),
                vk::Result::SUCCESS
            );
            let (framebuffer, view) = self.framebuffer(pass, image, false);
            let clears = [vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [1.0, 0.0, 0.0, 1.0],
                },
            }];
            let info = vk::RenderPassBeginInfo::default()
                .render_pass(pass)
                .framebuffer(framebuffer)
                .render_area(vk::Rect2D::default().extent(vk::Extent2D {
                    width: 16,
                    height: 16,
                }))
                .clear_values(&clears);
            self.hook::<vk::PFN_vkCmdBeginRenderPass>(b"vkCmdBeginRenderPass")(
                command,
                &info,
                vk::SubpassContents::INLINE,
            );
            self.native.cmd_end_render_pass(command);
            (pass, framebuffer, view)
        }
    }
    unsafe fn framebuffer(
        &self,
        pass: vk::RenderPass,
        image: vk::Image,
        imageless: bool,
    ) -> (vk::Framebuffer, vk::ImageView) {
        unsafe {
            let mut view = vk::ImageView::null();
            let range = vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(1);
            assert_eq!(
                self.hook::<vk::PFN_vkCreateImageView>(b"vkCreateImageView")(
                    self.device(),
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(vk::Format::R8G8B8A8_UNORM)
                        .subresource_range(range),
                    std::ptr::null(),
                    &mut view
                ),
                vk::Result::SUCCESS
            );
            let views = [view];
            let formats = [vk::Format::R8G8B8A8_UNORM];
            let images = [vk::FramebufferAttachmentImageInfo::default()
                .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST)
                .width(16)
                .height(16)
                .layer_count(1)
                .view_formats(&formats)];
            let mut attachments =
                vk::FramebufferAttachmentsCreateInfo::default().attachment_image_infos(&images);
            let mut info = vk::FramebufferCreateInfo::default()
                .render_pass(pass)
                .attachments(&views)
                .width(16)
                .height(16)
                .layers(1);
            if imageless {
                info = info
                    .flags(vk::FramebufferCreateFlags::IMAGELESS)
                    .push_next(&mut attachments);
                info.p_attachments = std::ptr::null();
            }
            let mut framebuffer = vk::Framebuffer::null();
            assert_eq!(
                self.hook::<vk::PFN_vkCreateFramebuffer>(b"vkCreateFramebuffer")(
                    self.device(),
                    &info,
                    std::ptr::null(),
                    &mut framebuffer
                ),
                vk::Result::SUCCESS
            );
            (framebuffer, view)
        }
    }
    pub(super) unsafe fn render_pass2(
        &self,
        command: vk::CommandBuffer,
        image: vk::Image,
    ) -> (vk::RenderPass, vk::Framebuffer, vk::ImageView) {
        unsafe {
            let attachments = [vk::AttachmentDescription2::default()
                .format(vk::Format::R8G8B8A8_UNORM)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .initial_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .final_layout(vk::ImageLayout::PRESENT_SRC_KHR)];
            let colors = [vk::AttachmentReference2::default()
                .attachment(0)
                .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .aspect_mask(vk::ImageAspectFlags::COLOR)];
            let subpasses = [vk::SubpassDescription2::default()
                .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                .color_attachments(&colors)];
            let info = vk::RenderPassCreateInfo2::default()
                .attachments(&attachments)
                .subpasses(&subpasses);
            let mut pass = vk::RenderPass::null();
            assert_eq!(
                self.hook::<vk::PFN_vkCreateRenderPass2>(b"vkCreateRenderPass2KHR")(
                    self.device(),
                    &info,
                    std::ptr::null(),
                    &mut pass
                ),
                vk::Result::SUCCESS
            );
            let (framebuffer, view) = self.framebuffer(pass, image, true);
            let views = [view];
            let mut attachments = vk::RenderPassAttachmentBeginInfo::default().attachments(&views);
            let clears = [vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [1.0, 0.0, 0.0, 1.0],
                },
            }];
            let begin = vk::RenderPassBeginInfo::default()
                .render_pass(pass)
                .framebuffer(framebuffer)
                .push_next(&mut attachments)
                .render_area(vk::Rect2D::default().extent(vk::Extent2D {
                    width: 16,
                    height: 16,
                }))
                .clear_values(&clears);
            self.hook::<vk::PFN_vkCmdBeginRenderPass2>(b"vkCmdBeginRenderPass2KHR")(
                command,
                &begin,
                &vk::SubpassBeginInfo::default().contents(vk::SubpassContents::INLINE),
            );
            self.native
                .cmd_end_render_pass2(command, &vk::SubpassEndInfo::default());
            (pass, framebuffer, view)
        }
    }
}
