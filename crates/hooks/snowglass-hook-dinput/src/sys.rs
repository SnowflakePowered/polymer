//! ABI trampolines used by the injector and inline hooks.

use std::ffi::c_void;
use windows_sys::{
    Win32::{Devices::HumanInterfaceDevice::DIDEVICEOBJECTDATA, Foundation::HINSTANCE},
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
        crate::hooks::dispatch_direct_input_8_create(
            crate::install::call_original,
            instance,
            version,
            interface_id,
            output,
            outer,
        )
    }
}

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
