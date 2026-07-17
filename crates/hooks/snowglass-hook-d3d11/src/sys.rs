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
    pub(crate) unsafe extern "system" fn create_device(
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
    pub(crate) unsafe extern "system" fn create_device_and_swapchain(
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

pub(crate) unsafe extern "system" fn draw_indexed(
    this: *mut std::ffi::c_void,
    index_count: u32,
    start_index: u32,
    base_vertex: i32,
) {
    if let Some(original) = crate::tracking::draw_indexed_original(this) {
        unsafe { original(this, index_count, start_index, base_vertex) };
    }
    crate::tracking::record_draw(this);
}

pub(crate) unsafe extern "system" fn draw(
    this: *mut std::ffi::c_void,
    vertex_count: u32,
    start_vertex: u32,
) {
    if let Some(original) = crate::tracking::draw_original(this) {
        unsafe { original(this, vertex_count, start_vertex) };
    }
    crate::tracking::record_draw(this);
}
