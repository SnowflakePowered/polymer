//! Vulkan loader layer for native zero-copy swapchain presentation.
mod dispatch;
mod hook_chain;
mod hooks;
mod layer;
mod swapchain;
mod terminal;
pub use hooks::{
    vk_layerGetPhysicalDeviceProcAddr, vkCreateDevice, vkCreateInstance, vkCreateSwapchainKHR,
    vkCreateWin32SurfaceKHR, vkDestroyDevice, vkDestroyInstance, vkDestroySurfaceKHR,
    vkDestroySwapchainKHR, vkEnumeratePhysicalDevices, vkGetDeviceProcAddr, vkGetDeviceQueue,
    vkGetDeviceQueue2, vkGetInstanceProcAddr, vkGetSwapchainImagesKHR,
    vkNegotiateLoaderLayerInterfaceVersion, vkQueuePresentKHR,
};

use ash::vk;
use std::mem;

fn hook(pointer: *const ()) -> vk::PFN_vkVoidFunction {
    Some(unsafe { mem::transmute::<*const (), unsafe extern "system" fn()>(pointer) })
}

/// Central registration table for every entry point owned by the layer.
pub(crate) fn registered_instance_hook(name: &[u8]) -> vk::PFN_vkVoidFunction {
    if let Some(hook) = swapchain::instance_hook(name).or_else(|| swapchain::device_hook(name)) {
        return Some(hook);
    }
    match name {
        b"vkGetInstanceProcAddr" => hook(hooks::vkGetInstanceProcAddr as *const ()),
        b"vkCreateInstance" => hook(hooks::vkCreateInstance as *const ()),
        b"vkDestroyInstance" => hook(hooks::vkDestroyInstance as *const ()),
        b"vkCreateDevice" => hook(hooks::vkCreateDevice as *const ()),
        b"vkEnumeratePhysicalDevices" => hook(hooks::vkEnumeratePhysicalDevices as *const ()),
        b"vkCreateWin32SurfaceKHR" => hook(hooks::vkCreateWin32SurfaceKHR as *const ()),
        b"vkDestroySurfaceKHR" => hook(hooks::vkDestroySurfaceKHR as *const ()),
        b"vkGetDeviceProcAddr" => hook(hooks::vkGetDeviceProcAddr as *const ()),
        _ => registered_device_hook(name),
    }
}

pub(crate) fn registered_device_hook(name: &[u8]) -> vk::PFN_vkVoidFunction {
    if let Some(hook) = swapchain::device_hook(name) {
        return Some(hook);
    }

    match name {
        b"vkGetDeviceProcAddr" => hook(hooks::vkGetDeviceProcAddr as *const ()),
        b"vkDestroyDevice" => hook(hooks::vkDestroyDevice as *const ()),
        b"vkCreateSwapchainKHR" => hook(hooks::vkCreateSwapchainKHR as *const ()),
        b"vkDestroySwapchainKHR" => hook(hooks::vkDestroySwapchainKHR as *const ()),
        b"vkGetSwapchainImagesKHR" => hook(hooks::vkGetSwapchainImagesKHR as *const ()),
        b"vkQueuePresentKHR" => hook(hooks::vkQueuePresentKHR as *const ()),
        b"vkGetDeviceQueue" => hook(hooks::vkGetDeviceQueue as *const ()),
        b"vkGetDeviceQueue2" => hook(hooks::vkGetDeviceQueue2 as *const ()),
        _ => None,
    }
}
