//! Declarations for typed Vulkan hook chains.

use crate::hook_chain::{__resolve_observer_terminal, chain_hooks};
use crate::{layer, observers, swapchain};
use ash::vk;
use snowglass_producer_vulkan::{capture, tracking};

chain_hooks! {
    #![order(forward)]

    #[export(vkCmdDraw)]
    #[terminal(command_buffer, c"vkCmdDraw": vk::PFN_vkCmdDraw)]
    #[prefix(observers::cmd_draw(command_buffer))]
    pub(crate) unsafe extern "system" fn cmd_draw(
        command_buffer: vk::CommandBuffer,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    ) {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
        }
    }

    #[export(vkGetDeviceQueue)]
    #[terminal(device, c"vkGetDeviceQueue": vk::PFN_vkGetDeviceQueue)]
    #[postfix(observers::get_device_queue(device, queue_family_index, queue))]
    pub(crate) unsafe extern "system" fn get_device_queue(
        device: vk::Device,
        queue_family_index: u32,
        queue_index: u32,
        queue: *mut vk::Queue,
    );

    #[export(vkGetDeviceQueue2)]
    #[terminal(device, c"vkGetDeviceQueue2": vk::PFN_vkGetDeviceQueue2)]
    #[postfix(observers::get_device_queue2(device, queue_info, queue))]
    pub(crate) unsafe extern "system" fn get_device_queue2(
        device: vk::Device,
        queue_info: *const vk::DeviceQueueInfo2<'_>,
        queue: *mut vk::Queue,
    );

    #[export(vkQueueSubmit)]
    #[terminal(queue, c"vkQueueSubmit": vk::PFN_vkQueueSubmit, fallback = vk::Result::ERROR_DEVICE_LOST)]
    #[postfix(swapchain::queue_submit(queue, submit_count, submits, result))]
    pub(crate) unsafe extern "system" fn queue_submit(
        queue: vk::Queue,
        submit_count: u32,
        submits: *const vk::SubmitInfo<'_>,
        fence: vk::Fence,
    ) -> vk::Result {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::queue_tracking(queue),
            capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::queue_capture(queue),
        }
    }

    #[export(vkCreateSwapchainKHR)]
    #[terminal(crate::terminal::create_swapchain)]
    #[postfix(swapchain::create_swapchain(device, create_info, swapchain, result))]
    pub(crate) unsafe extern "system" fn create_swapchain(
        device: vk::Device,
        create_info: *const vk::SwapchainCreateInfoKHR<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        swapchain: *mut vk::SwapchainKHR,
    ) -> vk::Result {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
            capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::device_capture(device),
        }
    }

    #[export(vkDestroySwapchainKHR)]
    #[terminal(device, c"vkDestroySwapchainKHR": vk::PFN_vkDestroySwapchainKHR)]
    #[prefix(swapchain::destroy_swapchain(swapchain))]
    pub(crate) unsafe extern "system" fn destroy_swapchain(
        device: vk::Device,
        swapchain: vk::SwapchainKHR,
        allocator: *const vk::AllocationCallbacks<'_>,
    ) {
        context struct {
            capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::device_capture(device),
        }
    }

    #[export(vkGetSwapchainImagesKHR)]
    #[terminal(device, c"vkGetSwapchainImagesKHR": vk::PFN_vkGetSwapchainImagesKHR, fallback = vk::Result::ERROR_EXTENSION_NOT_PRESENT)]
    #[postfix(swapchain::get_swapchain_images(swapchain, image_count, images, result))]
    pub(crate) unsafe extern "system" fn get_swapchain_images(
        device: vk::Device,
        swapchain: vk::SwapchainKHR,
        image_count: *mut u32,
        images: *mut vk::Image,
    ) -> vk::Result {
        context struct {
            capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::device_capture(device),
        }
    }

    #[export(vkQueuePresentKHR)]
    #[terminal(queue, c"vkQueuePresentKHR": vk::PFN_vkQueuePresentKHR, fallback = vk::Result::ERROR_EXTENSION_NOT_PRESENT)]
    #[infix(swapchain::handle_queue_present)]
    pub(crate) unsafe extern "system" fn queue_present(
        queue: vk::Queue,
        present_info: *const vk::PresentInfoKHR<'_>,
    ) -> vk::Result {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::queue_tracking(queue),
            capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::queue_capture(queue),
        }
    }

    #[export(vkCreateWin32SurfaceKHR)]
    #[terminal(instance, c"vkCreateWin32SurfaceKHR": vk::PFN_vkCreateWin32SurfaceKHR, fallback = vk::Result::ERROR_EXTENSION_NOT_PRESENT)]
    #[postfix(swapchain::create_win32_surface(create_info, surface, result))]
    pub(crate) unsafe extern "system" fn create_win32_surface(
        instance: vk::Instance,
        create_info: *const vk::Win32SurfaceCreateInfoKHR<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        surface: *mut vk::SurfaceKHR,
    ) -> vk::Result {
        context struct {
            capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::instance_capture(instance),
        }
    }

    #[export(vkDestroySurfaceKHR)]
    #[terminal(instance, c"vkDestroySurfaceKHR": vk::PFN_vkDestroySurfaceKHR)]
    #[prefix(swapchain::destroy_surface(surface))]
    pub(crate) unsafe extern "system" fn destroy_surface(
        instance: vk::Instance,
        surface: vk::SurfaceKHR,
        allocator: *const vk::AllocationCallbacks<'_>,
    ) {
        context struct {
            capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::instance_capture(instance),
        }
    }

    #[export(vkEnumeratePhysicalDevices)]
    #[terminal(instance, c"vkEnumeratePhysicalDevices": vk::PFN_vkEnumeratePhysicalDevices, fallback = vk::Result::ERROR_INITIALIZATION_FAILED)]
    #[postfix(observers::enumerate_physical_devices(instance, count, devices, result))]
    pub(crate) unsafe extern "system" fn enumerate_physical_devices(
        instance: vk::Instance,
        count: *mut u32,
        devices: *mut vk::PhysicalDevice,
    ) -> vk::Result;

    #[export(vkDestroyInstance)]
    #[terminal(instance, c"vkDestroyInstance": vk::PFN_vkDestroyInstance, fallback = ())]
    #[infix(layer::handle_destroy_instance)]
    pub(crate) unsafe extern "system" fn destroy_instance(
        instance: vk::Instance,
        allocator: *const vk::AllocationCallbacks<'_>,
    );

    #[export(vkCreateInstance)]
    #[terminal(crate::terminal::create_instance)]
    #[postfix(observers::create_instance(result))]
    pub(crate) unsafe extern "system" fn create_instance(
        create_info: *const vk::InstanceCreateInfo<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        instance: *mut vk::Instance,
    ) -> vk::Result;

    #[export(vkCreateDevice)]
    #[terminal(crate::terminal::create_device)]
    #[prefix(observers::noop())]
    pub(crate) unsafe extern "system" fn create_device(
        physical_device: vk::PhysicalDevice,
        create_info: *const vk::DeviceCreateInfo<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        device: *mut vk::Device,
    ) -> vk::Result;

    #[export(vkNegotiateLoaderLayerInterfaceVersion)]
    #[terminal(crate::terminal::negotiate)]
    #[prefix(observers::noop())]
    pub(crate) unsafe extern "system" fn negotiate(
        interface: *mut crate::layer::VkNegotiateLayerInterface,
    ) -> vk::Result;

    #[export(vkGetInstanceProcAddr)]
    #[terminal(crate::terminal::get_instance_proc_addr)]
    #[prefix(observers::noop())]
    pub(crate) unsafe extern "system" fn get_instance_proc_addr(
        instance: vk::Instance,
        name: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction;

    #[export(vk_layerGetPhysicalDeviceProcAddr)]
    #[terminal(crate::terminal::get_physical_device_proc_addr)]
    #[prefix(observers::noop())]
    pub(crate) unsafe extern "system" fn get_physical_device_proc_addr(
        instance: vk::Instance,
        name: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction;

    #[export(vkGetDeviceProcAddr)]
    #[terminal(crate::terminal::get_device_proc_addr)]
    #[prefix(observers::noop())]
    pub(crate) unsafe extern "system" fn get_device_proc_addr(
        device: vk::Device,
        name: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction;

    #[export(vkDestroyDevice)]
    #[terminal(device, c"vkDestroyDevice": vk::PFN_vkDestroyDevice, fallback = ())]
    #[infix(layer::handle_destroy_device)]
    pub(crate) unsafe extern "system" fn destroy_device(
        device: vk::Device,
        allocator: *const vk::AllocationCallbacks<'_>,
    ) {
        context struct {
            capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::device_capture(device),
        }
    }

    #[export(vkBeginCommandBuffer)]
    #[terminal(command_buffer, c"vkBeginCommandBuffer": vk::PFN_vkBeginCommandBuffer, fallback = vk::Result::ERROR_INITIALIZATION_FAILED)]
    #[prefix(observers::begin_command_buffer(command_buffer))]
    pub(crate) unsafe extern "system" fn begin_command_buffer(
        command_buffer: vk::CommandBuffer,
        begin_info: *const vk::CommandBufferBeginInfo<'_>,
    ) -> vk::Result {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
        }
    }

    #[export(vkCmdDrawIndexed)]
    #[terminal(command_buffer, c"vkCmdDrawIndexed": vk::PFN_vkCmdDrawIndexed)]
    #[prefix(observers::cmd_draw_indexed(command_buffer))]
    pub(crate) unsafe extern "system" fn cmd_draw_indexed(
        command_buffer: vk::CommandBuffer,
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        vertex_offset: i32,
        first_instance: u32,
    ) {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
        }
    }

    #[export(vkCmdSetViewport)]
    #[terminal(command_buffer, c"vkCmdSetViewport": vk::PFN_vkCmdSetViewport)]
    #[prefix(observers::cmd_set_viewport(command_buffer, first, count, viewports))]
    pub(crate) unsafe extern "system" fn cmd_set_viewport(
        command_buffer: vk::CommandBuffer,
        first: u32,
        count: u32,
        viewports: *const vk::Viewport,
    ) {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
        }
    }

    #[export(vkCmdSetScissor)]
    #[terminal(command_buffer, c"vkCmdSetScissor": vk::PFN_vkCmdSetScissor)]
    #[prefix(observers::cmd_set_scissor(command_buffer, first, count, scissors))]
    pub(crate) unsafe extern "system" fn cmd_set_scissor(
        command_buffer: vk::CommandBuffer,
        first: u32,
        count: u32,
        scissors: *const vk::Rect2D,
    ) {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
        }
    }

    #[export(vkCmdEndRenderPass)]
    #[terminal(command_buffer, c"vkCmdEndRenderPass": vk::PFN_vkCmdEndRenderPass)]
    #[prefix(observers::cmd_end_render_pass(command_buffer))]
    pub(crate) unsafe extern "system" fn cmd_end_render_pass(
        command_buffer: vk::CommandBuffer,
    ) {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
        }
    }

    #[export(vkCmdBeginRenderPass)]
    #[terminal(command_buffer, c"vkCmdBeginRenderPass": vk::PFN_vkCmdBeginRenderPass)]
    #[prefix(observers::cmd_begin_render_pass(command_buffer, begin, contents))]
    pub(crate) unsafe extern "system" fn cmd_begin_render_pass(
        command_buffer: vk::CommandBuffer,
        begin: *const vk::RenderPassBeginInfo<'_>,
        contents: vk::SubpassContents,
    ) {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
        }
    }

    #[export(vkCmdBindDescriptorSets)]
    #[terminal(command_buffer, c"vkCmdBindDescriptorSets": vk::PFN_vkCmdBindDescriptorSets)]
    #[prefix(observers::cmd_bind_descriptor_sets(command_buffer, bind_point, layout, first_set, set_count, sets, dynamic_offset_count, dynamic_offsets))]
    pub(crate) unsafe extern "system" fn cmd_bind_descriptor_sets(
        command_buffer: vk::CommandBuffer,
        bind_point: vk::PipelineBindPoint,
        layout: vk::PipelineLayout,
        first_set: u32,
        set_count: u32,
        sets: *const vk::DescriptorSet,
        dynamic_offset_count: u32,
        dynamic_offsets: *const u32,
    ) {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
        }
    }

    #[export(vkCmdPushDescriptorSetKHR)]
    #[terminal(command_buffer, c"vkCmdPushDescriptorSetKHR": vk::PFN_vkCmdPushDescriptorSetKHR)]
    #[prefix(observers::cmd_push_descriptor_set(command_buffer, bind_point, layout, set, write_count, writes))]
    pub(crate) unsafe extern "system" fn cmd_push_descriptor_set(
        command_buffer: vk::CommandBuffer,
        bind_point: vk::PipelineBindPoint,
        layout: vk::PipelineLayout,
        set: u32,
        write_count: u32,
        writes: *const vk::WriteDescriptorSet<'_>,
    ) {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
        }
    }

    #[export(vkCmdBlitImage)]
    #[terminal(command_buffer, c"vkCmdBlitImage": vk::PFN_vkCmdBlitImage)]
    #[prefix(observers::cmd_blit_image(command_buffer, source_image, source_layout, destination_image, destination_layout, region_count, regions, filter))]
    pub(crate) unsafe extern "system" fn cmd_blit_image(
        command_buffer: vk::CommandBuffer,
        source_image: vk::Image,
        source_layout: vk::ImageLayout,
        destination_image: vk::Image,
        destination_layout: vk::ImageLayout,
        region_count: u32,
        regions: *const vk::ImageBlit,
        filter: vk::Filter,
    ) {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
        }
    }

    #[export(vkCmdCopyImage)]
    #[terminal(command_buffer, c"vkCmdCopyImage": vk::PFN_vkCmdCopyImage)]
    #[prefix(observers::cmd_copy_image(command_buffer, source_image, source_layout, destination_image, destination_layout, region_count, regions))]
    pub(crate) unsafe extern "system" fn cmd_copy_image(
        command_buffer: vk::CommandBuffer,
        source_image: vk::Image,
        source_layout: vk::ImageLayout,
        destination_image: vk::Image,
        destination_layout: vk::ImageLayout,
        region_count: u32,
        regions: *const vk::ImageCopy,
    ) {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
        }
    }

    #[export(vkCreateImage)]
    #[terminal(crate::terminal::create_image)]
    #[postfix(observers::create_image(create_info, image, result))]
    pub(crate) unsafe extern "system" fn create_image(
        device: vk::Device,
        create_info: *const vk::ImageCreateInfo<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        image: *mut vk::Image,
    ) -> vk::Result {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
        }
    }

    #[export(vkCreateImageView)]
    #[terminal(device, c"vkCreateImageView": vk::PFN_vkCreateImageView, fallback = vk::Result::ERROR_INITIALIZATION_FAILED)]
    #[postfix(observers::create_image_view(create_info, view, result))]
    pub(crate) unsafe extern "system" fn create_image_view(
        device: vk::Device,
        create_info: *const vk::ImageViewCreateInfo<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        view: *mut vk::ImageView,
    ) -> vk::Result {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
        }
    }

    #[export(vkCreateFramebuffer)]
    #[terminal(device, c"vkCreateFramebuffer": vk::PFN_vkCreateFramebuffer, fallback = vk::Result::ERROR_INITIALIZATION_FAILED)]
    #[postfix(observers::create_framebuffer(create_info, framebuffer, result))]
    pub(crate) unsafe extern "system" fn create_framebuffer(
        device: vk::Device,
        create_info: *const vk::FramebufferCreateInfo<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        framebuffer: *mut vk::Framebuffer,
    ) -> vk::Result {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
        }
    }

    #[export(vkDestroyImage)]
    #[terminal(device, c"vkDestroyImage": vk::PFN_vkDestroyImage)]
    #[prefix(observers::destroy_image(image))]
    pub(crate) unsafe extern "system" fn destroy_image(
        device: vk::Device,
        image: vk::Image,
        allocator: *const vk::AllocationCallbacks<'_>,
    ) {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
        }
    }

    #[export(vkDestroyImageView)]
    #[terminal(device, c"vkDestroyImageView": vk::PFN_vkDestroyImageView)]
    #[prefix(observers::destroy_image_view(view))]
    pub(crate) unsafe extern "system" fn destroy_image_view(
        device: vk::Device,
        view: vk::ImageView,
        allocator: *const vk::AllocationCallbacks<'_>,
    ) {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
        }
    }

    #[export(vkDestroyFramebuffer)]
    #[terminal(device, c"vkDestroyFramebuffer": vk::PFN_vkDestroyFramebuffer)]
    #[prefix(observers::destroy_framebuffer(framebuffer))]
    pub(crate) unsafe extern "system" fn destroy_framebuffer(
        device: vk::Device,
        framebuffer: vk::Framebuffer,
        allocator: *const vk::AllocationCallbacks<'_>,
    ) {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
        }
    }

    #[export(vkUpdateDescriptorSets)]
    #[terminal(device, c"vkUpdateDescriptorSets": vk::PFN_vkUpdateDescriptorSets)]
    #[prefix(observers::update_descriptor_sets(write_count, writes))]
    pub(crate) unsafe extern "system" fn update_descriptor_sets(
        device: vk::Device,
        write_count: u32,
        writes: *const vk::WriteDescriptorSet<'_>,
        copy_count: u32,
        copies: *const vk::CopyDescriptorSet<'_>,
    ) {
        context struct {
            tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
        }
    }
}
