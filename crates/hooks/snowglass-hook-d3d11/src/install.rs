//! Early-process detours for targets that already loaded the system D3D11 DLL.

use crate::loader::{D3D11CreateDeviceAndSwapChainFn, D3D11CreateDeviceFn};
use retour::GenericDetour;
use std::{ffi::c_void, sync::OnceLock};
use windows_sys::Win32::Foundation::HMODULE;

const E_FAIL: i32 = 0x8000_4005u32 as i32;

struct D3D11Detours {
    create_device: GenericDetour<D3D11CreateDeviceFn>,
    create_device_and_swapchain: GenericDetour<D3D11CreateDeviceAndSwapChainFn>,
}

static DETOURS: OnceLock<D3D11Detours> = OnceLock::new();

pub(crate) unsafe fn install() -> i32 {
    if let Some(detours) = DETOURS.get() {
        return if detours.create_device.is_enabled()
            && detours.create_device_and_swapchain.is_enabled()
        {
            0
        } else {
            E_FAIL
        };
    }
    let create_device = match crate::loader::system_create_device() {
        Ok(target) => {
            match unsafe { GenericDetour::new(target, crate::hooks::D3D11CreateDevice) } {
                Ok(detour) => detour,
                Err(_) => return E_FAIL,
            }
        }
        Err(error) => return error,
    };
    let create_device_and_swapchain = match crate::loader::system_create_device_and_swapchain() {
        Ok(target) => {
            match unsafe { GenericDetour::new(target, crate::hooks::D3D11CreateDeviceAndSwapChain) }
            {
                Ok(detour) => detour,
                Err(_) => return E_FAIL,
            }
        }
        Err(error) => return error,
    };
    if DETOURS
        .set(D3D11Detours {
            create_device,
            create_device_and_swapchain,
        })
        .is_err()
    {
        return E_FAIL;
    }
    let detours = DETOURS.get().expect("detours were just initialized");
    let enabled = unsafe {
        detours
            .create_device
            .enable()
            .and_then(|()| detours.create_device_and_swapchain.enable())
    };
    if enabled.is_err() {
        let _ = unsafe { detours.create_device_and_swapchain.disable() };
        let _ = unsafe { detours.create_device.disable() };
        return E_FAIL;
    }
    0
}

#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn call_original_create_device(
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
) -> i32 {
    if let Some(detours) = DETOURS.get()
        && detours.create_device.is_enabled()
    {
        return unsafe {
            detours.create_device.call(
                adapter,
                driver_type,
                software,
                flags,
                feature_levels,
                feature_level_count,
                sdk_version,
                device,
                feature_level,
                immediate_context,
            )
        };
    }
    match crate::loader::system_create_device() {
        Ok(function) => unsafe {
            function(
                adapter,
                driver_type,
                software,
                flags,
                feature_levels,
                feature_level_count,
                sdk_version,
                device,
                feature_level,
                immediate_context,
            )
        },
        Err(error) => error,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn call_original_create_device_and_swapchain(
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
) -> i32 {
    if let Some(detours) = DETOURS.get()
        && detours.create_device_and_swapchain.is_enabled()
    {
        return unsafe {
            detours.create_device_and_swapchain.call(
                adapter,
                driver_type,
                software,
                flags,
                feature_levels,
                feature_level_count,
                sdk_version,
                swapchain_desc,
                swapchain,
                device,
                feature_level,
                immediate_context,
            )
        };
    }
    match crate::loader::system_create_device_and_swapchain() {
        Ok(function) => unsafe {
            function(
                adapter,
                driver_type,
                software,
                flags,
                feature_levels,
                feature_level_count,
                sdk_version,
                swapchain_desc,
                swapchain,
                device,
                feature_level,
                immediate_context,
            )
        },
        Err(error) => error,
    }
}
