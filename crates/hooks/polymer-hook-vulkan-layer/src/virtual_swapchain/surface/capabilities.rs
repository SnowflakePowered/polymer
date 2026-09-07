use super::*;

pub(super) struct Capabilities;
impl Capabilities {
    pub(super) unsafe fn query(
        physical: vk::PhysicalDevice,
        info: &vk::PhysicalDeviceSurfaceInfo2KHR<'_>,
        output: &mut vk::SurfaceCapabilities2KHR<'_>,
    ) -> vk::Result {
        let mode = match unsafe { Self::mode(info) } {
            Ok(mode) => mode,
            Err(error) => return error,
        };
        let result = unsafe {
            vkGetPhysicalDeviceSurfaceCapabilitiesKHR(
                physical,
                info.surface,
                &mut output.surface_capabilities,
            )
        };
        if result != vk::Result::SUCCESS {
            return result;
        }
        unsafe { Self::fill(output, mode) }.map_or_else(|error| error, |_| vk::Result::SUCCESS)
    }
    unsafe fn mode(
        info: &vk::PhysicalDeviceSurfaceInfo2KHR<'_>,
    ) -> Result<vk::PresentModeKHR, vk::Result> {
        let mut mode = vk::PresentModeKHR::FIFO;
        let mut current = info.p_next.cast::<vk::BaseInStructure<'_>>();
        while let Some(base) = unsafe { current.as_ref() } {
            if base.s_type != vk::StructureType::SURFACE_PRESENT_MODE_EXT {
                return Err(vk::Result::ERROR_EXTENSION_NOT_PRESENT);
            }
            mode = unsafe { (*current.cast::<vk::SurfacePresentModeEXT<'_>>()).present_mode };
            current = base.p_next;
        }
        if !matches!(
            mode,
            vk::PresentModeKHR::FIFO | vk::PresentModeKHR::IMMEDIATE
        ) {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }
        Ok(mode)
    }
    unsafe fn fill(
        output: &mut vk::SurfaceCapabilities2KHR<'_>,
        mode: vk::PresentModeKHR,
    ) -> Result<(), vk::Result> {
        let extent = output.surface_capabilities.current_extent;
        let mut current = output.p_next.cast::<vk::BaseOutStructure<'_>>();
        while let Some(base) = unsafe { current.as_mut() } {
            match base.s_type {
                vk::StructureType::SURFACE_PROTECTED_CAPABILITIES_KHR => unsafe {
                    (*current.cast::<vk::SurfaceProtectedCapabilitiesKHR<'_>>())
                        .supports_protected = vk::FALSE;
                },
                vk::StructureType::SHARED_PRESENT_SURFACE_CAPABILITIES_KHR => unsafe {
                    (*current.cast::<vk::SharedPresentSurfaceCapabilitiesKHR<'_>>())
                        .shared_present_supported_usage_flags = vk::ImageUsageFlags::empty();
                },
                vk::StructureType::SURFACE_CAPABILITIES_FULL_SCREEN_EXCLUSIVE_EXT => unsafe {
                    (*current.cast::<vk::SurfaceCapabilitiesFullScreenExclusiveEXT<'_>>())
                        .full_screen_exclusive_supported = vk::FALSE;
                },
                vk::StructureType::SURFACE_PRESENT_SCALING_CAPABILITIES_EXT => {
                    let scaling = unsafe {
                        &mut *current.cast::<vk::SurfacePresentScalingCapabilitiesEXT<'_>>()
                    };
                    scaling.supported_present_scaling = vk::PresentScalingFlagsEXT::empty();
                    scaling.supported_present_gravity_x = vk::PresentGravityFlagsEXT::empty();
                    scaling.supported_present_gravity_y = vk::PresentGravityFlagsEXT::empty();
                    scaling.min_scaled_image_extent = extent;
                    scaling.max_scaled_image_extent = extent;
                }
                vk::StructureType::SURFACE_PRESENT_MODE_COMPATIBILITY_EXT => {
                    let modes = unsafe {
                        &mut *current.cast::<vk::SurfacePresentModeCompatibilityEXT<'_>>()
                    };
                    unsafe {
                        let _ = WsiResult::enumerate(
                            &[mode],
                            &mut modes.present_mode_count,
                            modes.p_present_modes,
                        );
                    }
                }
                _ => return Err(vk::Result::ERROR_EXTENSION_NOT_PRESENT),
            }
            current = base.p_next;
        }
        Ok(())
    }
}
