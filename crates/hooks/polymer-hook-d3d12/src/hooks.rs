#![allow(clippy::too_many_arguments)]

use std::ffi::c_void;
use windows_sys::{Win32::Foundation::E_FAIL, core::GUID};

polymer_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(polymer_platform_windows::InlineDetour<_>)]

    #[installer]
    pub(crate) struct HookInstallation;



    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_create_device,
    ), fallback = E_FAIL)]
    #[postfix(created_device(device, result))]
    pub(crate) unsafe extern "system" fn create_device(
        adapter: *mut c_void,
        minimum_feature_level: i32,
        interface_id: *const GUID,
        device: *mut *mut c_void,
    ) -> i32;
}

unsafe fn created_device(device: *mut *mut c_void, result: i32) {
    if result >= 0 {
        tracing::debug!("polymer: intercepted D3D12CreateDevice");
        unsafe { crate::vtable::install_device(device, std::sync::Arc::default()) };
    }
}
