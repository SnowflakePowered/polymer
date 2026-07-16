//! ABI trampolines used by the injector and `retour`.

use std::ffi::c_void;
use windows_sys::{
    Win32::Foundation::HINSTANCE,
    core::{GUID, HRESULT},
};

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallDinputHook() -> HRESULT {
    unsafe { crate::install::install() }
}

pub(crate) unsafe extern "system" fn direct_input_8_create(
    instance: HINSTANCE,
    version: u32,
    interface_id: *const GUID,
    output: *mut *mut c_void,
    outer: *mut c_void,
) -> HRESULT {
    unsafe {
        crate::interception::direct_input_8_create(instance, version, interface_id, output, outer)
    }
}
