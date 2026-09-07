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

    /// Reacquire external image access after the consumer releases this slot.
    pub(crate) unsafe fn wait(&self, texture: Gluint, value: u64) {
        if value == 0 {
            return;
        }
        unsafe {
            (self.functions.semaphore_parameter)(self.semaphore, GL_D3D12_FENCE_VALUE_EXT, &value);
            (self.functions.wait_semaphore)(
                self.semaphore,
                0,
                std::ptr::null(),
                1,
                &texture,
                &GL_LAYOUT_GENERAL_EXT,
            );
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
