//! Imported D3D12 fence timelines used to coordinate the image ring.

use crate::interop::{
    Gluint,
    memory_object_win32::{
        GL_D3D12_FENCE_VALUE_EXT, GL_HANDLE_TYPE_D3D12_FENCE_EXT, GL_LAYOUT_GENERAL_EXT,
        GlFunctions,
    },
};
use windows::Win32::{
    Foundation::{CloseHandle, GENERIC_ALL, HANDLE},
    Graphics::Direct3D12::{D3D12_FENCE_FLAG_SHARED, ID3D12Device, ID3D12Fence},
};

pub(crate) struct Timeline {
    fence: ID3D12Fence,
    handle: HANDLE,
    semaphore: Gluint,
    functions: GlFunctions,
}

unsafe impl Send for Timeline {}

impl Timeline {
    pub(crate) unsafe fn new(
        device: &ID3D12Device,
        functions: &GlFunctions,
    ) -> Result<Self, String> {
        let fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_SHARED) }
            .map_err(|error| format!("ID3D12Device::CreateFence: {error}"))?;
        let handle = unsafe { device.CreateSharedHandle(&fence, None, GENERIC_ALL.0, None) }
            .map_err(|error| format!("CreateSharedHandle(fence): {error}"))?;
        let mut semaphore = 0;
        unsafe {
            (functions.gen_semaphores)(1, &mut semaphore);
            (functions.import_semaphore)(semaphore, GL_HANDLE_TYPE_D3D12_FENCE_EXT, handle.0);
        }
        Ok(Self {
            fence,
            handle,
            semaphore,
            functions: *functions,
        })
    }

    pub(crate) fn handle(&self) -> HANDLE {
        self.handle
    }

    pub(crate) unsafe fn completed_value(&self) -> u64 {
        unsafe { self.fence.GetCompletedValue() }
    }

    pub(crate) unsafe fn signal(&self, functions: &GlFunctions, texture: Gluint, value: u64) {
        let layout = GL_LAYOUT_GENERAL_EXT;
        unsafe {
            (functions.semaphore_parameter)(self.semaphore, GL_D3D12_FENCE_VALUE_EXT, &value);
            (functions.signal_semaphore)(self.semaphore, 0, std::ptr::null(), 1, &texture, &layout);
        }
    }
}

impl Drop for Timeline {
    fn drop(&mut self) {
        unsafe {
            (self.functions.delete_semaphores)(1, &self.semaphore);
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::interop::{
        memory_object_win32::{ExternalImage, GL_DEVICE_LUID_EXT},
        proc,
    };
    use polymer_protocol::AdapterLuid;
    use windows::Win32::Foundation::GetHandleInformation;

    /// Run under the native WGL fixture's current context.
    pub(crate) unsafe fn abandoned_setup_releases_imported_timeline() {
        let functions = unsafe { GlFunctions::load() }.expect("EXT interop unavailable");
        let mut luid = [0u8; 8];
        unsafe { (functions.get_unsigned_byte)(GL_DEVICE_LUID_EXT, luid.as_mut_ptr()) };
        let (device, _) = unsafe {
            ExternalImage::create_device(AdapterLuid {
                low_part: u32::from_le_bytes(luid[..4].try_into().unwrap()),
                high_part: i32::from_le_bytes(luid[4..].try_into().unwrap()),
            })
        }
        .unwrap();
        let is_semaphore: unsafe extern "system" fn(u32) -> u8 =
            unsafe { proc(b"glIsSemaphoreEXT\0") }.unwrap();
        let timeline = unsafe { Timeline::new(&device, &functions) }.unwrap();
        let (handle, semaphore) = (timeline.handle, timeline.semaphore);
        let mut flags = 0;
        assert!(unsafe { GetHandleInformation(handle, &mut flags) }.is_ok());
        assert_ne!(unsafe { is_semaphore(semaphore) }, 0);
        // This is the ownership state when a later setup stage returns an
        // error: no ExtCapture exists to perform manual teardown.
        drop(timeline);
        assert_eq!(unsafe { is_semaphore(semaphore) }, 0);
        assert!(unsafe { GetHandleInformation(handle, &mut flags) }.is_err());
    }
}
