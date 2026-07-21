//! Early-process detours for targets that already loaded the system D3D11 DLL.

use snowglass_platform_windows::InlineDetour;
use std::{ffi::c_void, sync::Mutex};
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

type CreateDeviceInstallation =
    snowglass_hook::TerminalInstallation<dyn crate::hooks::CreateDeviceTerminal>;
type CreateDeviceAndSwapChainInstallation =
    snowglass_hook::TerminalInstallation<dyn crate::hooks::CreateDeviceAndSwapchainTerminal>;

struct D3D11Installation {
    create_device: Option<CreateDeviceInstallation>,
    create_device_and_swapchain: Option<CreateDeviceAndSwapChainInstallation>,
}

static INSTALLATION: Mutex<Option<D3D11Installation>> = Mutex::new(None);

pub(crate) unsafe fn install() -> i32 {
    snowglass_hook_dxgi::register_claim(crate::device::claim_swapchain);
    let mut installation = INSTALLATION
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(installed) = installation.as_mut() {
        if installed.create_device.is_some() && installed.create_device_and_swapchain.is_some() {
            return 0;
        }
        if !uninstall_group(installed) {
            return E_FAIL;
        }
        *installation = None;
    }
    let create_device = InlineDetour::new(
        system_create_device as D3D11CreateDeviceFn,
        crate::hooks::create_device,
    );
    let create_device_and_swapchain = InlineDetour::new(
        system_create_device_and_swapchain as D3D11CreateDeviceAndSwapChainFn,
        crate::hooks::create_device_and_swapchain,
    );
    let Ok(create_device) = crate::hooks::install_create_device_terminal(create_device) else {
        return E_FAIL;
    };
    match crate::hooks::install_create_device_and_swapchain_terminal(create_device_and_swapchain) {
        Ok(create_device_and_swapchain) => {
            *installation = Some(D3D11Installation {
                create_device: Some(create_device),
                create_device_and_swapchain: Some(create_device_and_swapchain),
            });
            0
        }
        Err(_) => {
            if create_device.uninstall().is_err() {
                *installation = Some(D3D11Installation {
                    create_device: Some(create_device),
                    create_device_and_swapchain: None,
                });
            }
            E_FAIL
        }
    }
}

pub(crate) fn uninstall() -> i32 {
    let mut installation = INSTALLATION
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(installed) = installation.as_mut() else {
        return 0;
    };
    let success = uninstall_group(installed);
    if success {
        *installation = None;
        0
    } else {
        E_FAIL
    }
}

fn uninstall_group(installed: &mut D3D11Installation) -> bool {
    let mut success = true;
    if installed
        .create_device_and_swapchain
        .as_ref()
        .is_some_and(|terminal| terminal.uninstall().is_ok())
    {
        installed.create_device_and_swapchain = None;
    } else if installed.create_device_and_swapchain.is_some() {
        success = false;
    }
    if installed
        .create_device
        .as_ref()
        .is_some_and(|terminal| terminal.uninstall().is_ok())
    {
        installed.create_device = None;
    } else if installed.create_device.is_some() {
        success = false;
    }
    success
}
