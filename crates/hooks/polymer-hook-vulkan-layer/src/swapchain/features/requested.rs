use super::*;

pub(super) struct Requested {
    pub timeline: Option<vk::Bool32>,
    pub sync2: Option<vk::Bool32>,
    pub maintenance: Option<vk::Bool32>,
    pub devices: u32,
    pub vulkan12: Option<ptr::NonNull<vk::PhysicalDeviceVulkan12Features<'static>>>,
    pub vulkan13: Option<ptr::NonNull<vk::PhysicalDeviceVulkan13Features<'static>>>,
}
impl Requested {
    pub(super) unsafe fn new(info: &vk::DeviceCreateInfo<'_>) -> Self {
        let mut requested = Self {
            timeline: None,
            sync2: None,
            maintenance: None,
            devices: 1,
            vulkan12: None,
            vulkan13: None,
        };
        let mut current = info.p_next.cast::<vk::BaseInStructure<'_>>();
        while let Some(base) = unsafe { current.as_ref() } {
            match base.s_type {
                vk::StructureType::PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_FEATURES => {
                    requested.timeline = Some(unsafe {
                        (*current.cast::<vk::PhysicalDeviceTimelineSemaphoreFeatures<'_>>())
                            .timeline_semaphore
                    })
                }
                vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_2_FEATURES => {
                    let node = current
                        .cast_mut()
                        .cast::<vk::PhysicalDeviceVulkan12Features<'static>>();
                    requested.vulkan12 = ptr::NonNull::new(node);
                    requested.timeline =
                        (unsafe { (*node).timeline_semaphore } == vk::TRUE).then_some(vk::TRUE);
                }
                vk::StructureType::PHYSICAL_DEVICE_SYNCHRONIZATION_2_FEATURES => {
                    requested.sync2 = Some(unsafe {
                        (*current.cast::<vk::PhysicalDeviceSynchronization2Features<'_>>())
                            .synchronization2
                    })
                }
                vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_3_FEATURES => {
                    let node = current
                        .cast_mut()
                        .cast::<vk::PhysicalDeviceVulkan13Features<'static>>();
                    requested.vulkan13 = ptr::NonNull::new(node);
                    requested.sync2 =
                        (unsafe { (*node).synchronization2 } == vk::TRUE).then_some(vk::TRUE);
                }
                vk::StructureType::PHYSICAL_DEVICE_SWAPCHAIN_MAINTENANCE_1_FEATURES_EXT => {
                    requested.maintenance = Some(unsafe {
                        (*current.cast::<vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT<'_>>())
                            .swapchain_maintenance1
                    })
                }
                vk::StructureType::DEVICE_GROUP_DEVICE_CREATE_INFO => {
                    requested.devices = unsafe {
                        (*current.cast::<vk::DeviceGroupDeviceCreateInfo<'_>>())
                            .physical_device_count
                    }
                }
                _ => {}
            }
            current = base.p_next;
        }
        requested
    }
    pub(super) unsafe fn validate_extensions(
        info: &vk::DeviceCreateInfo<'_>,
    ) -> Result<(), vk::Result> {
        let names = if info.enabled_extension_count == 0 {
            &[]
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    info.pp_enabled_extension_names,
                    info.enabled_extension_count as usize,
                )
            }
        };
        for &name in names {
            let name = unsafe { CStr::from_ptr(name) };
            if Extensions::unsupported(name.to_bytes()) {
                tracing::error!(extension = %name.to_string_lossy(), "unsupported virtual WSI extension requested");
                return Err(vk::Result::ERROR_EXTENSION_NOT_PRESENT);
            }
        }
        Ok(())
    }
}
