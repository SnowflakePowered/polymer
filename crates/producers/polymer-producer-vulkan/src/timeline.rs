use crate::ExternalDevice;
use ash::vk;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use windows::Win32::Graphics::Direct3D12::ID3D12Fence;

/// Timeline semaphore importing the shared D3D12 fence used by both consumers.
pub(crate) struct ExportTimelineSemaphore {
    device: ash::Device,
    semaphore: vk::Semaphore,
    handle: OwnedHandle,
    _fence: ID3D12Fence,
}

impl ExportTimelineSemaphore {
    pub(crate) fn create(external: &ExternalDevice) -> Result<Self, vk::Result> {
        let d3d12 = crate::d3d12_interop::create_device(external.adapter_luid)?;
        let shared = crate::d3d12_interop::create_fence(&d3d12)?;
        let device = &external.device;
        let mut timeline = vk::SemaphoreTypeCreateInfo::default()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0);
        let info = vk::SemaphoreCreateInfo::default().push_next(&mut timeline);
        let semaphore = unsafe { device.create_semaphore(&info, None)? };
        let import = vk::ImportSemaphoreWin32HandleInfoKHR::default()
            .semaphore(semaphore)
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::D3D12_FENCE)
            .handle(shared.handle.as_raw_handle() as _);
        let result = unsafe { (external.import_semaphore_win32_handle)(device.handle(), &import) };
        if result != vk::Result::SUCCESS {
            unsafe { device.destroy_semaphore(semaphore, None) };
            return Err(result);
        }
        Ok(Self {
            device: device.clone(),
            semaphore,
            handle: shared.handle,
            _fence: shared.fence,
        })
    }
    pub(crate) fn semaphore(&self) -> vk::Semaphore {
        self.semaphore
    }
    pub(crate) fn raw_handle(&self) -> isize {
        self.handle.as_raw_handle() as isize
    }
}

impl Drop for ExportTimelineSemaphore {
    fn drop(&mut self) {
        unsafe { self.device.destroy_semaphore(self.semaphore, None) };
    }
}
