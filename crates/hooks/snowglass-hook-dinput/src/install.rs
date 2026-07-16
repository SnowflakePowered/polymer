//! Detour ownership and lifecycle.

use dinput8::{DirectInput8CreateFn, system_direct_input8_create};
use retour::GenericDetour;
use std::{ffi::c_void, sync::OnceLock};
use windows_sys::{
    Win32::Foundation::HINSTANCE,
    core::{GUID, HRESULT},
};

const E_FAIL: HRESULT = 0x8000_4005u32 as i32;
static DIRECT_INPUT_8_CREATE: OnceLock<GenericDetour<DirectInput8CreateFn>> = OnceLock::new();

pub(crate) unsafe fn install() -> HRESULT {
    if let Some(detour) = DIRECT_INPUT_8_CREATE.get() {
        return if detour.is_enabled() { 0 } else { E_FAIL };
    }
    let target = match system_direct_input8_create() {
        Ok(target) => target,
        Err(error) => return error,
    };
    let detour = match unsafe { GenericDetour::new(target, crate::sys::direct_input_8_create) } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
    };
    if DIRECT_INPUT_8_CREATE.set(detour).is_err() {
        return E_FAIL;
    }
    match unsafe {
        DIRECT_INPUT_8_CREATE
            .get()
            .expect("detour was set")
            .enable()
    } {
        Ok(()) => 0,
        Err(_) => E_FAIL,
    }
}

pub(crate) unsafe extern "system" fn call_original(
    instance: HINSTANCE,
    version: u32,
    interface_id: *const GUID,
    output: *mut *mut c_void,
    outer: *mut c_void,
) -> HRESULT {
    match DIRECT_INPUT_8_CREATE.get() {
        Some(detour) => unsafe { detour.call(instance, version, interface_id, output, outer) },
        None => E_FAIL,
    }
}
