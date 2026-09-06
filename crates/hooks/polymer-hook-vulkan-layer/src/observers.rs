//! Raw Vulkan command observations.
//!
//! This layer's only tracking responsibility is parsing hooked call
//! arguments and forwarding them to `polymer_producer_vulkan::tracking`,
//! which owns every heuristic and selection policy. Dispatch-table
//! bookkeeping (queues, physical devices) stays here because it belongs to
//! the loader-layer plumbing, not to capture.

use crate::{hooks, layer::*};
use ash::vk;

pub(crate) fn noop() {}

pub(crate) fn begin_command_buffer(
    command_buffer: vk::CommandBuffer,
    context: &hooks::BeginCommandBufferContext,
) {
    context
        .common
        .command_tracking(command_buffer)
        .command_buffer_began(command_buffer);
}

/// The draw observer deliberately declares only the dispatchable command
/// buffer. Vertex topology is irrelevant to capture target selection.
pub(crate) fn cmd_draw(command_buffer: vk::CommandBuffer, context: &hooks::CmdDrawContext) {
    context
        .common
        .command_tracking(command_buffer)
        .command_drew(command_buffer);
}

pub(crate) fn cmd_draw_indexed(
    command_buffer: vk::CommandBuffer,
    context: &hooks::CmdDrawIndexedContext,
) {
    context
        .common
        .command_tracking(command_buffer)
        .command_drew(command_buffer);
}

pub(crate) fn create_image(
    device: vk::Device,
    create_info: *const vk::ImageCreateInfo<'_>,
    image: *mut vk::Image,
    result: vk::Result,
    context: &hooks::CreateImageContext,
) {
    if result == vk::Result::SUCCESS && !create_info.is_null() && !image.is_null() {
        let info = unsafe { &*create_info };
        context.common.device_tracking(device).image_subresources(
            unsafe { *image },
            info.mip_levels,
            info.array_layers,
        );
        context.common.device_tracking(device).image_created(
            unsafe { *image },
            vk::Extent2D {
                width: info.extent.width,
                height: info.extent.height,
            },
            info.format,
            info.usage,
        );
    }
}

pub(crate) fn destroy_image(
    device: vk::Device,
    image: vk::Image,
    context: &hooks::DestroyImageContext,
) {
    context
        .common
        .device_tracking(device)
        .image_destroyed(image);
}

pub(crate) fn create_image_view(
    device: vk::Device,
    create_info: *const vk::ImageViewCreateInfo<'_>,
    view: *mut vk::ImageView,
    result: vk::Result,
    context: &hooks::CreateImageViewContext,
) {
    if result == vk::Result::SUCCESS && !create_info.is_null() && !view.is_null() {
        let info = unsafe { &*create_info };
        context.common.device_tracking(device).image_view_created(
            unsafe { *view },
            info.image,
            info.subresource_range,
        );
    }
}

pub(crate) fn destroy_image_view(
    device: vk::Device,
    view: vk::ImageView,
    context: &hooks::DestroyImageViewContext,
) {
    context
        .common
        .device_tracking(device)
        .image_view_destroyed(view);
}

pub(crate) fn create_framebuffer(
    device: vk::Device,
    create_info: *const vk::FramebufferCreateInfo<'_>,
    framebuffer: *mut vk::Framebuffer,
    result: vk::Result,
    context: &hooks::CreateFramebufferContext,
) {
    if result != vk::Result::SUCCESS || create_info.is_null() || framebuffer.is_null() {
        return;
    }
    let info = unsafe { &*create_info };
    let views = if info.attachment_count == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(info.p_attachments, info.attachment_count as usize) }
    };
    context
        .common
        .device_tracking(device)
        .framebuffer_created(unsafe { *framebuffer }, views);
}

pub(crate) fn destroy_framebuffer(
    device: vk::Device,
    framebuffer: vk::Framebuffer,
    context: &hooks::DestroyFramebufferContext,
) {
    context
        .common
        .device_tracking(device)
        .framebuffer_destroyed(framebuffer);
}

pub(crate) fn update_descriptor_sets(
    device: vk::Device,
    write_count: u32,
    writes: *const vk::WriteDescriptorSet<'_>,
    context: &hooks::UpdateDescriptorSetsContext,
) {
    if write_count == 0 || writes.is_null() {
        return;
    }
    for write in unsafe { std::slice::from_raw_parts(writes, write_count as usize) } {
        if !matches!(
            write.descriptor_type,
            vk::DescriptorType::COMBINED_IMAGE_SAMPLER | vk::DescriptorType::SAMPLED_IMAGE
        ) || write.p_image_info.is_null()
        {
            continue;
        }
        let entries = unsafe {
            std::slice::from_raw_parts(write.p_image_info, write.descriptor_count as usize)
        }
        .iter()
        .enumerate()
        .map(|(index, info)| {
            (
                write.dst_binding,
                write.dst_array_element + index as u32,
                info.image_view,
                info.image_layout,
            )
        })
        .collect::<Vec<_>>();
        context
            .common
            .device_tracking(device)
            .descriptor_images_written(write.dst_set, &entries);
    }
}

