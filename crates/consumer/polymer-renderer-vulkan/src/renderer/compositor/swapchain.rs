use super::*;

impl VulkanCompositor {
    pub(crate) fn recreate_swapchain(
        &mut self,
        window: &impl RenderWindow,
    ) -> Result<bool, Box<dyn Error>> {
        let requested = window.render_extent();
        if requested.width == 0 || requested.height == 0 {
            return Ok(false);
        }
        // SAFETY: the device is idle before replacing every swapchain child;
        // the old swapchain remains live until the replacement is created.
        unsafe {
            self.device.device_wait_idle()?;
            let capabilities = self
                .surface_loader
                .get_physical_device_surface_capabilities(self.physical_device, self.surface)?;
            let extent = if capabilities.current_extent.width != u32::MAX {
                capabilities.current_extent
            } else {
                vk::Extent2D {
                    width: requested.width.clamp(
                        capabilities.min_image_extent.width,
                        capabilities.max_image_extent.width,
                    ),
                    height: requested.height.clamp(
                        capabilities.min_image_extent.height,
                        capabilities.max_image_extent.height,
                    ),
                }
            };
            if extent.width == 0 || extent.height == 0 {
                return Ok(false);
            }
            let image_count = capabilities.min_image_count;
            let replacement = self.swapchain_loader.create_swapchain(
                &vk::SwapchainCreateInfoKHR::default()
                    .surface(self.surface)
                    .min_image_count(image_count)
                    .image_format(self.surface_format.format)
                    .image_color_space(self.surface_format.color_space)
                    .image_extent(extent)
                    .image_array_layers(1)
                    .image_usage(
                        vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT,
                    )
                    .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .pre_transform(capabilities.current_transform)
                    .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                    .present_mode(vk::PresentModeKHR::FIFO)
                    .clipped(true)
                    .old_swapchain(self.swapchain),
                None,
            )?;
            let images = self.swapchain_loader.get_swapchain_images(replacement)?;
            let views = create_swapchain_views(&self.device, &images, self.surface_format.format)?;
            let framebuffers = create_framebuffers(&self.device, self.render_pass, &views, extent)?;
            let render_finished = images
                .iter()
                .map(|_| {
                    self.device
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                })
                .collect::<Result<Vec<_>, _>>()?;

            for framebuffer in self.framebuffers.drain(..) {
                self.device.destroy_framebuffer(framebuffer, None);
            }
            for view in self.swapchain_views.drain(..) {
                self.device.destroy_image_view(view, None);
            }
            for semaphore in self.render_finished.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
            }
            self.swapchain_loader
                .destroy_swapchain(self.swapchain, None);
            self.swapchain = replacement;
            self.swapchain_images = images;
            self.swapchain_views = views;
            self.framebuffers = framebuffers;
            self.render_finished = render_finished;
            self.swapchain_initialized = vec![false; self.swapchain_images.len()];
            self.extent = extent;
            Ok(true)
        }
    }
}

pub(super) unsafe fn create_swapchain_views(
    device: &ash::Device,
    images: &[vk::Image],
    format: vk::Format,
) -> Result<Vec<vk::ImageView>, vk::Result> {
    images
        .iter()
        .map(|&image| unsafe {
            device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(color_range()),
                None,
            )
        })
        .collect()
}

pub(super) unsafe fn create_framebuffers(
    device: &ash::Device,
    render_pass: vk::RenderPass,
    views: &[vk::ImageView],
    extent: vk::Extent2D,
) -> Result<Vec<vk::Framebuffer>, vk::Result> {
    views
        .iter()
        .map(|view| unsafe {
            device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(render_pass)
                    .attachments(std::slice::from_ref(view))
                    .width(extent.width)
                    .height(extent.height)
                    .layers(1),
                None,
            )
        })
        .collect()
}
