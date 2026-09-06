use ash::vk;

#[derive(Debug, PartialEq)]
pub(crate) enum FeatureNegotiation {
    Disabled,
    Existing,
    Inject,
}

impl FeatureNegotiation {
    pub(crate) unsafe fn swapchain_maintenance(
        info: &vk::DeviceCreateInfo<'_>,
        physical: vk::PhysicalDevice,
        query: Option<vk::PFN_vkGetPhysicalDeviceFeatures2>,
    ) -> Self {
        let mut node = info.p_next.cast::<vk::BaseInStructure<'_>>();
        while !node.is_null() {
            if unsafe { (*node).s_type }
                == vk::StructureType::PHYSICAL_DEVICE_SWAPCHAIN_MAINTENANCE_1_FEATURES_EXT
            {
                return if unsafe {
                    (*node.cast::<vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT<'_>>())
                        .swapchain_maintenance1
                } == vk::TRUE
                {
                    Self::Existing
                } else {
                    Self::Disabled
                };
            }
            node = unsafe { (*node).p_next };
        }
        let Some(query) = query else {
            return Self::Disabled;
        };
        let mut maintenance = vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT::default();
        let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut maintenance);
        unsafe { query(physical, &mut features) };
        if maintenance.swapchain_maintenance1 == vk::TRUE {
            Self::Inject
        } else {
            Self::Disabled
        }
    }

    /// The create-info chain is borrowed unchanged from a valid Vulkan call.
    /// Respect explicit application settings, including the promoted 1.2 node.
    pub(crate) unsafe fn timeline(
        info: &vk::DeviceCreateInfo<'_>,
        physical: vk::PhysicalDevice,
        query: Option<vk::PFN_vkGetPhysicalDeviceFeatures2>,
    ) -> Self {
        let mut node = info.p_next.cast::<vk::BaseInStructure<'_>>();
        while !node.is_null() {
            let requested = unsafe {
                match (*node).s_type {
                    vk::StructureType::PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_FEATURES => Some(
                        (*node.cast::<vk::PhysicalDeviceTimelineSemaphoreFeatures<'_>>())
                            .timeline_semaphore,
                    ),
                    vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_2_FEATURES => Some(
                        (*node.cast::<vk::PhysicalDeviceVulkan12Features<'_>>()).timeline_semaphore,
                    ),
                    _ => None,
                }
            };
            if let Some(requested) = requested {
                return if requested == vk::TRUE {
                    Self::Existing
                } else {
                    Self::Disabled
                };
            }
            node = unsafe { (*node).p_next };
        }
        let Some(query) = query else {
            return Self::Disabled;
        };
        let mut timeline = vk::PhysicalDeviceTimelineSemaphoreFeatures::default();
        let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut timeline);
        unsafe { query(physical, &mut features) };
        if timeline.timeline_semaphore == vk::TRUE {
            Self::Inject
        } else {
            Self::Disabled
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "system" fn supported(
        _: vk::PhysicalDevice,
        features: *mut vk::PhysicalDeviceFeatures2<'_>,
    ) {
        unsafe {
            let next = (*features).p_next.cast::<vk::BaseOutStructure<'_>>();
            match (*next).s_type {
                vk::StructureType::PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_FEATURES => {
                    (*next.cast::<vk::PhysicalDeviceTimelineSemaphoreFeatures<'_>>())
                        .timeline_semaphore = vk::TRUE;
                }
                vk::StructureType::PHYSICAL_DEVICE_SWAPCHAIN_MAINTENANCE_1_FEATURES_EXT => {
                    (*next.cast::<vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT<'_>>())
                        .swapchain_maintenance1 = vk::TRUE;
                }
                _ => unreachable!(),
            }
        };
    }

    #[test]
    fn maintenance_negotiation_respects_explicit_settings_and_requires_a_supported_query() {
        unsafe {
            for enabled in [false, true] {
                let mut maintenance = vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT::default()
                    .swapchain_maintenance1(enabled);
                let mut unrelated = vk::PhysicalDeviceVulkan11Features::default();
                let info = vk::DeviceCreateInfo::default()
                    .push_next(&mut maintenance)
                    .push_next(&mut unrelated);
                assert_eq!(
                    FeatureNegotiation::swapchain_maintenance(
                        &info,
                        vk::PhysicalDevice::null(),
                        Some(supported)
                    ),
                    if enabled {
                        FeatureNegotiation::Existing
                    } else {
                        FeatureNegotiation::Disabled
                    }
                );
            }
            let info = vk::DeviceCreateInfo::default();
            assert_eq!(
                FeatureNegotiation::swapchain_maintenance(&info, vk::PhysicalDevice::null(), None),
                FeatureNegotiation::Disabled
            );
            assert_eq!(
                FeatureNegotiation::swapchain_maintenance(
                    &info,
                    vk::PhysicalDevice::null(),
                    Some(supported)
                ),
                FeatureNegotiation::Inject
            );
        }
    }

    #[test]
    fn timeline_negotiation_preserves_existing_and_disabled_feature_chains() {
        unsafe {
            let mut core = vk::PhysicalDeviceVulkan12Features::default().timeline_semaphore(true);
            let mut unrelated = vk::PhysicalDeviceVulkan11Features::default();
            let info = vk::DeviceCreateInfo::default()
                .push_next(&mut core)
                .push_next(&mut unrelated);
            assert_eq!(
                FeatureNegotiation::timeline(&info, vk::PhysicalDevice::null(), None),
                FeatureNegotiation::Existing
            );
            let mut explicit = vk::PhysicalDeviceTimelineSemaphoreFeatures::default();
            let info = vk::DeviceCreateInfo::default().push_next(&mut explicit);
            assert_eq!(
                FeatureNegotiation::timeline(&info, vk::PhysicalDevice::null(), Some(supported)),
                FeatureNegotiation::Disabled
            );
            let info = vk::DeviceCreateInfo::default();
            assert_eq!(
                FeatureNegotiation::timeline(&info, vk::PhysicalDevice::null(), Some(supported)),
                FeatureNegotiation::Inject
            );
            assert_eq!(
                FeatureNegotiation::timeline(&info, vk::PhysicalDevice::null(), None),
                FeatureNegotiation::Disabled
            );
        }
    }
}
