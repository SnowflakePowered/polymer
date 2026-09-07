use super::*;
impl Driver {
    pub(in crate::virtual_swapchain::gpu_tests) fn surface(&self, name: &str) -> vk::SurfaceKHR {
        unsafe {
            let title = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                windows::core::w!("STATIC"),
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                0,
                0,
                16,
                16,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let surface = ash::khr::win32_surface::Instance::new(&self._entry, &self.instance)
                .create_win32_surface(
                    &vk::Win32SurfaceCreateInfoKHR::default().hwnd(window.0 as isize),
                    None,
                )
                .unwrap();
            Surface::created(self.instance.handle(), surface, window.0 as isize);
            self.windows.borrow_mut().push((window, surface));
            surface
        }
    }
    pub(in crate::virtual_swapchain::gpu_tests) fn swapchain(
        &self,
        surface: vk::SurfaceKHR,
        old: vk::SwapchainKHR,
    ) -> vk::SwapchainKHR {
        let mut caps = vk::SurfaceCapabilitiesKHR::default();
        unsafe {
            assert_eq!(
                crate::virtual_swapchain::surface::vkGetPhysicalDeviceSurfaceCapabilitiesKHR(
                    self.physical,
                    surface,
                    &mut caps
                ),
                vk::Result::SUCCESS
            );
        }
        assert_eq!(
            (caps.current_extent.width, caps.current_extent.height),
            (16, 16)
        );
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(surface)
            .min_image_count(3)
            .image_format(vk::Format::R8G8B8A8_UNORM)
            .image_color_space(vk::ColorSpaceKHR::SRGB_NONLINEAR)
            .image_extent(caps.current_extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(vk::SurfaceTransformFlagsKHR::IDENTITY)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(
                if std::env::var("POLYMER_VULKAN_WSI_TEST_PROFILE").unwrap() == "exclusive" {
                    vk::PresentModeKHR::FIFO
                } else {
                    vk::PresentModeKHR::IMMEDIATE
                },
            )
            .clipped(true)
            .old_swapchain(old);
        let mut handle = vk::SwapchainKHR::null();
        unsafe {
            assert_eq!(
                wsi::create(self.device(), &info, std::ptr::null(), &mut handle),
                vk::Result::SUCCESS
            );
        }
        handle
    }
    pub(in crate::virtual_swapchain::gpu_tests) fn native_swapchain(
        &self,
        surface: vk::SurfaceKHR,
    ) -> (vk::SwapchainKHR, Vec<vk::Image>) {
        unsafe {
            let extension = ash::khr::surface::Instance::new(&self._entry, &self.instance);
            let caps = extension
                .get_physical_device_surface_capabilities(self.physical, surface)
                .unwrap();
            let format = extension
                .get_physical_device_surface_formats(self.physical, surface)
                .unwrap()[0];
            let info = vk::SwapchainCreateInfoKHR::default()
                .surface(surface)
                .min_image_count(caps.min_image_count.max(2))
                .image_format(format.format)
                .image_color_space(format.color_space)
                .image_extent(caps.current_extent)
                .image_array_layers(1)
                .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(caps.current_transform)
                .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                .present_mode(vk::PresentModeKHR::FIFO)
                .clipped(true);
            let mut handle = vk::SwapchainKHR::null();
            assert_eq!(
                wsi::create(self.device(), &info, std::ptr::null(), &mut handle),
                vk::Result::SUCCESS
            );
            assert!(!SWAPCHAINS.contains_key(&handle));
            let images = ash::khr::swapchain::Device::new(&self.instance, &self.native)
                .get_swapchain_images(handle)
                .unwrap();
            (handle, images)
        }
    }
}
