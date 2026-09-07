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
    if let Err(error) = polymer_hook::config::require_swapchain() {
        tracing::error!(%error, "D3D11 presentation configuration rejected");
        return windows_sys::Win32::Foundation::E_INVALIDARG;
    }
    polymer_hook_dxgi::register_virtual_claim(
        polymer_producer_d3d11::swapchain::creation::D3d11Creation::claim,
    );
    if let Err(error) = crate::ddi::Rotation::install() {
        tracing::error!(%error, "D3D11 driver rotation installation failed");
        return error.code().0;
    }
    crate::hooks::HookInstallation::install().map_or(E_FAIL, |()| 0)
}

pub(crate) fn uninstall() -> i32 {
    crate::hooks::HookInstallation::uninstall().map_or(E_FAIL, |()| 0)
}
