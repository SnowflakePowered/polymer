//! Declarations for typed Vulkan hook chains.

use crate::hook_chain::{
    __resolve_around_terminal, __resolve_observer_terminal, around_chain, chain, observer_chain,
    post_chain,
};
use ash::vk;
use snowglass_producer_vulkan::{capture, tracking};

chain! {
    pub(crate) dispatch_cmd_draw, register_cmd_draw;
    observes vkCmdDraw;
    chain CMD_DRAW_CHAIN, context CmdDrawContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
    };
    hook CmdDrawHook(command_buffer: vk::CommandBuffer);
    abi (
        command_buffer: vk::CommandBuffer,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    );
    resolve with command_buffer, c"vkCmdDraw": vk::PFN_vkCmdDraw;
}

chain! {
    pub(crate) dispatch_get_device_queue, register_get_device_queue;
    after vkGetDeviceQueue;
    chain GET_DEVICE_QUEUE_CHAIN, context GetDeviceQueueContext;
    hook GetDeviceQueueHook(device: vk::Device, queue_family_index: u32, queue: *mut vk::Queue);
    abi (device: vk::Device, queue_family_index: u32, queue_index: u32, queue: *mut vk::Queue);
    resolve with device, c"vkGetDeviceQueue": vk::PFN_vkGetDeviceQueue;
}

