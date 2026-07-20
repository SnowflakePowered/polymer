//! Vulkan loader layer that transparently observes swapchain presentation.

mod hook_chain;
mod hooks;
mod layer;
mod observers;
mod swapchain;
mod terminal;

// The Vulkan loader discovers these symbols directly from the layer DLL.
pub use hooks::vkCreateDevice;
pub use hooks::vkCreateInstance;
pub use hooks::vkEnumeratePhysicalDevices;
pub use hooks::vkQueuePresentKHR;
pub use hooks::{
    vk_layerGetPhysicalDeviceProcAddr, vkGetDeviceProcAddr, vkGetInstanceProcAddr,
    vkNegotiateLoaderLayerInterfaceVersion,
};
pub use hooks::{
    vkBeginCommandBuffer, vkCmdBeginRenderPass, vkCmdBindDescriptorSets, vkCmdBlitImage,
    vkCmdCopyImage, vkCmdDraw, vkCmdDrawIndexed, vkCmdEndRenderPass, vkCmdPushDescriptorSetKHR,
    vkCmdSetScissor, vkCmdSetViewport,
};
pub use hooks::{
    vkCreateFramebuffer, vkCreateImage, vkCreateImageView, vkDestroyFramebuffer, vkDestroyImage,
    vkDestroyImageView, vkGetDeviceQueue, vkGetDeviceQueue2, vkQueueSubmit, vkUpdateDescriptorSets,
};
pub use hooks::{vkCreateSwapchainKHR, vkDestroySwapchainKHR, vkGetSwapchainImagesKHR};
pub use hooks::{vkCreateWin32SurfaceKHR, vkDestroySurfaceKHR};
pub use hooks::{vkDestroyDevice, vkDestroyInstance};

use ash::vk;
use std::mem;

fn hook(pointer: *const ()) -> vk::PFN_vkVoidFunction {
    Some(unsafe { mem::transmute(pointer) })
}

/// Central registration table for every entry point owned by the layer.
pub(crate) fn registered_instance_hook(name: &[u8]) -> vk::PFN_vkVoidFunction {
    match name {
        b"vkGetInstanceProcAddr" => hook(hooks::vkGetInstanceProcAddr as *const ()),
        b"vkCreateInstance" => hook(hooks::vkCreateInstance as *const ()),
        b"vkDestroyInstance" => hook(hooks::vkDestroyInstance as *const ()),
        b"vkCreateDevice" => hook(hooks::vkCreateDevice as *const ()),
        b"vkEnumeratePhysicalDevices" => hook(hooks::vkEnumeratePhysicalDevices as *const ()),
        b"vkCreateWin32SurfaceKHR" => hook(hooks::vkCreateWin32SurfaceKHR as *const ()),
        b"vkDestroySurfaceKHR" => hook(hooks::vkDestroySurfaceKHR as *const ()),
        b"vkGetDeviceProcAddr" => hook(hooks::vkGetDeviceProcAddr as *const ()),
        _ => None,
    }
}

pub(crate) fn registered_device_hook(name: &[u8], tracking: bool) -> vk::PFN_vkVoidFunction {
    let always = match name {
        b"vkGetDeviceProcAddr" => hook(hooks::vkGetDeviceProcAddr as *const ()),
        b"vkDestroyDevice" => hook(hooks::vkDestroyDevice as *const ()),
        b"vkCreateSwapchainKHR" => hook(hooks::vkCreateSwapchainKHR as *const ()),
        b"vkDestroySwapchainKHR" => hook(hooks::vkDestroySwapchainKHR as *const ()),
        b"vkGetSwapchainImagesKHR" => hook(hooks::vkGetSwapchainImagesKHR as *const ()),
        b"vkQueuePresentKHR" => hook(hooks::vkQueuePresentKHR as *const ()),
        b"vkGetDeviceQueue" => hook(hooks::vkGetDeviceQueue as *const ()),
        b"vkGetDeviceQueue2" => hook(hooks::vkGetDeviceQueue2 as *const ()),
        _ => None,
    };
    always.or_else(|| {
        tracking.then_some(())?;
        match name {
            b"vkQueueSubmit" => hook(hooks::vkQueueSubmit as *const ()),
            b"vkBeginCommandBuffer" => hook(hooks::vkBeginCommandBuffer as *const ()),
            b"vkCreateImage" => hook(hooks::vkCreateImage as *const ()),
            b"vkDestroyImage" => hook(hooks::vkDestroyImage as *const ()),
            b"vkCreateImageView" => hook(hooks::vkCreateImageView as *const ()),
            b"vkDestroyImageView" => hook(hooks::vkDestroyImageView as *const ()),
            b"vkCreateFramebuffer" => hook(hooks::vkCreateFramebuffer as *const ()),
            b"vkDestroyFramebuffer" => hook(hooks::vkDestroyFramebuffer as *const ()),
            b"vkUpdateDescriptorSets" => hook(hooks::vkUpdateDescriptorSets as *const ()),
            b"vkCmdPushDescriptorSetKHR" => hook(hooks::vkCmdPushDescriptorSetKHR as *const ()),
            b"vkCmdBeginRenderPass" => hook(hooks::vkCmdBeginRenderPass as *const ()),
            b"vkCmdEndRenderPass" => hook(hooks::vkCmdEndRenderPass as *const ()),
            b"vkCmdBindDescriptorSets" => hook(hooks::vkCmdBindDescriptorSets as *const ()),
            b"vkCmdSetViewport" => hook(hooks::vkCmdSetViewport as *const ()),
            b"vkCmdSetScissor" => hook(hooks::vkCmdSetScissor as *const ()),
            b"vkCmdDraw" => hook(hooks::vkCmdDraw as *const ()),
            b"vkCmdDrawIndexed" => hook(hooks::vkCmdDrawIndexed as *const ()),
            b"vkCmdBlitImage" => hook(hooks::vkCmdBlitImage as *const ()),
            b"vkCmdCopyImage" => hook(hooks::vkCmdCopyImage as *const ()),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn registers_core_layer_entry_points() {
        assert!(super::registered_instance_hook(b"vkCreateInstance").is_some());
        assert!(super::registered_device_hook(b"vkQueuePresentKHR", false).is_some());
        assert!(super::registered_device_hook(b"vkCmdSetScissor", true).is_some());
        assert!(super::registered_device_hook(b"vkCmdBlitImage", true).is_some());
        assert!(super::registered_device_hook(b"vkCmdSetScissor", false).is_none());
        assert!(super::registered_device_hook(b"vkNotARealCommand", true).is_none());
    }
}
