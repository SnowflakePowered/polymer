//! Early-process DirectInput detour installer.
#![allow(unsafe_code)]

mod install;
mod sys;

pub use sys::SnowglassInstallDinputHook;

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

        let target = dinput8::system_direct_input8_create().unwrap();
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
