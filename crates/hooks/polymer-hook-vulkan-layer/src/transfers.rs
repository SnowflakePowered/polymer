//! Core/KHR transfer entry points normalize into the same producer recorder.
use crate::hooks::VulkanHookContext;
use ash::vk;
use polymer_producer_vulkan::Evidence;

impl VulkanHookContext {
    pub(crate) fn cmd_copy_image2(
        &self,
        command: vk::CommandBuffer,
        info: *const vk::CopyImageInfo2<'_>,
    ) {
        let Some(info) = (unsafe { info.as_ref() }) else {
            return;
        };
        if info.region_count == 0 || info.p_regions.is_null() {
            return;
        }
        let tracking = self.command_tracking(command);
        for region in
            unsafe { std::slice::from_raw_parts(info.p_regions, info.region_count as usize) }
        {
            tracking.command_image_transferred(
                command,
                info.src_image,
                info.src_image_layout,
                info.dst_image,
                &vk::ImageCopy {
                    src_subresource: region.src_subresource,
                    src_offset: region.src_offset,
                    dst_subresource: region.dst_subresource,
                    dst_offset: region.dst_offset,
                    extent: region.extent,
                },
                Evidence::Copy,
            );
        }
    }

    pub(crate) fn cmd_resolve_image2(
        &self,
        command: vk::CommandBuffer,
        info: *const vk::ResolveImageInfo2<'_>,
    ) {
        let Some(info) = (unsafe { info.as_ref() }) else {
            return;
        };
        if info.region_count == 0 || info.p_regions.is_null() {
            return;
        }
        let tracking = self.command_tracking(command);
        for region in
            unsafe { std::slice::from_raw_parts(info.p_regions, info.region_count as usize) }
        {
            tracking.command_image_transferred(
                command,
                info.src_image,
                info.src_image_layout,
                info.dst_image,
                &vk::ImageCopy {
                    src_subresource: region.src_subresource,
                    src_offset: region.src_offset,
                    dst_subresource: region.dst_subresource,
                    dst_offset: region.dst_offset,
                    extent: region.extent,
                },
                Evidence::Resolve,
            );
        }
    }

    pub(crate) fn cmd_blit_image2(
        &self,
        command: vk::CommandBuffer,
        info: *const vk::BlitImageInfo2<'_>,
    ) {
        let Some(info) = (unsafe { info.as_ref() }) else {
            return;
        };
        if info.region_count == 0 || info.p_regions.is_null() {
            return;
        }
        let tracking = self.command_tracking(command);
        for region in
            unsafe { std::slice::from_raw_parts(info.p_regions, info.region_count as usize) }
        {
            tracking.command_image_blitted(
                command,
                info.src_image,
                info.src_image_layout,
                info.dst_image,
                &vk::ImageBlit {
                    src_subresource: region.src_subresource,
                    src_offsets: region.src_offsets,
                    dst_subresource: region.dst_subresource,
                    dst_offsets: region.dst_offsets,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ash::vk::Handle;
    use std::sync::Arc;

    unsafe extern "system" fn no_device_proc(
        _: vk::Device,
        _: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction {
        None
    }

    #[test]
    fn transfer2_recording_requires_submission_and_preserves_crop_and_reflection() {
        let dispatch_key = Box::new(71usize);
        let command = vk::CommandBuffer::from_raw((&*dispatch_key as *const usize) as u64);
        let source = vk::Image::from_raw(1);
        let destination = vk::Image::from_raw(2);
        let layers = vk::ImageSubresourceLayers::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .layer_count(1);
        for operation in 0..3 {
            let state = Arc::new(crate::layer::VulkanHookState::default());
            let tracking = Arc::new(polymer_producer_vulkan::tracking::Tracking::default());
            state.device.insert(
                71,
                crate::layer::DeviceDispatchTable {
                    next_gdpa: no_device_proc,
                    physical_device: vk::PhysicalDevice::null(),
                    physical_state: None,
                    capture: None,
                    capture_state: Arc::default(),
                    tracking: tracking.clone(),
                },
            );
            let context = VulkanHookContext { state };
            tracking.image_created(
                source,
                vk::Extent2D {
                    width: 64,
                    height: 64,
                },
                vk::Format::R8G8B8A8_UNORM,
                vk::ImageUsageFlags::TRANSFER_SRC,
            );
            tracking.swapchain_image_added(
                destination,
                vk::Extent2D {
                    width: 64,
                    height: 64,
                },
            );
            let source_offset = vk::Offset3D { x: 8, y: 4, z: 0 };
            let extent = vk::Extent3D {
                width: 24,
                height: 32,
                depth: 1,
            };
            match operation {
                0 => context.cmd_copy_image2(
                    command,
                    &vk::CopyImageInfo2::default()
                        .src_image(source)
                        .src_image_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .dst_image(destination)
                        .regions(&[vk::ImageCopy2::default()
                            .src_subresource(layers)
                            .dst_subresource(layers)
                            .src_offset(source_offset)
                            .extent(extent)]),
                ),
                1 => context.cmd_resolve_image2(
                    command,
                    &vk::ResolveImageInfo2::default()
                        .src_image(source)
                        .src_image_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .dst_image(destination)
                        .regions(&[vk::ImageResolve2::default()
                            .src_subresource(layers)
                            .dst_subresource(layers)
                            .src_offset(source_offset)
                            .extent(extent)]),
                ),
                _ => context.cmd_blit_image2(
                    command,
                    &vk::BlitImageInfo2::default()
                        .src_image(source)
                        .src_image_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .dst_image(destination)
                        .regions(&[vk::ImageBlit2::default()
                            .src_subresource(layers)
                            .dst_subresource(layers)
                            .src_offsets([source_offset, vk::Offset3D { x: 32, y: 36, z: 1 }])
                            .dst_offsets([
                                vk::Offset3D { x: 24, y: 0, z: 0 },
                                vk::Offset3D { x: 0, y: 32, z: 1 },
                            ])]),
                ),
            }
            assert!(tracking.take_present_sources(destination, 1).is_empty());
            tracking.commands_submitted(&[command]);
            let selected = tracking.take_present_sources(destination, 1);
            assert_eq!(selected.len(), 1);
            assert_eq!(selected[0].image, source);
            assert_eq!(
                (
                    selected[0].selection.source_rect.x,
                    selected[0].selection.source_rect.y
                ),
                (8, 4)
            );
            assert_eq!(
                (
                    selected[0].selection.source_rect.extent.width,
                    selected[0].selection.source_rect.extent.height
                ),
                (24, 32)
            );
            assert_eq!(
                (selected[0].selection.flip_x, selected[0].selection.flip_y),
                (operation == 2, false)
            );
            tracking.command_buffer_began(command);
            tracking.reset_graph();
            tracking.commands_submitted(&[command]);
            assert!(tracking.take_present_sources(destination, 1).is_empty());
        }
    }
}