chain! {
    pub(crate) dispatch_get_device_queue2, register_get_device_queue2;
    after vkGetDeviceQueue2;
    chain GET_DEVICE_QUEUE2_CHAIN, context GetDeviceQueue2Context;
    hook GetDeviceQueue2Hook(device: vk::Device, queue_info: *const vk::DeviceQueueInfo2<'_>, queue: *mut vk::Queue);
    resolve with device, c"vkGetDeviceQueue2": vk::PFN_vkGetDeviceQueue2;
}

chain! {
    pub(crate) dispatch_queue_submit, register_queue_submit;
    after vkQueueSubmit;
    chain QUEUE_SUBMIT_CHAIN, context QueueSubmitContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::queue_tracking(queue),
        capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::queue_capture(queue),
    };
    hook QueueSubmitHook(queue: vk::Queue, submit_count: u32, submits: *const vk::SubmitInfo<'_>) -> result: vk::Result;
    abi (queue: vk::Queue, submit_count: u32, submits: *const vk::SubmitInfo<'_>, fence: vk::Fence);
    resolve with queue, c"vkQueueSubmit": vk::PFN_vkQueueSubmit -> vk::Result::ERROR_DEVICE_LOST;
}

chain! {
    pub(crate) dispatch_create_swapchain, register_create_swapchain;
    after vkCreateSwapchainKHR;
    chain CREATE_SWAPCHAIN_CHAIN, context CreateSwapchainContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
        capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::device_capture(device),
    };
    hook CreateSwapchainHook(device: vk::Device, create_info: *const vk::SwapchainCreateInfoKHR<'_>, swapchain: *mut vk::SwapchainKHR) -> result: vk::Result;
    abi (device: vk::Device, create_info: *const vk::SwapchainCreateInfoKHR<'_>, allocator: *const vk::AllocationCallbacks<'_>, swapchain: *mut vk::SwapchainKHR);
    resolve with crate::terminal::create_swapchain;
}
chain! {
    pub(crate) dispatch_destroy_swapchain, register_destroy_swapchain;
    observes vkDestroySwapchainKHR;
    chain DESTROY_SWAPCHAIN_CHAIN, context DestroySwapchainContext {
        capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::device_capture(device),
    };
    hook DestroySwapchainHook(swapchain: vk::SwapchainKHR);
    abi (device: vk::Device, swapchain: vk::SwapchainKHR, allocator: *const vk::AllocationCallbacks<'_>);
    resolve with device, c"vkDestroySwapchainKHR": vk::PFN_vkDestroySwapchainKHR;
}
chain! {
    pub(crate) dispatch_get_swapchain_images, register_get_swapchain_images;
    after vkGetSwapchainImagesKHR;
    chain GET_SWAPCHAIN_IMAGES_CHAIN, context GetSwapchainImagesContext {
        capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::device_capture(device),
    };
    hook GetSwapchainImagesHook(swapchain: vk::SwapchainKHR, image_count: *mut u32, images: *mut vk::Image) -> result: vk::Result;
    abi (device: vk::Device, swapchain: vk::SwapchainKHR, image_count: *mut u32, images: *mut vk::Image);
    resolve with device, c"vkGetSwapchainImagesKHR": vk::PFN_vkGetSwapchainImagesKHR -> vk::Result::ERROR_EXTENSION_NOT_PRESENT;
}

chain! {
    pub(crate) dispatch_queue_present, register_queue_present;
    around vkQueuePresentKHR;
    chain QUEUE_PRESENT_CHAIN, context QueuePresentContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::queue_tracking(queue),
        capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::queue_capture(queue),
    };
    hook QueuePresentHook(queue: vk::Queue, present_info: *const vk::PresentInfoKHR<'_>) -> vk::Result;
    resolve with queue, c"vkQueuePresentKHR": vk::PFN_vkQueuePresentKHR -> vk::Result::ERROR_EXTENSION_NOT_PRESENT;
}

chain! {
    pub(crate) dispatch_create_win32_surface, register_create_win32_surface;
    after vkCreateWin32SurfaceKHR;
    chain CREATE_WIN32_SURFACE_CHAIN, context CreateWin32SurfaceContext {
        capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::instance_capture(instance),
    };
    hook CreateWin32SurfaceHook(create_info: *const vk::Win32SurfaceCreateInfoKHR<'_>, surface: *mut vk::SurfaceKHR) -> result: vk::Result;
    abi (instance: vk::Instance, create_info: *const vk::Win32SurfaceCreateInfoKHR<'_>, allocator: *const vk::AllocationCallbacks<'_>, surface: *mut vk::SurfaceKHR);
    resolve with instance, c"vkCreateWin32SurfaceKHR": vk::PFN_vkCreateWin32SurfaceKHR -> vk::Result::ERROR_EXTENSION_NOT_PRESENT;
}
chain! {
    pub(crate) dispatch_destroy_surface, register_destroy_surface;
    observes vkDestroySurfaceKHR;
    chain DESTROY_SURFACE_CHAIN, context DestroySurfaceContext {
        capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::instance_capture(instance),
    };
    hook DestroySurfaceHook(surface: vk::SurfaceKHR);
    abi (instance: vk::Instance, surface: vk::SurfaceKHR, allocator: *const vk::AllocationCallbacks<'_>);
    resolve with instance, c"vkDestroySurfaceKHR": vk::PFN_vkDestroySurfaceKHR;
}
chain! {
    pub(crate) dispatch_enumerate_physical_devices, register_enumerate_physical_devices;
    after vkEnumeratePhysicalDevices;
    chain ENUMERATE_PHYSICAL_DEVICES_CHAIN, context EnumeratePhysicalDevicesContext;
    hook EnumeratePhysicalDevicesHook(instance: vk::Instance, count: *mut u32, devices: *mut vk::PhysicalDevice) -> result: vk::Result;
    resolve with instance, c"vkEnumeratePhysicalDevices": vk::PFN_vkEnumeratePhysicalDevices -> vk::Result::ERROR_INITIALIZATION_FAILED;
}
chain! {
    pub(crate) dispatch_destroy_instance, register_destroy_instance;
    around vkDestroyInstance;
    chain DESTROY_INSTANCE_CHAIN, context DestroyInstanceContext;
    hook DestroyInstanceHook(instance: vk::Instance, allocator: *const vk::AllocationCallbacks<'_>) -> ();
    resolve with instance, c"vkDestroyInstance": vk::PFN_vkDestroyInstance -> ();
}
chain! {
    pub(crate) dispatch_create_instance, register_create_instance;
    after vkCreateInstance;
    chain CREATE_INSTANCE_CHAIN, context CreateInstanceContext;
    hook CreateInstanceHook() -> result: vk::Result;
    abi (create_info: *const vk::InstanceCreateInfo<'_>, allocator: *const vk::AllocationCallbacks<'_>, instance: *mut vk::Instance);
    resolve with crate::terminal::create_instance;
}
chain! {
    pub(crate) dispatch_create_device, register_create_device;
    observes vkCreateDevice;
    chain CREATE_DEVICE_CHAIN, context CreateDeviceContext;
    hook CreateDeviceHook() -> vk::Result;
    abi (physical_device: vk::PhysicalDevice, create_info: *const vk::DeviceCreateInfo<'_>, allocator: *const vk::AllocationCallbacks<'_>, device: *mut vk::Device);
    resolve with crate::terminal::create_device;
}
chain! {
    pub(crate) dispatch_negotiate, register_negotiate;
    observes vkNegotiateLoaderLayerInterfaceVersion;
    chain NEGOTIATE_CHAIN, context NegotiateContext;
    hook NegotiateHook() -> vk::Result;
    abi (interface: *mut crate::layer::VkNegotiateLayerInterface);
    resolve with crate::terminal::negotiate;
}
chain! {
    pub(crate) dispatch_get_instance_proc_addr, register_get_instance_proc_addr;
    observes vkGetInstanceProcAddr;
    chain GET_INSTANCE_PROC_ADDR_CHAIN, context GetInstanceProcAddrContext;
    hook GetInstanceProcAddrHook() -> vk::PFN_vkVoidFunction;
    abi (instance: vk::Instance, name: *const std::ffi::c_char);
    resolve with crate::terminal::get_instance_proc_addr;
}
chain! {
    pub(crate) dispatch_get_physical_device_proc_addr, register_get_physical_device_proc_addr;
    observes vk_layerGetPhysicalDeviceProcAddr;
    chain GET_PHYSICAL_DEVICE_PROC_ADDR_CHAIN, context GetPhysicalDeviceProcAddrContext;
    hook GetPhysicalDeviceProcAddrHook() -> vk::PFN_vkVoidFunction;
    abi (instance: vk::Instance, name: *const std::ffi::c_char);
    resolve with crate::terminal::get_physical_device_proc_addr;
}
chain! {
    pub(crate) dispatch_get_device_proc_addr, register_get_device_proc_addr;
    observes vkGetDeviceProcAddr;
    chain GET_DEVICE_PROC_ADDR_CHAIN, context GetDeviceProcAddrContext;
    hook GetDeviceProcAddrHook() -> vk::PFN_vkVoidFunction;
    abi (device: vk::Device, name: *const std::ffi::c_char);
    resolve with crate::terminal::get_device_proc_addr;
}
chain! {
    pub(crate) dispatch_destroy_device, register_destroy_device;
    around vkDestroyDevice;
    chain DESTROY_DEVICE_CHAIN, context DestroyDeviceContext {
        capture_state: std::sync::Arc<capture::CaptureState> = crate::layer::device_capture(device),
    };
    hook DestroyDeviceHook(device: vk::Device, allocator: *const vk::AllocationCallbacks<'_>) -> ();
    resolve with device, c"vkDestroyDevice": vk::PFN_vkDestroyDevice -> ();
}

chain! {
    pub(crate) dispatch_begin_command_buffer, register_begin_command_buffer;
    observes vkBeginCommandBuffer;
    chain BEGIN_COMMAND_BUFFER_CHAIN, context BeginCommandBufferContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
    };
    hook BeginCommandBufferHook(command_buffer: vk::CommandBuffer) -> vk::Result;
    abi (command_buffer: vk::CommandBuffer, begin_info: *const vk::CommandBufferBeginInfo<'_>);
    resolve with command_buffer, c"vkBeginCommandBuffer": vk::PFN_vkBeginCommandBuffer -> vk::Result::ERROR_INITIALIZATION_FAILED;
}

chain! {
    pub(crate) dispatch_cmd_draw_indexed, register_cmd_draw_indexed;
    observes vkCmdDrawIndexed;
    chain CMD_DRAW_INDEXED_CHAIN, context CmdDrawIndexedContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
    };
    hook CmdDrawIndexedHook(command_buffer: vk::CommandBuffer);
    abi (
        command_buffer: vk::CommandBuffer,
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        vertex_offset: i32,
        first_instance: u32,
    );
    resolve with command_buffer, c"vkCmdDrawIndexed": vk::PFN_vkCmdDrawIndexed;
}

chain! {
    pub(crate) dispatch_cmd_set_viewport, register_cmd_set_viewport;
    observes vkCmdSetViewport;
    chain CMD_SET_VIEWPORT_CHAIN, context CmdSetViewportContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
    };
    hook CmdSetViewportHook(command_buffer: vk::CommandBuffer, first: u32, count: u32, viewports: *const vk::Viewport);
    resolve with command_buffer, c"vkCmdSetViewport": vk::PFN_vkCmdSetViewport;
}

chain! {
    pub(crate) dispatch_cmd_set_scissor, register_cmd_set_scissor;
    observes vkCmdSetScissor;
    chain CMD_SET_SCISSOR_CHAIN, context CmdSetScissorContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
    };
    hook CmdSetScissorHook(command_buffer: vk::CommandBuffer, first: u32, count: u32, scissors: *const vk::Rect2D);
    resolve with command_buffer, c"vkCmdSetScissor": vk::PFN_vkCmdSetScissor;
}

chain! {
    pub(crate) dispatch_cmd_end_render_pass, register_cmd_end_render_pass;
    observes vkCmdEndRenderPass;
    chain CMD_END_RENDER_PASS_CHAIN, context CmdEndRenderPassContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
    };
    hook CmdEndRenderPassHook(command_buffer: vk::CommandBuffer);
    resolve with command_buffer, c"vkCmdEndRenderPass": vk::PFN_vkCmdEndRenderPass;
}

chain! {
    pub(crate) dispatch_cmd_begin_render_pass, register_cmd_begin_render_pass;
    observes vkCmdBeginRenderPass;
    chain CMD_BEGIN_RENDER_PASS_CHAIN, context CmdBeginRenderPassContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
    };
    hook CmdBeginRenderPassHook(command_buffer: vk::CommandBuffer, begin: *const vk::RenderPassBeginInfo<'_>, contents: vk::SubpassContents);
    resolve with command_buffer, c"vkCmdBeginRenderPass": vk::PFN_vkCmdBeginRenderPass;
}

chain! {
    pub(crate) dispatch_cmd_bind_descriptor_sets, register_cmd_bind_descriptor_sets;
    observes vkCmdBindDescriptorSets;
    chain CMD_BIND_DESCRIPTOR_SETS_CHAIN, context CmdBindDescriptorSetsContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
    };
    hook CmdBindDescriptorSetsHook(command_buffer: vk::CommandBuffer, bind_point: vk::PipelineBindPoint, layout: vk::PipelineLayout, first_set: u32, set_count: u32, sets: *const vk::DescriptorSet, dynamic_offset_count: u32, dynamic_offsets: *const u32);
    resolve with command_buffer, c"vkCmdBindDescriptorSets": vk::PFN_vkCmdBindDescriptorSets;
}

chain! {
    pub(crate) dispatch_cmd_push_descriptor_set, register_cmd_push_descriptor_set;
    observes vkCmdPushDescriptorSetKHR;
    chain CMD_PUSH_DESCRIPTOR_SET_CHAIN, context CmdPushDescriptorSetContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
    };
    hook CmdPushDescriptorSetHook(command_buffer: vk::CommandBuffer, bind_point: vk::PipelineBindPoint, layout: vk::PipelineLayout, set: u32, write_count: u32, writes: *const vk::WriteDescriptorSet<'_>);
    resolve with command_buffer, c"vkCmdPushDescriptorSetKHR": vk::PFN_vkCmdPushDescriptorSetKHR;
}

chain! {
    pub(crate) dispatch_cmd_blit_image, register_cmd_blit_image;
    observes vkCmdBlitImage;
    chain CMD_BLIT_IMAGE_CHAIN, context CmdBlitImageContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
    };
    hook CmdBlitImageHook(command_buffer: vk::CommandBuffer, source_image: vk::Image, source_layout: vk::ImageLayout, destination_image: vk::Image, destination_layout: vk::ImageLayout, region_count: u32, regions: *const vk::ImageBlit, filter: vk::Filter);
    resolve with command_buffer, c"vkCmdBlitImage": vk::PFN_vkCmdBlitImage;
}

chain! {
    pub(crate) dispatch_cmd_copy_image, register_cmd_copy_image;
    observes vkCmdCopyImage;
    chain CMD_COPY_IMAGE_CHAIN, context CmdCopyImageContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::command_tracking(command_buffer),
    };
    hook CmdCopyImageHook(command_buffer: vk::CommandBuffer, source_image: vk::Image, source_layout: vk::ImageLayout, destination_image: vk::Image, destination_layout: vk::ImageLayout, region_count: u32, regions: *const vk::ImageCopy);
    resolve with command_buffer, c"vkCmdCopyImage": vk::PFN_vkCmdCopyImage;
}

