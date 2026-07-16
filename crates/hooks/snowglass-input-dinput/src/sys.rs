//! Lightweight `dinput8.dll` ABI trampolines.

use std::ffi::c_void;
use windows_sys::Win32::Devices::HumanInterfaceDevice::DIDEVICEOBJECTDATA;
use windows_sys::{
    Win32::Foundation::HINSTANCE,
    core::{GUID, HRESULT},
};

macro_rules! com_trampoline {
    ($name:ident($($arg:ident: $ty:ty),* $(,)?) -> $result:ty) => {
        pub(crate) unsafe extern "system" fn $name($($arg: $ty),*) -> $result {
            unsafe { crate::runtime::$name($($arg),*) }
        }
    };
}

com_trampoline!(direct_input_release(this: *mut c_void) -> u32);
com_trampoline!(create_device(this: *mut c_void, class: *const GUID, output: *mut *mut c_void, outer: *mut c_void) -> HRESULT);
com_trampoline!(device_release(this: *mut c_void) -> u32);
com_trampoline!(acquire(this: *mut c_void) -> HRESULT);
com_trampoline!(get_device_state(this: *mut c_void, size: u32, output: *mut c_void) -> HRESULT);
com_trampoline!(get_device_data(this: *mut c_void, object_size: u32, output: *mut DIDEVICEOBJECTDATA, count: *mut u32, flags: u32) -> HRESULT);
com_trampoline!(set_data_format(this: *mut c_void, format: *const c_void) -> HRESULT);
com_trampoline!(set_event_notification(this: *mut c_void, event: *mut c_void) -> HRESULT);
com_trampoline!(set_cooperative_level(this: *mut c_void, window: *mut c_void, flags: u32) -> HRESULT);

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DirectInput8Create(
    instance: HINSTANCE,
    version: u32,
    interface_id: *const GUID,
    output: *mut *mut c_void,
    outer: *mut c_void,
) -> HRESULT {
    unsafe { crate::runtime::DirectInput8Create(instance, version, interface_id, output, outer) }
}

macro_rules! proxy_export {
    ($name:ident() -> $result:ty) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn $name() -> $result {
            unsafe { crate::runtime::$name() }
        }
    };
}

proxy_export!(DllCanUnloadNow() -> HRESULT);
proxy_export!(DllRegisterServer() -> HRESULT);
proxy_export!(DllUnregisterServer() -> HRESULT);
proxy_export!(GetdfDIJoystick() -> *const c_void);

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    class_id: *const GUID,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    unsafe { crate::runtime::DllGetClassObject(class_id, interface_id, output) }
}
