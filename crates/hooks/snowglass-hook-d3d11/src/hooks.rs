#![allow(clippy::too_many_arguments)] // Signatures are fixed by the D3D11 ABI.

use crate::hook_chain::{__resolve_terminal, chain, post_chain};
use snowglass_producer_d3d11::Tracking;
use std::ffi::c_void;
use windows_sys::Win32::Foundation::{E_FAIL, HMODULE};

chain! {
    pub(crate) dispatch_create_device, register_create_device;
    after D3D11CreateDevice;
    chain CREATE_DEVICE_CHAIN, context CreateDeviceContext {
        tracking: std::sync::Arc<Tracking> = std::sync::Arc::default(),
    };
    hook CreateDeviceHook(adapter: *mut c_void, device: *mut *mut c_void, feature_level: *mut i32, immediate_context: *mut *mut c_void) -> result: i32;
    abi (adapter: *mut c_void, driver_type: i32, software: HMODULE, flags: u32, feature_levels: *const i32, feature_level_count: u32, sdk_version: u32, device: *mut *mut c_void, feature_level: *mut i32, immediate_context: *mut *mut c_void);
    resolve with detour crate::install::DETOURS, create_device -> E_FAIL;
}

chain! {
    pub(crate) dispatch_create_device_and_swapchain, register_create_device_and_swapchain;
    after D3D11CreateDeviceAndSwapChain;
    chain CREATE_DEVICE_AND_SWAPCHAIN_CHAIN, context CreateDeviceAndSwapChainContext {
        tracking: std::sync::Arc<Tracking> = std::sync::Arc::default(),
    };
    hook CreateDeviceAndSwapChainHook(adapter: *mut c_void, swapchain: *mut *mut c_void, device: *mut *mut c_void, feature_level: *mut i32, immediate_context: *mut *mut c_void) -> result: i32;
    abi (adapter: *mut c_void, driver_type: i32, software: HMODULE, flags: u32, feature_levels: *const i32, feature_level_count: u32, sdk_version: u32, swapchain_desc: *const c_void, swapchain: *mut *mut c_void, device: *mut *mut c_void, feature_level: *mut i32, immediate_context: *mut *mut c_void);
    resolve with detour crate::install::DETOURS, create_device_and_swapchain -> E_FAIL;
}
