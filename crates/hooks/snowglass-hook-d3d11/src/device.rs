//! D3D11 device discovery and hook installation.

use std::{
    ffi::c_void,
    sync::{Mutex, OnceLock},
};
use windows::{
    Win32::Graphics::{
        Direct3D11::ID3D11Device,
        Dxgi::{IDXGIAdapter, IDXGIDevice, IDXGIFactory},
    },
    core::{IUnknown, Interface},
};

static FACTORY_OWNERS: OnceLock<Mutex<Vec<IDXGIFactory>>> = OnceLock::new();

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

fn retain_factory(factory: IDXGIFactory) {
    let mut owners = FACTORY_OWNERS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if !owners
        .iter()
        .any(|owner| owner.as_raw() == factory.as_raw())
    {
        owners.push(factory);
    }
}

pub(crate) unsafe fn create_device(
    adapter: *mut c_void,
    device: *mut *mut c_void,
    _feature_level: *mut i32,
    immediate_context: *mut *mut c_void,
    result: i32,
) {
    if result >= 0 && !snowglass_platform_windows::vulkan_capture_active() {
        crate::d3d11_log("snowglass: intercepted D3D11CreateDevice");
        let installed = unsafe { install_factory_from_adapter(adapter) };
        if !installed {
            unsafe { install_factory_from_device(device) };
        }
        unsafe { crate::vtable::install_immediate_context(immediate_context) };
    }
}

pub(crate) unsafe fn create_device_and_swapchain(
    adapter: *mut c_void,
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
    let installed = unsafe { install_factory_from_adapter(adapter) };
    if !installed {
        unsafe { install_factory_from_device(device) };
    }
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

unsafe fn install_factory_from_device(device: *mut *mut c_void) {
    if device.is_null() {
        return;
    }
    let raw = unsafe { *device };
    if raw.is_null() {
        return;
    }
    let Some(device) = (unsafe { ID3D11Device::from_raw_borrowed(&raw) }) else {
        return;
    };
    let Ok(dxgi_device) = device.cast::<IDXGIDevice>() else {
        return;
    };
    let Ok(adapter) = (unsafe { dxgi_device.GetAdapter() }) else {
        return;
    };
    unsafe { install_factory_from_adapter(adapter.as_raw()) };
}

unsafe fn install_factory_from_adapter(adapter: *mut c_void) -> bool {
    if adapter.is_null() {
        return false;
    }
    let Some(adapter) = (unsafe { IDXGIAdapter::from_raw_borrowed(&adapter) }) else {
        return false;
    };
    let Ok(factory) = (unsafe { adapter.GetParent::<IDXGIFactory>() }) else {
        return false;
    };

    let installed = unsafe { snowglass_hook_dxgi::install_factory(factory.as_raw()) };
    if installed {
        retain_factory(factory);
        crate::d3d11_log("snowglass: installed device DXGI factory swapchain hooks");
    }
    installed
}
