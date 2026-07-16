//! DirectInput object interception policy.

use std::ffi::c_void;
use windows_sys::{
    Win32::Foundation::HINSTANCE,
    core::{GUID, HRESULT},
};

pub(crate) unsafe fn direct_input_8_create(
    instance: HINSTANCE,
    version: u32,
    interface_id: *const GUID,
    output: *mut *mut c_void,
    outer: *mut c_void,
) -> HRESULT {
    unsafe {
        dinput8::intercept_direct_input8_create(
            crate::install::call_original,
            instance,
            version,
            interface_id,
            output,
            outer,
        )
    }
}