chain! {
    pub(crate) dispatch_create_image, register_create_image;
    after vkCreateImage;
    chain CREATE_IMAGE_CHAIN, context CreateImageContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
    };
    hook CreateImageHook(create_info: *const vk::ImageCreateInfo<'_>, image: *mut vk::Image) -> result: vk::Result;
    abi (device: vk::Device, create_info: *const vk::ImageCreateInfo<'_>, allocator: *const vk::AllocationCallbacks<'_>, image: *mut vk::Image);
    resolve with crate::terminal::create_image;
}

chain! {
    pub(crate) dispatch_create_image_view, register_create_image_view;
    after vkCreateImageView;
    chain CREATE_IMAGE_VIEW_CHAIN, context CreateImageViewContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
    };
    hook CreateImageViewHook(create_info: *const vk::ImageViewCreateInfo<'_>, view: *mut vk::ImageView) -> result: vk::Result;
    abi (device: vk::Device, create_info: *const vk::ImageViewCreateInfo<'_>, allocator: *const vk::AllocationCallbacks<'_>, view: *mut vk::ImageView);
    resolve with device, c"vkCreateImageView": vk::PFN_vkCreateImageView -> vk::Result::ERROR_INITIALIZATION_FAILED;
}

chain! {
    pub(crate) dispatch_create_framebuffer, register_create_framebuffer;
    after vkCreateFramebuffer;
    chain CREATE_FRAMEBUFFER_CHAIN, context CreateFramebufferContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
    };
    hook CreateFramebufferHook(create_info: *const vk::FramebufferCreateInfo<'_>, framebuffer: *mut vk::Framebuffer) -> result: vk::Result;
    abi (device: vk::Device, create_info: *const vk::FramebufferCreateInfo<'_>, allocator: *const vk::AllocationCallbacks<'_>, framebuffer: *mut vk::Framebuffer);
    resolve with device, c"vkCreateFramebuffer": vk::PFN_vkCreateFramebuffer -> vk::Result::ERROR_INITIALIZATION_FAILED;
}