pub(crate) fn cmd_bind_descriptor_sets(
    command_buffer: vk::CommandBuffer,
    bind_point: vk::PipelineBindPoint,
    layout: vk::PipelineLayout,
    first_set: u32,
    set_count: u32,
    sets: *const vk::DescriptorSet,
    dynamic_offset_count: u32,
    dynamic_offsets: *const u32,
    context: &hooks::CmdBindDescriptorSetsContext,
) {
    let _ = (layout, dynamic_offset_count, dynamic_offsets);
    if bind_point != vk::PipelineBindPoint::GRAPHICS || set_count == 0 || sets.is_null() {
        return;
    }
    let bound = unsafe { std::slice::from_raw_parts(sets, set_count as usize) };
    context
        .common
        .command_tracking(command_buffer)
        .command_bound_descriptor_sets(command_buffer, first_set, bound);
}

pub(crate) fn cmd_push_descriptor_set(
    command_buffer: vk::CommandBuffer,
    bind_point: vk::PipelineBindPoint,
    layout: vk::PipelineLayout,
    set: u32,
    write_count: u32,
    writes: *const vk::WriteDescriptorSet<'_>,
    context: &hooks::CmdPushDescriptorSetContext,
) {
    let _ = (bind_point, layout, set);
    if write_count == 0 || writes.is_null() {
        return;
    }
    let mut views = Vec::new();
    for write in unsafe { std::slice::from_raw_parts(writes, write_count as usize) } {
        if !matches!(
            write.descriptor_type,
            vk::DescriptorType::COMBINED_IMAGE_SAMPLER | vk::DescriptorType::SAMPLED_IMAGE
        ) || write.p_image_info.is_null()
        {
            continue;
        }
        for info in unsafe {
            std::slice::from_raw_parts(write.p_image_info, write.descriptor_count as usize)
        } {
            views.push((info.image_view, info.image_layout));
        }
    }
    context
        .common
        .command_tracking(command_buffer)
        .command_pushed_descriptors(command_buffer, &views);
}

pub(crate) fn cmd_set_viewport(
    command_buffer: vk::CommandBuffer,
    first: u32,
    count: u32,
    viewports: *const vk::Viewport,
    context: &hooks::CmdSetViewportContext,
) {
    if first == 0 && count != 0 && !viewports.is_null() {
        context
            .common
            .command_tracking(command_buffer)
            .command_set_viewport(command_buffer, unsafe { *viewports });
    }
}

pub(crate) fn cmd_set_scissor(
    command_buffer: vk::CommandBuffer,
    first: u32,
    count: u32,
    scissors: *const vk::Rect2D,
    context: &hooks::CmdSetScissorContext,
) {
    if first == 0 && count != 0 && !scissors.is_null() {
        context
            .common
            .command_tracking(command_buffer)
            .command_set_scissor(command_buffer, unsafe { *scissors });
    }
}

pub(crate) fn cmd_begin_render_pass(
    command_buffer: vk::CommandBuffer,
    begin: *const vk::RenderPassBeginInfo<'_>,
    contents: vk::SubpassContents,
    context: &hooks::CmdBeginRenderPassContext,
) {
    let _ = contents;
    if !begin.is_null() {
        context
            .common
            .command_tracking(command_buffer)
            .command_began_render_pass(command_buffer, unsafe { (*begin).framebuffer });
    }
}

pub(crate) fn cmd_end_render_pass(
    command_buffer: vk::CommandBuffer,
    context: &hooks::CmdEndRenderPassContext,
) {
    context
        .common
        .command_tracking(command_buffer)
        .command_ended_render_pass(command_buffer);
}

pub(crate) fn cmd_blit_image(
    command_buffer: vk::CommandBuffer,
    source_image: vk::Image,
    source_layout: vk::ImageLayout,
    destination_image: vk::Image,
    destination_layout: vk::ImageLayout,
    region_count: u32,
    regions: *const vk::ImageBlit,
    filter: vk::Filter,
    context: &hooks::CmdBlitImageContext,
) {
    let _ = (command_buffer, destination_layout, filter);
    if region_count == 0 || regions.is_null() {
        return;
    }
    for region in unsafe { std::slice::from_raw_parts(regions, region_count as usize) } {
        context
            .common
            .command_tracking(command_buffer)
            .command_image_blitted(
                command_buffer,
                source_image,
                source_layout,
                destination_image,
                region,
            );
    }
}

