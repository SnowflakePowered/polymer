//! Early-process detours for targets that already loaded the system D3D11 DLL.

use retour::GenericDetour;
use std::{ffi::c_void, sync::OnceLock};
use windows_sys::Win32::Foundation::{E_FAIL, HMODULE};

pub(crate) type D3D11CreateDeviceFn = unsafe extern "system" fn(
    *mut c_void,
    i32,
    HMODULE,
    u32,
    *const i32,
    u32,
    u32,
    *mut *mut c_void,
    *mut i32,
    *mut *mut c_void,
) -> i32;

pub(crate) type D3D11CreateDeviceAndSwapChainFn = unsafe extern "system" fn(
    *mut c_void,
    i32,
    HMODULE,
    u32,
    *const i32,
    u32,
    u32,
    *const c_void,
    *mut *mut c_void,
    *mut *mut c_void,
    *mut i32,
    *mut *mut c_void,
) -> i32;

#[link(name = "d3d11")]
unsafe extern "system" {
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

pub(crate) struct D3D11Detours {
    pub(crate) create_device: GenericDetour<D3D11CreateDeviceFn>,
    pub(crate) create_device_and_swapchain: GenericDetour<D3D11CreateDeviceAndSwapChainFn>,
}

pub(crate) static DETOURS: OnceLock<D3D11Detours> = OnceLock::new();

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
    let create_device = match unsafe {
        GenericDetour::new(
            system_create_device as D3D11CreateDeviceFn,
            crate::sys::create_device,
        )
    } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
    };
    let create_device_and_swapchain = match unsafe {
        GenericDetour::new(
            system_create_device_and_swapchain as D3D11CreateDeviceAndSwapChainFn,
            crate::sys::create_device_and_swapchain,
        )
    } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
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
