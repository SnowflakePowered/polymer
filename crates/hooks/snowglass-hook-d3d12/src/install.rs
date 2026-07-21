use snowglass_platform_windows::InlineDetour;
use std::{ffi::c_void, sync::Mutex};
use windows_sys::{Win32::Foundation::E_FAIL, core::GUID};

pub(crate) type D3D12CreateDeviceFn =
    unsafe extern "system" fn(*mut c_void, i32, *const GUID, *mut *mut c_void) -> i32;

#[link(name = "d3d12")]
unsafe extern "system" {
    #[link_name = "D3D12CreateDevice"]
    fn system_create_device(
        adapter: *mut c_void,
        minimum_feature_level: i32,
        interface_id: *const GUID,
        device: *mut *mut c_void,
    ) -> i32;
}

type Installation = snowglass_hook::TerminalInstallation<dyn crate::hooks::CreateDeviceTerminal>;

static INSTALLATION: Mutex<Option<Installation>> = Mutex::new(None);

pub(crate) unsafe fn install() -> i32 {
    snowglass_hook_dxgi::register_claim(crate::claim_swapchain);
    let mut installation = INSTALLATION
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if installation.is_some() {
        return 0;
    }
    let detour = InlineDetour::new(
        system_create_device as D3D12CreateDeviceFn,
        crate::hooks::create_device,
    );
    match crate::hooks::install_create_device_terminal(detour) {
        Ok(installed) => {
            *installation = Some(installed);
            0
        }
        Err(_) => E_FAIL,
    }
}

pub(crate) fn uninstall() -> i32 {
    let mut installation = INSTALLATION
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(installed) = installation.as_ref() else {
        return 0;
    };
    match installed.uninstall() {
        Ok(()) => {
            *installation = None;
            0
        }
        Err(_) => E_FAIL,
    }
}
