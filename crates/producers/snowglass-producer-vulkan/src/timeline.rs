use ash::vk;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

/// Exportable timeline semaphore shared with the viewer.
pub struct ExportTimelineSemaphore {
    device: ash::Device,
    semaphore: vk::Semaphore,
    handle: OwnedHandle,
}

impl ExportTimelineSemaphore {
    pub fn create_with_function(
        device: ash::Device,
        get_semaphore_win32_handle: vk::PFN_vkGetSemaphoreWin32HandleKHR,
    ) -> Result<Self, vk::Result> {
        let mut export = vk::ExportSemaphoreCreateInfo::default()
            .handle_types(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_WIN32);
        let mut timeline = vk::SemaphoreTypeCreateInfo::default()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0);
        let create_info = vk::SemaphoreCreateInfo::default()
            .push_next(&mut export)
            .push_next(&mut timeline);
        let semaphore = unsafe { device.create_semaphore(&create_info, None)? };
        let handle_info = vk::SemaphoreGetWin32HandleInfoKHR::default()
            .semaphore(semaphore)
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_WIN32);
        let mut raw_handle = 0;
        let result =
            unsafe { get_semaphore_win32_handle(device.handle(), &handle_info, &mut raw_handle) };
        if result != vk::Result::SUCCESS || raw_handle == 0 {
            unsafe { device.destroy_semaphore(semaphore, None) };
            return Err(if result == vk::Result::SUCCESS {
                vk::Result::ERROR_INVALID_EXTERNAL_HANDLE
            } else {
                result
            });
        }
        // SAFETY: Vulkan returned a new owned Win32 handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw_handle as _) };
        Ok(Self {
            device,
            semaphore,
            handle,
        })
    }

    pub fn semaphore(&self) -> vk::Semaphore {
        self.semaphore
    }

    pub fn raw_handle(&self) -> isize {
        self.handle.as_raw_handle() as isize
    }
}

impl Drop for ExportTimelineSemaphore {
    fn drop(&mut self) {
        unsafe { self.device.destroy_semaphore(self.semaphore, None) };
    }
}
