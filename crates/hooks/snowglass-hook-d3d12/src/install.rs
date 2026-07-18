use retour::GenericDetour;
use std::{ffi::c_void, sync::OnceLock};
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

pub(crate) static CREATE_DEVICE: OnceLock<GenericDetour<D3D12CreateDeviceFn>> = OnceLock::new();

pub(crate) unsafe fn install() -> i32 {
    crate::register_hooks();
    crate::install_dxgi_interception();
    if let Some(detour) = CREATE_DEVICE.get() {
        return if detour.is_enabled() { 0 } else { E_FAIL };
    }
    let Ok(detour) = (unsafe {
        GenericDetour::new(
            system_create_device as D3D12CreateDeviceFn,
            crate::sys::create_device,
        )
    }) else {
        return E_FAIL;
    };
    if CREATE_DEVICE.set(detour).is_err() {
        return E_FAIL;
    }
    match unsafe { CREATE_DEVICE.get().expect("detour was set").enable() } {
        Ok(()) => 0,
        Err(_) => E_FAIL,
    }
}