chain! {
    pub(crate) dispatch_destroy_image, register_destroy_image;
    observes vkDestroyImage;
    chain DESTROY_IMAGE_CHAIN, context DestroyImageContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
    };
    hook DestroyImageHook(image: vk::Image);
    abi (device: vk::Device, image: vk::Image, allocator: *const vk::AllocationCallbacks<'_>);
    resolve with device, c"vkDestroyImage": vk::PFN_vkDestroyImage;
}

chain! {
    pub(crate) dispatch_destroy_image_view, register_destroy_image_view;
    observes vkDestroyImageView;
    chain DESTROY_IMAGE_VIEW_CHAIN, context DestroyImageViewContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
    };
    hook DestroyImageViewHook(view: vk::ImageView);
    abi (device: vk::Device, view: vk::ImageView, allocator: *const vk::AllocationCallbacks<'_>);
    resolve with device, c"vkDestroyImageView": vk::PFN_vkDestroyImageView;
}

chain! {
    pub(crate) dispatch_destroy_framebuffer, register_destroy_framebuffer;
    observes vkDestroyFramebuffer;
    chain DESTROY_FRAMEBUFFER_CHAIN, context DestroyFramebufferContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
    };
    hook DestroyFramebufferHook(framebuffer: vk::Framebuffer);
    abi (device: vk::Device, framebuffer: vk::Framebuffer, allocator: *const vk::AllocationCallbacks<'_>);
    resolve with device, c"vkDestroyFramebuffer": vk::PFN_vkDestroyFramebuffer;
}

chain! {
    pub(crate) dispatch_update_descriptor_sets, register_update_descriptor_sets;
    observes vkUpdateDescriptorSets;
    chain UPDATE_DESCRIPTOR_SETS_CHAIN, context UpdateDescriptorSetsContext {
        tracking: std::sync::Arc<tracking::Tracking> = crate::layer::device_tracking(device),
    };
    hook UpdateDescriptorSetsHook(write_count: u32, writes: *const vk::WriteDescriptorSet<'_>);
    abi (device: vk::Device, write_count: u32, writes: *const vk::WriteDescriptorSet<'_>, copy_count: u32, copies: *const vk::CopyDescriptorSet<'_>);
    resolve with device, c"vkUpdateDescriptorSets": vk::PFN_vkUpdateDescriptorSets;
}
