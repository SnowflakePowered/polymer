//! D3D11 device discovery and hook installation.

use std::ffi::c_void;
use windows::{
    Win32::Graphics::Direct3D11::ID3D11Device,
    core::{IUnknown, Interface},
};

pub(crate) unsafe fn claim_swapchain(
    _swapchain: *mut c_void,
    creation_device: *mut c_void,
) -> bool {
    if snowglass_platform_windows::vulkan_capture_active() || creation_device.is_null() {
        return false;
    }
    unsafe { IUnknown::from_raw_borrowed(&creation_device) }
        .is_some_and(|unknown| unknown.cast::<ID3D11Device>().is_ok())
}

pub(crate) unsafe fn create_device(
    _adapter: *mut c_void,
    _device: *mut *mut c_void,
    _feature_level: *mut i32,
    immediate_context: *mut *mut c_void,
    result: i32,
) {
    if result >= 0 && !snowglass_platform_windows::vulkan_capture_active() {
        crate::d3d11_log("snowglass: intercepted D3D11CreateDevice");
        unsafe { crate::vtable::install_immediate_context(immediate_context) };
    }
}

pub(crate) unsafe fn create_device_and_swapchain(
    _adapter: *mut c_void,
    swapchain: *mut *mut c_void,
    device: *mut *mut c_void,
    _feature_level: *mut i32,
    immediate_context: *mut *mut c_void,
    result: i32,
) {
    if result < 0 || swapchain.is_null() || snowglass_platform_windows::vulkan_capture_active() {
        return;
    }
    crate::d3d11_log("snowglass: intercepted D3D11CreateDeviceAndSwapChain");
    unsafe { crate::vtable::install_immediate_context(immediate_context) };
    let swapchain = unsafe { *swapchain };
    if !swapchain.is_null() {
        let creation_device = if device.is_null() {
            std::ptr::null_mut()
        } else {
            unsafe { *device }
        };
        unsafe { snowglass_hook_dxgi::register_swapchain(swapchain, creation_device) };
    }
}
