#![allow(clippy::too_many_arguments)]

use crate::hook_chain::{__resolve_terminal, chain, post_chain};
use std::ffi::c_void;
use windows_sys::{Win32::Foundation::E_FAIL, core::GUID};

chain! {
    pub(crate) dispatch_create_device, register_create_device;
    after D3D12CreateDevice;
    chain CREATE_DEVICE_CHAIN, context CreateDeviceContext;
    hook CreateDeviceHook(device: *mut *mut c_void) -> result: i32;
    abi (adapter: *mut c_void, minimum_feature_level: i32, interface_id: *const GUID, device: *mut *mut c_void);
    resolve with detour crate::install::CREATE_DEVICE, fallback E_FAIL;
}
