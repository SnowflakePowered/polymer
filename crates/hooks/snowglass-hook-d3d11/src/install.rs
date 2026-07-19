//! Early-process detours for targets that already loaded the system D3D11 DLL.

use retour::RawDetour;
use std::{ffi::c_void, sync::OnceLock};
use windows_sys::Win32::Foundation::{E_FAIL, HMODULE};

pub(crate) type D3D11CreateDeviceFn = unsafe extern "system-unwind" fn(
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

pub(crate) type D3D11CreateDeviceAndSwapChainFn = unsafe extern "system-unwind" fn(
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
unsafe extern "system-unwind" {
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
    pub(crate) create_device: CreateDeviceDetour,
    pub(crate) create_device_and_swapchain: CreateDeviceAndSwapChainDetour,
}

pub(crate) struct CreateDeviceDetour(RawDetour);

impl CreateDeviceDetour {
    unsafe fn new(
        target: D3D11CreateDeviceFn,
        detour: D3D11CreateDeviceFn,
    ) -> retour::Result<Self> {
        unsafe { RawDetour::new(target as *const (), detour as *const ()).map(Self) }
    }

    pub(crate) unsafe fn call(
        &self,
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
        let original: D3D11CreateDeviceFn = unsafe { std::mem::transmute(self.0.trampoline()) };
        unsafe {
            original(
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
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    unsafe fn enable(&self) -> retour::Result<()> {
        unsafe { self.0.enable() }
    }

    unsafe fn disable(&self) -> retour::Result<()> {
        unsafe { self.0.disable() }
    }
}

pub(crate) struct CreateDeviceAndSwapChainDetour(RawDetour);

impl CreateDeviceAndSwapChainDetour {
    unsafe fn new(
        target: D3D11CreateDeviceAndSwapChainFn,
        detour: D3D11CreateDeviceAndSwapChainFn,
    ) -> retour::Result<Self> {
        unsafe { RawDetour::new(target as *const (), detour as *const ()).map(Self) }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) unsafe fn call(
        &self,
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
        let original: D3D11CreateDeviceAndSwapChainFn =
            unsafe { std::mem::transmute(self.0.trampoline()) };
        unsafe {
            original(
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
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    unsafe fn enable(&self) -> retour::Result<()> {
        unsafe { self.0.enable() }
    }

    unsafe fn disable(&self) -> retour::Result<()> {
        unsafe { self.0.disable() }
    }
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
        CreateDeviceDetour::new(
            system_create_device as D3D11CreateDeviceFn,
            crate::sys::create_device,
        )
    } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
    };
    let create_device_and_swapchain = match unsafe {
        CreateDeviceAndSwapChainDetour::new(
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
