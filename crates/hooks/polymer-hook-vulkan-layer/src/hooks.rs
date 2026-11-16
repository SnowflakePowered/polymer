//! Declarations for typed Vulkan hook chains.

use crate::hook_chain::vulkan_terminal;
use crate::layer;
use ash::vk;

polymer_hook::chain_hooks! {
    #![terminal_resolver(vulkan_terminal)]
    #![order(forward)]
    #[context]
    pub(crate) struct VulkanHookContext {
        pub(crate) state: std::sync::Arc<crate::layer::VulkanHookState> = crate::layer::hook_state(),
    }

    #[export(vkGetDeviceQueue)]
    #[terminal(dynamic(device, c"vkGetDeviceQueue": vk::PFN_vkGetDeviceQueue), fallback = ())]
    #[postfix(|_result, context| context.common.state.queue_created(device, queue))]
    pub(crate) unsafe extern "system" fn get_device_queue(
        device: vk::Device,
        queue_family_index: u32,
        queue_index: u32,
        queue: *mut vk::Queue,
    );

    #[export(vkGetDeviceQueue2)]
    #[terminal(dynamic(device, c"vkGetDeviceQueue2": vk::PFN_vkGetDeviceQueue2), fallback = ())]
    #[postfix(|_result, context| context.common.state.queue_created(device, queue))]
    pub(crate) unsafe extern "system" fn get_device_queue2(
        device: vk::Device,
        queue_info: *const vk::DeviceQueueInfo2<'_>,
        queue: *mut vk::Queue,
    );

    #[export(vkCreateSwapchainKHR)]
    #[terminal(direct = crate::swapchain::create_swapchain)]
    pub(crate) unsafe extern "system" fn create_swapchain(
        device: vk::Device,
        create_info: *const vk::SwapchainCreateInfoKHR<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        swapchain: *mut vk::SwapchainKHR,
    ) -> vk::Result;

    #[export(vkDestroySwapchainKHR)]
    #[terminal(direct = crate::swapchain::destroy_swapchain)]
    pub(crate) unsafe extern "system" fn destroy_swapchain(
        device: vk::Device,
        swapchain: vk::SwapchainKHR,
        allocator: *const vk::AllocationCallbacks<'_>,
    );

    #[export(vkGetSwapchainImagesKHR)]
    #[terminal(direct = crate::swapchain::swapchain_images)]
    pub(crate) unsafe extern "system" fn get_swapchain_images(
        device: vk::Device,
        swapchain: vk::SwapchainKHR,
        image_count: *mut u32,
        images: *mut vk::Image,
    ) -> vk::Result;

    #[export(vkQueuePresentKHR)]
    #[terminal(direct = crate::swapchain::queue_present)]
    pub(crate) unsafe extern "system" fn queue_present(
        queue: vk::Queue,
        present_info: *const vk::PresentInfoKHR<'_>,
    ) -> vk::Result;

    #[export(vkCreateWin32SurfaceKHR)]
    #[terminal(dynamic(instance, c"vkCreateWin32SurfaceKHR": vk::PFN_vkCreateWin32SurfaceKHR), fallback = vk::Result::ERROR_EXTENSION_NOT_PRESENT)]
    #[postfix(|result, _context| {
        if result == vk::Result::SUCCESS && !create_info.is_null() && !surface.is_null() {
            crate::swapchain::Surface::created(instance, unsafe { *surface }, unsafe { (*create_info).hwnd });
        }
    })]
    pub(crate) unsafe extern "system" fn create_win32_surface(
        instance: vk::Instance,
        create_info: *const vk::Win32SurfaceCreateInfoKHR<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        surface: *mut vk::SurfaceKHR,
    ) -> vk::Result;

    #[export(vkDestroySurfaceKHR)]
    #[terminal(dynamic(instance, c"vkDestroySurfaceKHR": vk::PFN_vkDestroySurfaceKHR), fallback = ())]
    #[prefix(|_context| crate::swapchain::Surface::destroyed(surface))]
    pub(crate) unsafe extern "system" fn destroy_surface(
        instance: vk::Instance,
        surface: vk::SurfaceKHR,
        allocator: *const vk::AllocationCallbacks<'_>,
    );

    #[export(vkEnumeratePhysicalDevices)]
    #[terminal(dynamic(instance, c"vkEnumeratePhysicalDevices": vk::PFN_vkEnumeratePhysicalDevices), fallback = vk::Result::ERROR_INITIALIZATION_FAILED)]
    #[postfix(|result, context| context.common.state.physical_devices_enumerated(instance, count, devices, result))]
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
    #[postfix(|result, _context| tracing::debug!(?result, "downstream vkCreateInstance completed"))]
    pub(crate) unsafe extern "system" fn create_instance(
        create_info: *const vk::InstanceCreateInfo<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        instance: *mut vk::Instance,
    ) -> vk::Result;

    #[export(vkCreateDevice)]
    #[terminal(direct = crate::terminal::create_device)]
    pub(crate) unsafe extern "system" fn create_device(
        physical_device: vk::PhysicalDevice,
        create_info: *const vk::DeviceCreateInfo<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        device: *mut vk::Device,
    ) -> vk::Result;

    #[export(vkGetInstanceProcAddr)]
    #[terminal(direct = crate::terminal::get_instance_proc_addr)]
    pub(crate) unsafe extern "system" fn get_instance_proc_addr(
        instance: vk::Instance,
        name: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction;

    #[export(vk_layerGetPhysicalDeviceProcAddr)]
    #[terminal(direct = crate::terminal::get_physical_device_proc_addr)]
    pub(crate) unsafe extern "system" fn get_physical_device_proc_addr(
        instance: vk::Instance,
        name: *const std::ffi::c_char,
    ) -> vk::PFN_vkVoidFunction;

    #[export(vkGetDeviceProcAddr)]
    #[terminal(direct = crate::terminal::get_device_proc_addr)]
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

}

polymer_hook::chain_hooks! {
    #![order(forward)]

    #[export(vkNegotiateLoaderLayerInterfaceVersion)]
    #[terminal(direct = crate::terminal::negotiate)]
    pub(crate) unsafe extern "system" fn negotiate(
        interface: *mut crate::layer::VkNegotiateLayerInterface,
    ) -> vk::Result;
}
