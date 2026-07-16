//! In-process DirectInput interception installed with a trampoline detour.
#![allow(unsafe_code)]

use dinput8::{DirectInput8CreateFn, intercept_direct_input8_create, system_direct_input8_create};
use retour::GenericDetour;
use std::{ffi::c_void, sync::OnceLock};
use windows_sys::{
    Win32::Foundation::HINSTANCE,
    core::{GUID, HRESULT},
};

static DIRECT_INPUT_8_CREATE: OnceLock<GenericDetour<DirectInput8CreateFn>> = OnceLock::new();

unsafe extern "system" fn call_original(
    instance: HINSTANCE,
    version: u32,
    interface_id: *const GUID,
    output: *mut *mut c_void,
    outer: *mut c_void,
) -> HRESULT {
    match DIRECT_INPUT_8_CREATE.get() {
        Some(detour) => unsafe { detour.call(instance, version, interface_id, output, outer) },
        None => 0x8000_4005u32 as i32,
    }
}

unsafe extern "system" fn direct_input_8_create_detour(
    instance: HINSTANCE,
    version: u32,
    interface_id: *const GUID,
    output: *mut *mut c_void,
    outer: *mut c_void,
) -> HRESULT {
    unsafe {
        intercept_direct_input8_create(
            call_original,
            instance,
            version,
            interface_id,
            output,
            outer,
        )
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallDinputHook() -> HRESULT {
    if let Some(detour) = DIRECT_INPUT_8_CREATE.get() {
        return if detour.is_enabled() {
            0
        } else {
            0x8000_4005u32 as i32
        };
    }
    let target = match system_direct_input8_create() {
        Ok(target) => target,
        Err(error) => return error,
    };
    let detour = match unsafe { GenericDetour::new(target, direct_input_8_create_detour) } {
        Ok(detour) => detour,
        Err(_) => return 0x8000_4005u32 as i32,
    };
    if DIRECT_INPUT_8_CREATE.set(detour).is_err() {
        return 0x8000_4005u32 as i32;
    }
    match unsafe {
        DIRECT_INPUT_8_CREATE
            .get()
            .expect("detour was set")
            .enable()
    } {
        Ok(()) => 0,
        Err(_) => 0x8000_4005u32 as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_is_idempotent_and_intercepts_the_system_export() {
        const IID_IDIRECT_INPUT_8_W: GUID = GUID::from_u128(0xbf798031_483a_4da2_aa99_5d64ed369700);
        assert_eq!(unsafe { SnowglassInstallDinputHook() }, 0);
        assert_eq!(unsafe { SnowglassInstallDinputHook() }, 0);

        let target = system_direct_input8_create().unwrap();
        let instance = unsafe {
            windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null())
        };
        let mut interface = std::ptr::null_mut();
        let result = unsafe {
            target(
                instance,
                0x0800,
                &IID_IDIRECT_INPUT_8_W,
                &mut interface,
                std::ptr::null_mut(),
            )
        };
        assert!(
            result >= 0,
            "detoured DirectInput8Create failed: {result:#x}"
        );
        assert!(!interface.is_null());
        let vtable = unsafe { *interface.cast::<*const usize>() };
        let release = unsafe {
            std::mem::transmute::<usize, unsafe extern "system" fn(*mut c_void) -> u32>(
                *vtable.add(2),
            )
        };
        assert_eq!(unsafe { release(interface) }, 0);
    }
}
