//! Injector-facing D3D11 hook installation entry point.

use std::ffi::c_void;
use windows_sys::Win32::Foundation::HMODULE;

/// Installs the process-wide D3D11 export detours.
///
/// # Safety
///
/// The process must have a usable system D3D11 runtime, and callers must not
/// concurrently unload the D3D11 module while installation is in progress.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallD3d11Hook() -> i32 {
    unsafe { crate::install::install() }
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system-unwind" fn create_device(
        adapter: *mut c_void,
        driver_type: i32,
        software: HMODULE,
        flags: u32,
        feature_levels: *const i32,
        feature_level_count: u32,
        sdk_version: u32,
        device: *mut *mut c_void,
        feature_level: *mut i32,
        immediate_context: *mut *mut c_void,
    ) -> i32 => crate::hooks::dispatch_create_device;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system-unwind" fn create_device_and_swapchain(
        adapter: *mut c_void,
        driver_type: i32,
        software: HMODULE,
        flags: u32,
        feature_levels: *const i32,
        feature_level_count: u32,
        sdk_version: u32,
        swapchain_desc: *const c_void,
        swapchain: *mut *mut c_void,
        device: *mut *mut c_void,
        feature_level: *mut i32,
        immediate_context: *mut *mut c_void,
    ) -> i32 => crate::hooks::dispatch_create_device_and_swapchain;
}
