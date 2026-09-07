use super::*;
impl Surface {
    pub(in crate::virtual_swapchain) fn profile(
        physical: vk::PhysicalDevice,
    ) -> Result<Profile, vk::Result> {
        let query: vk::PFN_vkGetPhysicalDeviceImageFormatProperties2 = unsafe {
            Self::proc(physical, c"vkGetPhysicalDeviceImageFormatProperties2")
                .or_else(|| Self::proc(physical, c"vkGetPhysicalDeviceImageFormatProperties2KHR"))
        }
        .ok_or(vk::Result::ERROR_FEATURE_NOT_PRESENT)?;
        let usage = vk::ImageUsageFlags::COLOR_ATTACHMENT
            | vk::ImageUsageFlags::SAMPLED
            | vk::ImageUsageFlags::TRANSFER_SRC
            | vk::ImageUsageFlags::TRANSFER_DST;
        let mut formats = Vec::new();
        let mut max_extent = vk::Extent2D {
            width: u32::MAX,
            height: u32::MAX,
        };
        for format in [
            vk::Format::B8G8R8A8_UNORM,
            vk::Format::R8G8B8A8_UNORM,
            vk::Format::A2B10G10R10_UNORM_PACK32,
            vk::Format::R16G16B16A16_SFLOAT,
        ] {
            let mut external = vk::PhysicalDeviceExternalImageFormatInfo::default()
                .handle_type(vk::ExternalMemoryHandleTypeFlags::D3D12_RESOURCE);
            let info = vk::PhysicalDeviceImageFormatInfo2::default()
                .push_next(&mut external)
                .format(format)
                .ty(vk::ImageType::TYPE_2D)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(usage);
            let mut external_properties = vk::ExternalImageFormatProperties::default();
            let mut properties =
                vk::ImageFormatProperties2::default().push_next(&mut external_properties);
            let result = unsafe { query(physical, &info, &mut properties) };
            let maximum = properties.image_format_properties.max_extent;
            if result == vk::Result::SUCCESS
                && external_properties
                    .external_memory_properties
                    .external_memory_features
                    .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
            {
                max_extent.width = max_extent.width.min(maximum.width);
                max_extent.height = max_extent.height.min(maximum.height);
                formats.push(vk::SurfaceFormatKHR {
                    format,
                    color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
                });
            }
        }
        if formats.is_empty() {
            return Err(vk::Result::ERROR_FORMAT_NOT_SUPPORTED);
        }
        Ok(Profile {
            formats,
            usage,
            max_extent,
        })
    }
    pub(super) fn capabilities(
        &self,
        physical: vk::PhysicalDevice,
        binding: &FixedSurfaceBinding,
    ) -> Result<vk::SurfaceCapabilitiesKHR, vk::Result> {
        let profile = Self::profile(physical)?;
        let extent = vk::Extent2D {
            width: binding.extent().width,
            height: binding.extent().height,
        };
        if extent.width > profile.max_extent.width || extent.height > profile.max_extent.height {
            return Err(vk::Result::ERROR_SURFACE_LOST_KHR);
        }
        Ok(vk::SurfaceCapabilitiesKHR {
            min_image_count: 2,
            max_image_count: 4,
            current_extent: extent,
            min_image_extent: extent,
            max_image_extent: extent,
            max_image_array_layers: 1,
            supported_transforms: vk::SurfaceTransformFlagsKHR::IDENTITY,
            current_transform: vk::SurfaceTransformFlagsKHR::IDENTITY,
            supported_composite_alpha: vk::CompositeAlphaFlagsKHR::OPAQUE,
            supported_usage_flags: profile.usage,
        })
    }
}
