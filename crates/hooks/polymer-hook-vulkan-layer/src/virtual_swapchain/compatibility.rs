use super::*;
use crate::layer::PhysicalDeviceState;
use std::ffi::CStr;

pub(super) struct Compatibility;
impl Compatibility {
    pub(super) fn device_hook(name: &[u8]) -> vk::PFN_vkVoidFunction {
        crate::hook(match name {
            b"vkGetDeviceGroupPresentCapabilitiesKHR" => Self::group_capabilities as *const (),
            b"vkGetDeviceGroupSurfacePresentModesKHR" => Self::group_modes as *const (),
            b"vkCreateImage" => Self::create_image as *const (),
            b"vkSetDebugUtilsObjectNameEXT" => Self::debug_name as *const (),
            b"vkSetDebugUtilsObjectTagEXT" => Self::debug_tag as *const (),
            b"vkDebugMarkerSetObjectNameEXT" => Self::marker_name as *const (),
            b"vkDebugMarkerSetObjectTagEXT" => Self::marker_tag as *const (),
            _ => return None,
        })
    }
    pub(super) fn instance_hook(name: &[u8]) -> vk::PFN_vkVoidFunction {
        crate::hook(match name {
            b"vkEnumeratePhysicalDeviceGroups" | b"vkEnumeratePhysicalDeviceGroupsKHR" => {
                Self::groups as *const ()
            }
            b"vkGetPhysicalDevicePresentRectanglesKHR" => Self::rectangles as *const (),
            _ => return None,
        })
    }
    unsafe fn device_proc<T: Copy>(device: vk::Device, name: &CStr) -> Option<T> {
        crate::layer::device_gdpa(device)
            .and_then(|g| unsafe { crate::terminal::device_proc(g, device, name) })
    }
    unsafe extern "system" fn groups(
        instance: vk::Instance,
        count: *mut u32,
        output: *mut vk::PhysicalDeviceGroupProperties<'_>,
    ) -> vk::Result {
        let Some(gipa) = crate::layer::instance_gipa(instance) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let Some(next): Option<vk::PFN_vkEnumeratePhysicalDeviceGroups> = (unsafe {
            crate::terminal::instance_proc(gipa, instance, c"vkEnumeratePhysicalDeviceGroups")
                .or_else(|| {
                    crate::terminal::instance_proc(
                        gipa,
                        instance,
                        c"vkEnumeratePhysicalDeviceGroupsKHR",
                    )
                })
        }) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let result = unsafe { next(instance, count, output) };
        if !output.is_null() && matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE) {
            let state = crate::layer::hook_state();
            let capture = unsafe { crate::layer::dispatch_key(instance) }
                .and_then(|key| {
                    state
                        .instance
                        .get(&key)
                        .map(|entry| entry.capture_state.clone())
                })
                .unwrap_or_default();
            for group in unsafe { std::slice::from_raw_parts(output, *count as usize) } {
                for &physical in &group.physical_devices[..group.physical_device_count as usize] {
                    state.physical_device.insert(
                        physical,
                        PhysicalDeviceState {
                            instance,
                            next_gipa: gipa,
                            capture_state: capture.clone(),
                        },
                    );
                }
            }
        }
        result
    }
    unsafe extern "system" fn rectangles(
        physical: vk::PhysicalDevice,
        surface: vk::SurfaceKHR,
        count: *mut u32,
        output: *mut vk::Rect2D,
    ) -> vk::Result {
        if let Some(binding) = Surface::get(surface).and_then(|surface| surface.binding()) {
            let rectangle = vk::Rect2D::default().extent(vk::Extent2D {
                width: binding.extent().width,
                height: binding.extent().height,
            });
            return unsafe { WsiResult::enumerate(&[rectangle], count, output) };
        }
        let Some(next): Option<vk::PFN_vkGetPhysicalDevicePresentRectanglesKHR> =
            (unsafe { Surface::proc(physical, c"vkGetPhysicalDevicePresentRectanglesKHR") })
        else {
            return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
        };
        unsafe { next(physical, surface, count, output) }
    }
    unsafe extern "system" fn group_capabilities(
        device: vk::Device,
        output: *mut vk::DeviceGroupPresentCapabilitiesKHR<'_>,
    ) -> vk::Result {
        if Device::get(device).is_ok() {
            unsafe {
                (*output).present_mask.fill(0);
                (*output).present_mask[0] = 1;
                (*output).modes = vk::DeviceGroupPresentModeFlagsKHR::LOCAL;
            }
            return vk::Result::SUCCESS;
        }
        let Some(next): Option<vk::PFN_vkGetDeviceGroupPresentCapabilitiesKHR> =
            (unsafe { Self::device_proc(device, c"vkGetDeviceGroupPresentCapabilitiesKHR") })
        else {
            return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
        };
        unsafe { next(device, output) }
    }
    unsafe extern "system" fn group_modes(
        device: vk::Device,
        surface: vk::SurfaceKHR,
        output: *mut vk::DeviceGroupPresentModeFlagsKHR,
    ) -> vk::Result {
        if Surface::get(surface)
            .and_then(|surface| surface.binding())
            .is_some()
        {
            unsafe {
                *output = vk::DeviceGroupPresentModeFlagsKHR::LOCAL;
            }
            return vk::Result::SUCCESS;
        }
        let Some(next): Option<vk::PFN_vkGetDeviceGroupSurfacePresentModesKHR> =
            (unsafe { Self::device_proc(device, c"vkGetDeviceGroupSurfacePresentModesKHR") })
        else {
            return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
        };
        unsafe { next(device, surface, output) }
    }
    unsafe extern "system" fn create_image(
        device: vk::Device,
        info: *const vk::ImageCreateInfo<'_>,
        allocator: *const vk::AllocationCallbacks<'_>,
        output: *mut vk::Image,
    ) -> vk::Result {
        let mut current = unsafe { (*info).p_next.cast::<vk::BaseInStructure<'_>>() };
        while let Some(base) = unsafe { current.as_ref() } {
            if base.s_type == vk::StructureType::IMAGE_SWAPCHAIN_CREATE_INFO_KHR
                && SWAPCHAINS.contains_key(&unsafe {
                    (*current.cast::<vk::ImageSwapchainCreateInfoKHR<'_>>()).swapchain
                })
            {
                // Swapchain-backed alias images need a separate memory-binding
                // contract. Never pass a virtual swapchain handle to a driver.
                return vk::Result::ERROR_FEATURE_NOT_PRESENT;
            }
            current = base.p_next;
        }
        let Some(next): Option<vk::PFN_vkCreateImage> =
            (unsafe { Self::device_proc(device, c"vkCreateImage") })
        else {
            return vk::Result::ERROR_DEVICE_LOST;
        };
        unsafe { next(device, info, allocator, output) }
    }
    unsafe extern "system" fn debug_name(
        device: vk::Device,
        info: *const vk::DebugUtilsObjectNameInfoEXT<'_>,
    ) -> vk::Result {
        if unsafe {
            (*info).object_type == vk::ObjectType::SWAPCHAIN_KHR
                && SWAPCHAINS.contains_key(&vk::SwapchainKHR::from_raw((*info).object_handle))
        } {
            return vk::Result::SUCCESS;
        }
        let Some(next): Option<vk::PFN_vkSetDebugUtilsObjectNameEXT> =
            (unsafe { Self::device_proc(device, c"vkSetDebugUtilsObjectNameEXT") })
        else {
            return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
        };
        unsafe { next(device, info) }
    }
    unsafe extern "system" fn debug_tag(
        device: vk::Device,
        info: *const vk::DebugUtilsObjectTagInfoEXT<'_>,
    ) -> vk::Result {
        if unsafe {
            (*info).object_type == vk::ObjectType::SWAPCHAIN_KHR
                && SWAPCHAINS.contains_key(&vk::SwapchainKHR::from_raw((*info).object_handle))
        } {
            return vk::Result::SUCCESS;
        }
        let Some(next): Option<vk::PFN_vkSetDebugUtilsObjectTagEXT> =
            (unsafe { Self::device_proc(device, c"vkSetDebugUtilsObjectTagEXT") })
        else {
            return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
        };
        unsafe { next(device, info) }
    }
    unsafe extern "system" fn marker_name(
        device: vk::Device,
        info: *const vk::DebugMarkerObjectNameInfoEXT<'_>,
    ) -> vk::Result {
        if unsafe {
            (*info).object_type == vk::DebugReportObjectTypeEXT::SWAPCHAIN_KHR
                && SWAPCHAINS.contains_key(&vk::SwapchainKHR::from_raw((*info).object))
        } {
            return vk::Result::SUCCESS;
        }
        let Some(next): Option<vk::PFN_vkDebugMarkerSetObjectNameEXT> =
            (unsafe { Self::device_proc(device, c"vkDebugMarkerSetObjectNameEXT") })
        else {
            return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
        };
        unsafe { next(device, info) }
    }
    unsafe extern "system" fn marker_tag(
        device: vk::Device,
        info: *const vk::DebugMarkerObjectTagInfoEXT<'_>,
    ) -> vk::Result {
        if unsafe {
            (*info).object_type == vk::DebugReportObjectTypeEXT::SWAPCHAIN_KHR
                && SWAPCHAINS.contains_key(&vk::SwapchainKHR::from_raw((*info).object))
        } {
            return vk::Result::SUCCESS;
        }
        let Some(next): Option<vk::PFN_vkDebugMarkerSetObjectTagEXT> =
            (unsafe { Self::device_proc(device, c"vkDebugMarkerSetObjectTagEXT") })
        else {
            return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
        };
        unsafe { next(device, info) }
    }
}
