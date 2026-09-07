use super::*;

pub(super) struct Extensions;
impl Extensions {
    pub(super) fn unsupported(name: &[u8]) -> bool {
        [
            b"VK_KHR_present_id".as_slice(),
            b"VK_KHR_present_wait",
            b"VK_KHR_present_id2",
            b"VK_KHR_present_wait2",
            b"VK_EXT_swapchain_maintenance1",
            b"VK_KHR_swapchain_maintenance1",
            b"VK_EXT_full_screen_exclusive",
            b"VK_KHR_shared_presentable_image",
            b"VK_GOOGLE_display_timing",
            b"VK_EXT_present_timing",
            b"VK_KHR_swapchain_mutable_format",
            b"VK_EXT_hdr_metadata",
            b"VK_EXT_display_control",
            b"VK_AMD_display_native_hdr",
            b"VK_KHR_display_swapchain",
        ]
        .contains(&name)
    }
}

/// Device extensions describe the whole virtual WSI profile, including devices
/// that also contain native swapchains. Do not advertise unsupported WSI paths.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkEnumerateDeviceExtensionProperties(
    physical: vk::PhysicalDevice,
    layer: *const std::ffi::c_char,
    count: *mut u32,
    output: *mut vk::ExtensionProperties,
) -> vk::Result {
    let Some(state) = crate::layer::physical_device_state(physical) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    let Some(next): Option<vk::PFN_vkEnumerateDeviceExtensionProperties> = (unsafe {
        crate::terminal::instance_proc(
            state.next_gipa,
            state.instance,
            c"vkEnumerateDeviceExtensionProperties",
        )
    }) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    if !layer.is_null() {
        return unsafe { next(physical, layer, count, output) };
    }
    let mut available = 0;
    let result = unsafe { next(physical, layer, &mut available, ptr::null_mut()) };
    if result != vk::Result::SUCCESS {
        return result;
    }
    let mut properties = vec![vk::ExtensionProperties::default(); available as usize];
    let result = unsafe { next(physical, layer, &mut available, properties.as_mut_ptr()) };
    if result != vk::Result::SUCCESS {
        return result;
    }
    properties.truncate(available as usize);
    properties.retain(|property| {
        !Extensions::unsupported(
            unsafe { CStr::from_ptr(property.extension_name.as_ptr()) }.to_bytes(),
        )
    });
    unsafe { crate::virtual_swapchain::WsiResult::enumerate(&properties, count, output) }
}
