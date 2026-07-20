//! Early-process DirectInput detour and COM device interception.
#![allow(unsafe_code)]

mod hook_chain;
mod hooks;
mod install;
mod runtime;
mod sys;
mod vtable;

pub(crate) type DirectInput8CreateFn = unsafe extern "system" fn(
    windows_sys::Win32::Foundation::HINSTANCE,
    u32,
    *const windows_sys::core::GUID,
    *mut *mut std::ffi::c_void,
    *mut std::ffi::c_void,
) -> windows_sys::core::HRESULT;

pub use sys::SnowglassInstallDinputHook;

snowglass_hook::register_hooks! {
    hooks::register_direct_input_8_create -> runtime::direct_input_8_create;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;
    use windows_sys::core::GUID;

    #[test]
    fn installation_is_idempotent_and_intercepts_the_system_export() {
        const IID_IDIRECT_INPUT_8_W: GUID = GUID::from_u128(0xbf798031_483a_4da2_aa99_5d64ed369700);
        assert_eq!(unsafe { SnowglassInstallDinputHook() }, 0);
        assert_eq!(unsafe { SnowglassInstallDinputHook() }, 0);

        let target = windows_sys::Win32::Devices::HumanInterfaceDevice::DirectInput8Create;
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
