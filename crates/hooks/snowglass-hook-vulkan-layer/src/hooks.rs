//! Declarations for typed Vulkan hook chains.

use crate::hook_chain::vulkan_terminal;
use crate::{layer, observers, swapchain};
use ash::vk;
use snowglass_producer_vulkan::{capture, tracking};

snowglass_hook::chain_hooks! {
    #![terminal_resolver(vulkan_terminal)]
    #![order(forward)]
    #[context]
    pub(crate) struct VulkanHookContext {
        pub(crate) state: std::sync::Arc<crate::layer::VulkanHookState> = crate::layer::hook_state(),
    }

    #[export(vkCmdDraw)]
    #[terminal(dynamic(command_buffer, c"vkCmdDraw": vk::PFN_vkCmdDraw), fallback = ())]
    #[prefix(observers::cmd_draw(command_buffer))]
    pub(crate) unsafe extern "system" fn cmd_draw(
        command_buffer: vk::CommandBuffer,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    );

    #[export(vkGetDeviceQueue)]
    #[terminal(dynamic(device, c"vkGetDeviceQueue": vk::PFN_vkGetDeviceQueue), fallback = ())]
    #[postfix(observers::get_device_queue(device, queue_family_index, queue))]
    pub(crate) unsafe extern "system" fn get_device_queue(
        device: vk::Device,
        queue_family_index: u32,
        queue_index: u32,
        queue: *mut vk::Queue,
    );

    #[export(vkGetDeviceQueue2)]
    #[terminal(dynamic(device, c"vkGetDeviceQueue2": vk::PFN_vkGetDeviceQueue2), fallback = ())]
    #[postfix(observers::get_device_queue2(device, queue_info, queue))]
    pub(crate) unsafe extern "system" fn get_device_queue2(
        device: vk::Device,
        queue_info: *const vk::DeviceQueueInfo2<'_>,
        queue: *mut vk::Queue,
    );

    #[export(vkQueueSubmit)]
    #[terminal(dynamic(queue, c"vkQueueSubmit": vk::PFN_vkQueueSubmit), fallback = vk::Result::ERROR_DEVICE_LOST)]
    #[postfix(swapchain::queue_submit(queue, submit_count, submits, result))]
    pub(crate) unsafe extern "system" fn queue_submit(
        queue: vk::Queue,
        submit_count: u32,
        submits: *const vk::SubmitInfo<'_>,
        fence: vk::Fence,
    ) -> vk::Result;

    #[export(vkCreateSwapchainKHR)]
    #[terminal(direct = crate::terminal::create_swapchain)]
    #[postfix(swapchain::create_swapchain(device, create_info, swapchain, result))]
    pub(crate) unsafe extern "system" fn create_swapchain(
        device: vk::Device,
        create_info: *const vk::SwapchainCreateInfoKHR<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        swapchain: *mut vk::SwapchainKHR,
    ) -> vk::Result;

    #[export(vkDestroySwapchainKHR)]
    #[terminal(dynamic(device, c"vkDestroySwapchainKHR": vk::PFN_vkDestroySwapchainKHR), fallback = ())]
    #[prefix(swapchain::destroy_swapchain(device, swapchain))]
    pub(crate) unsafe extern "system" fn destroy_swapchain(
        device: vk::Device,
        swapchain: vk::SwapchainKHR,
        allocator: *const vk::AllocationCallbacks<'_>,
    );

    #[export(vkGetSwapchainImagesKHR)]
    #[terminal(dynamic(device, c"vkGetSwapchainImagesKHR": vk::PFN_vkGetSwapchainImagesKHR), fallback = vk::Result::ERROR_EXTENSION_NOT_PRESENT)]
    #[postfix(swapchain::get_swapchain_images(device, swapchain, image_count, images, result))]
    pub(crate) unsafe extern "system" fn get_swapchain_images(
        device: vk::Device,
        swapchain: vk::SwapchainKHR,
        image_count: *mut u32,
        images: *mut vk::Image,
    ) -> vk::Result;

    #[export(vkQueuePresentKHR)]
    #[terminal(dynamic(queue, c"vkQueuePresentKHR": vk::PFN_vkQueuePresentKHR), fallback = vk::Result::ERROR_EXTENSION_NOT_PRESENT)]
    #[infix(swapchain::handle_queue_present)]
    pub(crate) unsafe extern "system" fn queue_present(
        queue: vk::Queue,
        present_info: *const vk::PresentInfoKHR<'_>,
    ) -> vk::Result;

    #[export(vkCreateWin32SurfaceKHR)]
    #[terminal(dynamic(instance, c"vkCreateWin32SurfaceKHR": vk::PFN_vkCreateWin32SurfaceKHR), fallback = vk::Result::ERROR_EXTENSION_NOT_PRESENT)]
    #[postfix(swapchain::create_win32_surface(instance, create_info, surface, result))]
    pub(crate) unsafe extern "system" fn create_win32_surface(
        instance: vk::Instance,
        create_info: *const vk::Win32SurfaceCreateInfoKHR<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        surface: *mut vk::SurfaceKHR,
    ) -> vk::Result;

    #[export(vkDestroySurfaceKHR)]
    #[terminal(dynamic(instance, c"vkDestroySurfaceKHR": vk::PFN_vkDestroySurfaceKHR), fallback = ())]
    #[prefix(swapchain::destroy_surface(instance, surface))]
    pub(crate) unsafe extern "system" fn destroy_surface(
        instance: vk::Instance,
        surface: vk::SurfaceKHR,
        allocator: *const vk::AllocationCallbacks<'_>,
    );

    #[export(vkEnumeratePhysicalDevices)]
    #[terminal(dynamic(instance, c"vkEnumeratePhysicalDevices": vk::PFN_vkEnumeratePhysicalDevices), fallback = vk::Result::ERROR_INITIALIZATION_FAILED)]
    #[postfix(observers::enumerate_physical_devices(instance, count, devices, result))]
    pub(crate) unsafe extern "system" fn enumerate_physical_devices(
        instance: vk::Instance,
        count: *mut u32,
        devices: *mut vk::PhysicalDevice,
    ) -> vk::Result;

    #[export(vkDestroyInstance)]
    #[terminal(dynamic(instance, c"vkDestroyInstance": vk::PFN_vkDestroyInstance), fallback = ())]
    #[infix(layer::handle_destroy_instance)]
    pub(crate) unsafe extern "system" fn destroy_instance(
        instance: vk::Instance,
        allocator: *const vk::AllocationCallbacks<'_>,
    );

    #[export(vkCreateInstance)]
    #[terminal(direct = crate::terminal::create_instance)]
    #[postfix(|result, _context| observers::create_instance(result))]
    pub(crate) unsafe extern "system" fn create_instance(
        create_info: *const vk::InstanceCreateInfo<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        instance: *mut vk::Instance,
    ) -> vk::Result;

    #[export(vkCreateDevice)]
    #[terminal(direct = crate::terminal::create_device)]
    #[prefix(|_context| observers::noop())]
    pub(crate) unsafe extern "system" fn create_device(
        physical_device: vk::PhysicalDevice,
        create_info: *const vk::DeviceCreateInfo<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        device: *mut vk::Device,
    ) -> vk::Result;

    #[export(vkGetInstanceProcAddr)]
    #[terminal(direct = crate::terminal::get_instance_proc_addr)]
    #[prefix(|_context| observers::noop())]
    pub(crate) unsafe extern "system" fn get_instance_proc_addr(
        instance: vk::Instance,
        name: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction;

    #[export(vk_layerGetPhysicalDeviceProcAddr)]
    #[terminal(direct = crate::terminal::get_physical_device_proc_addr)]
    #[prefix(|_context| observers::noop())]
    pub(crate) unsafe extern "system" fn get_physical_device_proc_addr(
        instance: vk::Instance,
        name: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction;

    #[export(vkGetDeviceProcAddr)]
    #[terminal(direct = crate::terminal::get_device_proc_addr)]
    #[prefix(|_context| observers::noop())]
    pub(crate) unsafe extern "system" fn get_device_proc_addr(
        device: vk::Device,
        name: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction;

    #[export(vkDestroyDevice)]
    #[terminal(dynamic(device, c"vkDestroyDevice": vk::PFN_vkDestroyDevice), fallback = ())]
    #[infix(layer::handle_destroy_device)]
    pub(crate) unsafe extern "system" fn destroy_device(
        device: vk::Device,
        allocator: *const vk::AllocationCallbacks<'_>,
    );

    #[export(vkBeginCommandBuffer)]
    #[terminal(dynamic(command_buffer, c"vkBeginCommandBuffer": vk::PFN_vkBeginCommandBuffer), fallback = vk::Result::ERROR_INITIALIZATION_FAILED)]
    #[prefix(observers::begin_command_buffer(command_buffer))]
    pub(crate) unsafe extern "system" fn begin_command_buffer(
        command_buffer: vk::CommandBuffer,
        begin_info: *const vk::CommandBufferBeginInfo<'_>,
    ) -> vk::Result;

    #[export(vkCmdDrawIndexed)]
    #[terminal(dynamic(command_buffer, c"vkCmdDrawIndexed": vk::PFN_vkCmdDrawIndexed), fallback = ())]
    #[prefix(observers::cmd_draw_indexed(command_buffer))]
    pub(crate) unsafe extern "system" fn cmd_draw_indexed(
        command_buffer: vk::CommandBuffer,
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        vertex_offset: i32,
        first_instance: u32,
    );

    #[export(vkCmdSetViewport)]
    #[terminal(dynamic(command_buffer, c"vkCmdSetViewport": vk::PFN_vkCmdSetViewport), fallback = ())]
    #[prefix(observers::cmd_set_viewport(command_buffer, first, count, viewports))]
    pub(crate) unsafe extern "system" fn cmd_set_viewport(
        command_buffer: vk::CommandBuffer,
        first: u32,
        count: u32,
        viewports: *const vk::Viewport,
    );

    #[export(vkCmdSetScissor)]
    #[terminal(dynamic(command_buffer, c"vkCmdSetScissor": vk::PFN_vkCmdSetScissor), fallback = ())]
    #[prefix(observers::cmd_set_scissor(command_buffer, first, count, scissors))]
    pub(crate) unsafe extern "system" fn cmd_set_scissor(
        command_buffer: vk::CommandBuffer,
        first: u32,
        count: u32,
        scissors: *const vk::Rect2D,
    );

    #[export(vkCmdEndRenderPass)]
    #[terminal(dynamic(command_buffer, c"vkCmdEndRenderPass": vk::PFN_vkCmdEndRenderPass), fallback = ())]
    #[prefix(observers::cmd_end_render_pass(command_buffer))]
    pub(crate) unsafe extern "system" fn cmd_end_render_pass(
        command_buffer: vk::CommandBuffer,
    );

    #[export(vkCmdBeginRenderPass)]
    #[terminal(dynamic(command_buffer, c"vkCmdBeginRenderPass": vk::PFN_vkCmdBeginRenderPass), fallback = ())]
    #[prefix(observers::cmd_begin_render_pass(command_buffer, begin, contents))]
    pub(crate) unsafe extern "system" fn cmd_begin_render_pass(
        command_buffer: vk::CommandBuffer,
        begin: *const vk::RenderPassBeginInfo<'_>,
        contents: vk::SubpassContents,
    );

    #[export(vkCmdBindDescriptorSets)]
    #[terminal(dynamic(command_buffer, c"vkCmdBindDescriptorSets": vk::PFN_vkCmdBindDescriptorSets), fallback = ())]
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
    );

    #[export(vkCmdPushDescriptorSetKHR)]
    #[terminal(dynamic(command_buffer, c"vkCmdPushDescriptorSetKHR": vk::PFN_vkCmdPushDescriptorSetKHR), fallback = ())]
    #[prefix(observers::cmd_push_descriptor_set(command_buffer, bind_point, layout, set, write_count, writes))]
    pub(crate) unsafe extern "system" fn cmd_push_descriptor_set(
        command_buffer: vk::CommandBuffer,
        bind_point: vk::PipelineBindPoint,
        layout: vk::PipelineLayout,
        set: u32,
        write_count: u32,
        writes: *const vk::WriteDescriptorSet<'_>,
    );

    #[export(vkCmdBlitImage)]
    #[terminal(dynamic(command_buffer, c"vkCmdBlitImage": vk::PFN_vkCmdBlitImage), fallback = ())]
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
    );

    #[export(vkCmdCopyImage)]
    #[terminal(dynamic(command_buffer, c"vkCmdCopyImage": vk::PFN_vkCmdCopyImage), fallback = ())]
    #[prefix(observers::cmd_copy_image(command_buffer, source_image, source_layout, destination_image, destination_layout, region_count, regions))]
    pub(crate) unsafe extern "system" fn cmd_copy_image(
        command_buffer: vk::CommandBuffer,
        source_image: vk::Image,
        source_layout: vk::ImageLayout,
        destination_image: vk::Image,
        destination_layout: vk::ImageLayout,
        region_count: u32,
        regions: *const vk::ImageCopy,
    );

    #[export(vkCreateImage)]
    #[terminal(direct = crate::terminal::create_image)]
    #[postfix(observers::create_image(device, create_info, image, result))]
    pub(crate) unsafe extern "system" fn create_image(
        device: vk::Device,
        create_info: *const vk::ImageCreateInfo<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        image: *mut vk::Image,
    ) -> vk::Result;

    #[export(vkCreateImageView)]
    #[terminal(dynamic(device, c"vkCreateImageView": vk::PFN_vkCreateImageView), fallback = vk::Result::ERROR_INITIALIZATION_FAILED)]
    #[postfix(observers::create_image_view(device, create_info, view, result))]
    pub(crate) unsafe extern "system" fn create_image_view(
        device: vk::Device,
        create_info: *const vk::ImageViewCreateInfo<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        view: *mut vk::ImageView,
    ) -> vk::Result;

    #[export(vkCreateFramebuffer)]
    #[terminal(dynamic(device, c"vkCreateFramebuffer": vk::PFN_vkCreateFramebuffer), fallback = vk::Result::ERROR_INITIALIZATION_FAILED)]
    #[postfix(observers::create_framebuffer(device, create_info, framebuffer, result))]
    pub(crate) unsafe extern "system" fn create_framebuffer(
        device: vk::Device,
        create_info: *const vk::FramebufferCreateInfo<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        framebuffer: *mut vk::Framebuffer,
    ) -> vk::Result;

    #[export(vkDestroyImage)]
    #[terminal(dynamic(device, c"vkDestroyImage": vk::PFN_vkDestroyImage), fallback = ())]
    #[prefix(observers::destroy_image(device, image))]
    pub(crate) unsafe extern "system" fn destroy_image(
        device: vk::Device,
        image: vk::Image,
        allocator: *const vk::AllocationCallbacks<'_>,
    );

    #[export(vkDestroyImageView)]
    #[terminal(dynamic(device, c"vkDestroyImageView": vk::PFN_vkDestroyImageView), fallback = ())]
    #[prefix(observers::destroy_image_view(device, view))]
    pub(crate) unsafe extern "system" fn destroy_image_view(
        device: vk::Device,
        view: vk::ImageView,
        allocator: *const vk::AllocationCallbacks<'_>,
    );

    #[export(vkDestroyFramebuffer)]
    #[terminal(dynamic(device, c"vkDestroyFramebuffer": vk::PFN_vkDestroyFramebuffer), fallback = ())]
    #[prefix(observers::destroy_framebuffer(device, framebuffer))]
    pub(crate) unsafe extern "system" fn destroy_framebuffer(
        device: vk::Device,
        framebuffer: vk::Framebuffer,
        allocator: *const vk::AllocationCallbacks<'_>,
    );

    #[export(vkUpdateDescriptorSets)]
    #[terminal(dynamic(device, c"vkUpdateDescriptorSets": vk::PFN_vkUpdateDescriptorSets), fallback = ())]
    #[prefix(observers::update_descriptor_sets(device, write_count, writes))]
    pub(crate) unsafe extern "system" fn update_descriptor_sets(
        device: vk::Device,
        write_count: u32,
        writes: *const vk::WriteDescriptorSet<'_>,
        copy_count: u32,
        copies: *const vk::CopyDescriptorSet<'_>,
    );
}

snowglass_hook::chain_hooks! {
    #![order(forward)]

    #[export(vkNegotiateLoaderLayerInterfaceVersion)]
    #[terminal(direct = crate::terminal::negotiate)]
    pub(crate) unsafe extern "system" fn negotiate(
        interface: *mut crate::layer::VkNegotiateLayerInterface,
    ) -> vk::Result;
}

impl VulkanHookContext {
    pub(crate) fn command_tracking(
        &self,
        command_buffer: vk::CommandBuffer,
    ) -> std::sync::Arc<tracking::Tracking> {
        unsafe { crate::layer::dispatch_key(command_buffer) }
            .and_then(|key| {
                self.state
                    .device
                    .get(&key)
                    .map(|entry| entry.tracking.clone())
            })
            .unwrap_or_default()
    }

    pub(crate) fn device_tracking(&self, device: vk::Device) -> std::sync::Arc<tracking::Tracking> {
        unsafe { crate::layer::dispatch_key(device) }
            .and_then(|key| {
                self.state
                    .device
                    .get(&key)
                    .map(|entry| entry.tracking.clone())
            })
            .unwrap_or_default()
    }

    pub(crate) fn queue_tracking(&self, queue: vk::Queue) -> std::sync::Arc<tracking::Tracking> {
        self.state
            .queue
            .get(&queue)
            .map(|entry| entry.dispatch.tracking.clone())
            .unwrap_or_default()
    }

    pub(crate) fn instance_capture(
        &self,
        instance: vk::Instance,
    ) -> std::sync::Arc<capture::CaptureState> {
        unsafe { crate::layer::dispatch_key(instance) }
            .and_then(|key| {
                self.state
                    .instance
                    .get(&key)
                    .map(|entry| entry.capture_state.clone())
            })
            .unwrap_or_default()
    }

    pub(crate) fn device_capture(
        &self,
        device: vk::Device,
    ) -> std::sync::Arc<capture::CaptureState> {
        unsafe { crate::layer::dispatch_key(device) }
            .and_then(|key| {
                self.state
                    .device
                    .get(&key)
                    .map(|entry| entry.capture_state.clone())
            })
            .unwrap_or_default()
    }

    pub(crate) fn queue_capture(&self, queue: vk::Queue) -> std::sync::Arc<capture::CaptureState> {
        self.state
            .queue
            .get(&queue)
            .map(|entry| entry.dispatch.capture_state.clone())
            .unwrap_or_default()
    }
}
