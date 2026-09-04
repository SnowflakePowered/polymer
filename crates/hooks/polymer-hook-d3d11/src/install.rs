//! Early-process detours for targets that already loaded the system D3D11 DLL.

use std::ffi::c_void;
use windows_sys::Win32::Foundation::{E_FAIL, HMODULE};

#[link(name = "d3d11")]
unsafe extern "system-unwind" {
    #[link_name = "D3D11CreateDevice"]
    pub(crate) fn system_create_device(
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
    ) -> i32;

    #[link_name = "D3D11CreateDeviceAndSwapChain"]
    pub(crate) fn system_create_device_and_swapchain(
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
    ) -> i32;
}

pub(crate) unsafe fn install() -> i32 {
    polymer_hook_dxgi::register_claim(crate::device::claim_swapchain);
    crate::hooks::HookInstallation::install().map_or(E_FAIL, |()| 0)
}

pub(crate) fn uninstall() -> i32 {
    crate::hooks::HookInstallation::uninstall().map_or(E_FAIL, |()| 0)
}
