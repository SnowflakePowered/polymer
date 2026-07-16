//! Lightweight `dinput8.dll` ABI trampolines.

use std::ffi::c_void;
use windows_sys::Win32::Devices::HumanInterfaceDevice::DIDEVICEOBJECTDATA;
use windows_sys::{
    Win32::Foundation::HINSTANCE,
    core::{GUID, HRESULT},
};

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn direct_input_release(
        this: *mut c_void
    ) -> u32 => crate::hooks::dispatch_direct_input_release;
}
snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn create_device(
        this: *mut c_void,
        class: *const GUID,
        output: *mut *mut c_void,
        outer: *mut c_void,
    ) -> HRESULT => crate::hooks::dispatch_create_device;
}
snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn device_release(
        this: *mut c_void
    ) -> u32 => crate::hooks::dispatch_device_release;
}
snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn acquire(
        this: *mut c_void
    ) -> HRESULT => crate::hooks::dispatch_acquire;
}
snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn get_device_state(
        this: *mut c_void,
        size: u32,
        output: *mut c_void,
    ) -> HRESULT => crate::hooks::dispatch_get_device_state;
}
snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn get_device_data(
        this: *mut c_void,
        object_size: u32,
        output: *mut DIDEVICEOBJECTDATA,
        count: *mut u32,
        flags: u32,
    ) -> HRESULT => crate::hooks::dispatch_get_device_data;
}
snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn set_data_format(
        this: *mut c_void,
        format: *const c_void,
    ) -> HRESULT => crate::hooks::dispatch_set_data_format;
}
snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn set_event_notification(
        this: *mut c_void,
        event: *mut c_void,
    ) -> HRESULT => crate::hooks::dispatch_set_event_notification;
}
snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn set_cooperative_level(
        this: *mut c_void,
        window: *mut c_void,
        flags: u32,
    ) -> HRESULT => crate::hooks::dispatch_set_cooperative_level;
}

snowglass_hook::trampoline! {
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn DirectInput8Create(
        instance: HINSTANCE,
        version: u32,
        interface_id: *const GUID,
        output: *mut *mut c_void,
        outer: *mut c_void,
    ) -> HRESULT => crate::runtime::DirectInput8Create;
}

snowglass_hook::trampoline! {
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn DllCanUnloadNow(
    ) -> HRESULT => crate::loader::dll_can_unload_now;
}
snowglass_hook::trampoline! {
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn DllRegisterServer(
    ) -> HRESULT => crate::loader::dll_register_server;
}
snowglass_hook::trampoline! {
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn DllUnregisterServer(
    ) -> HRESULT => crate::loader::dll_unregister_server;
}
snowglass_hook::trampoline! {
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn GetdfDIJoystick(
    ) -> *const c_void => crate::loader::get_df_di_joystick;
}

snowglass_hook::trampoline! {
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn DllGetClassObject(
        class_id: *const GUID,
        interface_id: *const GUID,
        output: *mut *mut c_void,
    ) -> HRESULT => crate::loader::dll_get_class_object;
}
