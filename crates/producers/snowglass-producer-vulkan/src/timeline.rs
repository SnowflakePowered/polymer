use crate::ExternalHandleMode;
use ash::vk;
use snowglass_protocol::AdapterLuid;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

/// Exportable timeline semaphore shared with the viewer.
pub struct ExportTimelineSemaphore {
    device: ash::Device,
    semaphore: vk::Semaphore,
    handle: OwnedHandle,
    _d3d12_fence: Option<windows::Win32::Graphics::Direct3D12::ID3D12Fence>,
}

impl ExportTimelineSemaphore {
    pub fn create_with_function(
        device: ash::Device,
        get_semaphore_win32_handle: vk::PFN_vkGetSemaphoreWin32HandleKHR,
        import_semaphore_win32_handle: vk::PFN_vkImportSemaphoreWin32HandleKHR,
        adapter_luid: AdapterLuid,
        handle_mode: ExternalHandleMode,
    ) -> Result<Self, vk::Result> {
        let handle_type = handle_mode.semaphore_handle_type();
        let mut export = vk::ExportSemaphoreCreateInfo::default().handle_types(handle_type);
        let mut timeline = vk::SemaphoreTypeCreateInfo::default()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0);
        let mut create_info = vk::SemaphoreCreateInfo::default().push_next(&mut timeline);
        if handle_mode == ExternalHandleMode::VulkanOpaque {
            create_info = create_info.push_next(&mut export);
        }
        let semaphore = unsafe { device.create_semaphore(&create_info, None)? };
        if handle_mode == ExternalHandleMode::D3d12 {
            let d3d12 = crate::d3d12_interop::create_device(adapter_luid)?;
            let shared = crate::d3d12_interop::create_fence(&d3d12)?;
            let import = vk::ImportSemaphoreWin32HandleInfoKHR::default()
                .semaphore(semaphore)
                .handle_type(handle_type)
                .handle(shared.handle.as_raw_handle() as _);
            let result = unsafe { import_semaphore_win32_handle(device.handle(), &import) };
            if result != vk::Result::SUCCESS {
                unsafe { device.destroy_semaphore(semaphore, None) };
                return Err(result);
            }
            return Ok(Self {
                device,
                semaphore,
                handle: shared.handle,
                _d3d12_fence: Some(shared.fence),
            });
        }
        let handle_info = vk::SemaphoreGetWin32HandleInfoKHR::default()
            .semaphore(semaphore)
            .handle_type(handle_type);
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
            _d3d12_fence: None,
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
