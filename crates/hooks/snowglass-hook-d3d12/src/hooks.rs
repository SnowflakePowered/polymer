#![allow(clippy::too_many_arguments)]

use snowglass_producer_d3d12::Tracking;
use std::ffi::c_void;
use windows_sys::{Win32::Foundation::E_FAIL, core::GUID};

snowglass_hook::__chain_hooks! {
    #![order(reverse)]
    #[context]
    pub(crate) struct D3D12HookContext {
        pub(crate) tracking: std::sync::Arc<Tracking> = std::sync::Arc::default(),
    }

    #[trampoline]
    #[terminal(installed, fallback = E_FAIL)]
    #[postfix(created_device(device, result))]
    pub(crate) unsafe extern "system" fn create_device(
        adapter: *mut c_void,
        minimum_feature_level: i32,
        interface_id: *const GUID,
        device: *mut *mut c_void,
    ) -> i32;
}

unsafe fn created_device(device: *mut *mut c_void, result: i32, context: &CreateDeviceContext<'_>) {
    if result >= 0 {
        crate::d3d12_log("snowglass: intercepted D3D12CreateDevice");
        unsafe { crate::vtable::install_device(device, context.common.tracking.clone()) };
    }
}