pub(crate) fn cmd_copy_image(
    command_buffer: vk::CommandBuffer,
    source_image: vk::Image,
    source_layout: vk::ImageLayout,
    destination_image: vk::Image,
    destination_layout: vk::ImageLayout,
    region_count: u32,
    regions: *const vk::ImageCopy,
    context: &hooks::CmdCopyImageContext,
) {
    let _ = (command_buffer, destination_layout);
    if region_count == 0 || regions.is_null() {
        return;
    }
    let tracking = context.common.command_tracking(command_buffer);
    for region in unsafe { std::slice::from_raw_parts(regions, region_count as usize) } {
        tracking.command_image_transferred(
            command_buffer,
            source_image,
            source_layout,
            destination_image,
            region,
            polymer_producer_vulkan::Evidence::Copy,
        );
    }
}

pub(crate) fn cmd_resolve_image(
    command_buffer: vk::CommandBuffer,
    source_image: vk::Image,
    source_layout: vk::ImageLayout,
    destination_image: vk::Image,
    destination_layout: vk::ImageLayout,
    region_count: u32,
    regions: *const vk::ImageResolve,
    context: &hooks::CmdResolveImageContext,
) {
    let _ = (command_buffer, destination_layout);
    if region_count == 0 || regions.is_null() {
        return;
    }
    let tracking = context.common.command_tracking(command_buffer);
    for region in unsafe { std::slice::from_raw_parts(regions, region_count as usize) } {
        let copy = vk::ImageCopy {
            src_subresource: region.src_subresource,
            src_offset: region.src_offset,
            dst_subresource: region.dst_subresource,
            dst_offset: region.dst_offset,
            extent: region.extent,
        };
        tracking.command_image_transferred(
            command_buffer,
            source_image,
            source_layout,
            destination_image,
            &copy,
            polymer_producer_vulkan::Evidence::Resolve,
        );
    }
}

pub(crate) fn get_device_queue(
    device: vk::Device,
    queue_family_index: u32,
    queue: *mut vk::Queue,
    context: &hooks::GetDeviceQueueContext,
) {
    let Some(dispatch) = (unsafe { dispatch_key(device) }).and_then(|key| {
        context
            .common
            .state
            .device
            .get(&key)
            .map(|entry| entry.clone())
    }) else {
        return;
    };
    if !queue.is_null() {
        let queue = unsafe { *queue };
        if queue != vk::Queue::default() {
            {
                context.common.state.queue.insert(
                    queue,
                    QueueDispatchTable {
                        device,
                        queue_family_index,
                        dispatch,
                    },
                )
            };
        }
    }
}

pub(crate) fn get_device_queue2(
    device: vk::Device,
    queue_info: *const vk::DeviceQueueInfo2<'_>,
    queue: *mut vk::Queue,
    context: &hooks::GetDeviceQueue2Context,
) {
    let Some(dispatch) = (unsafe { dispatch_key(device) }).and_then(|key| {
        context
            .common
            .state
            .device
            .get(&key)
            .map(|entry| entry.clone())
    }) else {
        return;
    };
    if !queue.is_null() {
        let queue = unsafe { *queue };
        if queue != vk::Queue::default() {
            let queue_family_index = if queue_info.is_null() {
                0
            } else {
                unsafe { (*queue_info).queue_family_index }
            };
            {
                context.common.state.queue.insert(
                    queue,
                    QueueDispatchTable {
                        device,
                        queue_family_index,
                        dispatch,
                    },
                )
            };
        }
    }
}

pub(crate) fn enumerate_physical_devices(
    instance: vk::Instance,
    physical_device_count: *mut u32,
    physical_devices: *mut vk::PhysicalDevice,
    result: vk::Result,
    context: &hooks::EnumeratePhysicalDevicesContext,
) {
    let Some(next_gipa) = instance_gipa(instance) else {
        return;
    };
    let capture_state = context.common.instance_capture(instance);
    if matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
        && !physical_device_count.is_null()
        && !physical_devices.is_null()
    {
        for physical_device in
            unsafe { std::slice::from_raw_parts(physical_devices, *physical_device_count as usize) }
        {
            {
                context.common.state.physical_device.insert(
                    *physical_device,
                    PhysicalDeviceState {
                        instance,
                        next_gipa,
                        capture_state: capture_state.clone(),
                    },
                );
            }
        }
    }
}

pub(crate) fn create_instance(result: vk::Result) {
    tracing::debug!("polymer: downstream vkCreateInstance returned {result}");
}
