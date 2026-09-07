use crate::layer::PhysicalDeviceState;
use ash::vk;
use std::{ffi::CStr, ptr};
mod extensions;
mod requested;
use extensions::Extensions;
pub use extensions::vkEnumerateDeviceExtensionProperties;
use requested::Requested;

/// Only missing feature nodes are added. Application-owned chains are borrowed
/// unchanged, including explicit false values and Vulkan 1.2/1.3 feature nodes.
#[derive(Default)]
pub(crate) struct Features {
    timeline: vk::PhysicalDeviceTimelineSemaphoreFeatures<'static>,
    synchronization2: vk::PhysicalDeviceSynchronization2Features<'static>,
    maintenance: vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT<'static>,
    add_timeline: bool,
    add_synchronization2: bool,
    add_maintenance: bool,
    pub supported: bool,
}
impl Features {
    pub(crate) unsafe fn virtual_device(info: &vk::DeviceCreateInfo<'_>) -> bool {
        if !super::enabled() || info.enabled_extension_count == 0 {
            return false;
        }
        unsafe {
            std::slice::from_raw_parts(
                info.pp_enabled_extension_names,
                info.enabled_extension_count as usize,
            )
        }
        .iter()
        .any(|&name| unsafe { CStr::from_ptr(name) } == ash::khr::swapchain::NAME)
    }

    unsafe fn query(physical: vk::PhysicalDevice, state: Option<&PhysicalDeviceState>) -> Self {
        let mut support = Self::default();
        let Some(state) = state else {
            return support;
        };
        let get: Option<vk::PFN_vkGetPhysicalDeviceFeatures2> = unsafe {
            crate::terminal::instance_proc(
                state.next_gipa,
                state.instance,
                c"vkGetPhysicalDeviceFeatures2",
            )
            .or_else(|| {
                crate::terminal::instance_proc(
                    state.next_gipa,
                    state.instance,
                    c"vkGetPhysicalDeviceFeatures2KHR",
                )
            })
        };
        if let Some(get) = get {
            let mut features = vk::PhysicalDeviceFeatures2::default()
                .push_next(&mut support.timeline)
                .push_next(&mut support.synchronization2);
            if super::enabled() {
                features = features.push_next(&mut support.maintenance);
            }
            unsafe {
                get(physical, &mut features);
            }
        }
        support.timeline.p_next = ptr::null_mut();
        support.synchronization2.p_next = ptr::null_mut();
        support.maintenance.p_next = ptr::null_mut();
        support
    }
    pub(crate) unsafe fn new(
        physical: vk::PhysicalDevice,
        state: Option<&PhysicalDeviceState>,
        info: *const vk::DeviceCreateInfo<'_>,
        extensions: bool,
    ) -> Result<Self, vk::Result> {
        let virtualized = unsafe { Self::virtual_device(&*info) };
        let mut support = unsafe { Self::query(physical, state) };
        let requested = unsafe { Requested::new(&*info) };
        support.supported = extensions
            && support.timeline.timeline_semaphore == vk::TRUE
            && requested.timeline != Some(vk::FALSE);
        let virtual_supported = support.supported
            && support.maintenance.swapchain_maintenance1 == vk::TRUE
            && support.synchronization2.synchronization2 == vk::TRUE
            && requested.sync2 != Some(vk::FALSE)
            && requested.maintenance != Some(vk::FALSE)
            && requested.devices == 1;
        if virtualized && !virtual_supported {
            return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
        }
        if virtualized {
            unsafe {
                Requested::validate_extensions(&*info)?;
            }
        }
        support.add_timeline = support.supported && requested.timeline.is_none();
        support.add_synchronization2 = virtualized && requested.sync2.is_none();
        support.add_maintenance = virtualized && requested.maintenance.is_none();
        Ok(support)
    }
    pub(crate) fn attach(&mut self, info: &mut vk::DeviceCreateInfo<'_>) {
        if self.add_maintenance {
            self.maintenance.p_next = info.p_next.cast_mut();
            info.p_next = ptr::from_ref(&self.maintenance).cast();
        }
        if self.add_timeline {
            self.timeline.p_next = info.p_next.cast_mut();
            info.p_next = ptr::from_ref(&self.timeline).cast();
        }
        if self.add_synchronization2 {
            self.synchronization2.p_next = info.p_next.cast_mut();
            info.p_next = ptr::from_ref(&self.synchronization2).cast();
        }
    }
}
