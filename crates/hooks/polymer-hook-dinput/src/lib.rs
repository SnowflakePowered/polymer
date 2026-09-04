//! Early-process DirectInput detour and COM device interception.
#![allow(unsafe_code)]

mod hooks;
mod install;
mod runtime;
mod sys;
mod vtable;

pub use sys::{PolymerInstallDinputHook, PolymerUninstallDinputHook};

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;
    use windows_sys::core::GUID;

    #[test]
    fn installation_is_idempotent_and_intercepts_the_system_export() {
        const IID_IDIRECT_INPUT_8_W: GUID = GUID::from_u128(0xbf798031_483a_4da2_aa99_5d64ed369700);
        assert_eq!(unsafe { PolymerInstallDinputHook() }, 0);
        assert_eq!(unsafe { PolymerInstallDinputHook() }, 0);

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
        let get_device_status: unsafe extern "system" fn(*mut c_void, *const GUID) -> i32 =
            unsafe { std::mem::transmute(*vtable.add(5)) };
        assert!(
            unsafe {
                get_device_status(
                    interface,
                    &windows_sys::Win32::Devices::HumanInterfaceDevice::GUID_SysKeyboard,
                )
            } >= 0
        );

        const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
        let query_interface: unsafe extern "system" fn(
            *mut c_void,
            *const GUID,
            *mut *mut c_void,
        ) -> i32 = unsafe { std::mem::transmute(*vtable) };
        let mut unknown = std::ptr::null_mut();
        assert_eq!(
            unsafe { query_interface(interface, &IID_IUNKNOWN, &mut unknown) },
            0
        );
        assert_eq!(unknown, interface);
        let create_device: unsafe extern "system" fn(
            *mut c_void,
            *const GUID,
            *mut *mut c_void,
            *mut c_void,
        ) -> i32 = unsafe { std::mem::transmute(*vtable.add(3)) };
        let mut keyboard = std::ptr::null_mut();
        let create_result = unsafe {
            create_device(
                interface,
                &windows_sys::Win32::Devices::HumanInterfaceDevice::GUID_SysKeyboard,
                &mut keyboard,
                std::ptr::null_mut(),
            )
        };
        assert!(
            create_result >= 0,
            "proxied CreateDevice failed: {create_result:#x}"
        );
        assert!(!keyboard.is_null());
        let keyboard_vtable = unsafe { *keyboard.cast::<*const usize>() };
        let keyboard_release: unsafe extern "system" fn(*mut c_void) -> u32 =
            unsafe { std::mem::transmute(*keyboard_vtable.add(2)) };
        assert_eq!(unsafe { keyboard_release(keyboard) }, 0);

        let mut mouse = std::ptr::null_mut();
        let create_result = unsafe {
            create_device(
                interface,
                &windows_sys::Win32::Devices::HumanInterfaceDevice::GUID_SysMouse,
                &mut mouse,
                std::ptr::null_mut(),
            )
        };
        assert!(
            create_result >= 0,
            "proxied mouse CreateDevice failed: {create_result:#x}"
        );
        assert!(!mouse.is_null());
        let mouse_vtable = unsafe { *mouse.cast::<*const usize>() };
        let mouse_release: unsafe extern "system" fn(*mut c_void) -> u32 =
            unsafe { std::mem::transmute(*mouse_vtable.add(2)) };
        assert_eq!(unsafe { mouse_release(mouse) }, 0);
        let release = unsafe {
            std::mem::transmute::<usize, unsafe extern "system" fn(*mut c_void) -> u32>(
                *vtable.add(2),
            )
        };
        assert_eq!(unsafe { release(unknown) }, 1);
        assert_eq!(unsafe { release(interface) }, 0);

        assert_eq!(PolymerUninstallDinputHook(), 0);
        assert_eq!(unsafe { PolymerInstallDinputHook() }, 0);
        assert_eq!(PolymerUninstallDinputHook(), 0);
    }
}
