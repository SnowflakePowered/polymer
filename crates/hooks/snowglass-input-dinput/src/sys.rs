//! Lightweight `dinput8.dll` ABI trampolines.

use std::ffi::c_void;
use windows_sys::Win32::Devices::HumanInterfaceDevice::DIDEVICEOBJECTDATA;
use windows_sys::{
    Win32::Foundation::HINSTANCE,
    core::{GUID, HRESULT},
};

macro_rules! com_trampoline {
    ($name:ident($($arg:ident: $ty:ty),* $(,)?) -> $result:ty => $dispatch:path) => {
        pub(crate) unsafe extern "system" fn $name($($arg: $ty),*) -> $result {
            unsafe { $dispatch($($arg),*) }
        }
    };
}

com_trampoline!(direct_input_release(this: *mut c_void) -> u32 => crate::hook::dispatch_direct_input_release);
com_trampoline!(create_device(
    this: *mut c_void,
    class: *const GUID,
    output: *mut *mut c_void,
    outer: *mut c_void,
) -> HRESULT => crate::hook::dispatch_create_device);
com_trampoline!(device_release(this: *mut c_void) -> u32 => crate::hook::dispatch_device_release);
com_trampoline!(acquire(this: *mut c_void) -> HRESULT => crate::hook::dispatch_acquire);
com_trampoline!(get_device_state(
    this: *mut c_void,
    size: u32,
    output: *mut c_void,
) -> HRESULT => crate::hook::dispatch_get_device_state);
com_trampoline!(get_device_data(
    this: *mut c_void,
    object_size: u32,
    output: *mut DIDEVICEOBJECTDATA,
    count: *mut u32,
    flags: u32,
) -> HRESULT => crate::hook::dispatch_get_device_data);
com_trampoline!(set_data_format(this: *mut c_void, format: *const c_void) -> HRESULT => crate::hook::dispatch_set_data_format);
com_trampoline!(set_event_notification(
    this: *mut c_void,
    event: *mut c_void,
) -> HRESULT => crate::hook::dispatch_set_event_notification);
com_trampoline!(set_cooperative_level(this: *mut c_void, window: *mut c_void, flags: u32) -> HRESULT => crate::hook::dispatch_set_cooperative_level);

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
    ($name:ident() -> $result:ty => $target:path) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn $name() -> $result {
            unsafe { $target() }
        }
    };
}

proxy_export!(DllCanUnloadNow() -> HRESULT => crate::loader::dll_can_unload_now);
proxy_export!(DllRegisterServer() -> HRESULT => crate::loader::dll_register_server);
proxy_export!(DllUnregisterServer() -> HRESULT => crate::loader::dll_unregister_server);
proxy_export!(GetdfDIJoystick() -> *const c_void => crate::loader::get_df_di_joystick);

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    class_id: *const GUID,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    unsafe { crate::loader::dll_get_class_object(class_id, interface_id, output) }
}
