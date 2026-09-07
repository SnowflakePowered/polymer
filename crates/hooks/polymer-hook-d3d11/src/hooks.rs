#![allow(clippy::too_many_arguments)] // Signatures are fixed by the D3D11 ABI.

use std::ffi::c_void;
use windows_sys::Win32::Foundation::{E_FAIL, HMODULE};

polymer_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(polymer_platform_windows::InlineDetour<_>)]
    #[installer]
    pub(crate) struct HookInstallation;

    #[trampoline(abi = "system-unwind")]
    #[terminal(installed(
        target = crate::install::system_create_device_and_swapchain,
    ), fallback = E_FAIL)]
    #[infix(crate::virtual_creation::VirtualCreation::create)]
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
