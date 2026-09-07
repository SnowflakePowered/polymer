//! Native device dispatch and Win32 external-memory support.
use ash::vk;
use polymer_protocol::AdapterLuid;
use std::ffi::c_void;

pub type SetDeviceLoaderData = unsafe extern "system" fn(vk::Device, *mut c_void) -> vk::Result;

pub struct ExternalDevice {
    pub device: ash::Device,
    pub memory_properties: vk::PhysicalDeviceMemoryProperties,
    pub get_memory_win32_handle_properties: vk::PFN_vkGetMemoryWin32HandlePropertiesKHR,
    pub import_semaphore_win32_handle: vk::PFN_vkImportSemaphoreWin32HandleKHR,
    pub set_loader_data: Option<SetDeviceLoaderData>,
    pub supports_foreign_queue: bool,
    pub adapter_luid: AdapterLuid,
}
