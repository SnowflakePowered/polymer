#![allow(clippy::too_many_arguments)] // Signatures are fixed by the D3D11 ABI.

use snowglass_producer_d3d11::Tracking;
use std::ffi::c_void;
use windows_sys::Win32::Foundation::{E_FAIL, HMODULE};

snowglass_hook::__chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer]
    pub(crate) struct HookInstallation;
    #[context]
    pub(crate) struct D3D11HookContext {
        pub(crate) tracking: std::sync::Arc<Tracking> = std::sync::Arc::default(),
    }

    #[trampoline(abi = "system-unwind")]
    #[terminal(installed(
        target = crate::install::system_create_device,
    ), fallback = E_FAIL)]
    #[postfix(
        crate::device::create_device(
            adapter,
            device,
            feature_level,
            immediate_context,
            result
        )
    )]
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
    ) -> i32;

    #[trampoline(abi = "system-unwind")]
    #[terminal(installed(
        target = crate::install::system_create_device_and_swapchain,
    ), fallback = E_FAIL)]
    #[postfix(
        crate::device::create_device_and_swapchain(
            adapter,
            swapchain,
            device,
            feature_level,
            immediate_context,
            result
        )
    )]
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
    ) -> i32;
}
