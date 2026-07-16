//! Loading and forwarding of the real system `dinput8.dll`.

use std::{ffi::c_void, sync::OnceLock};
use windows_sys::{
    Win32::{
        Foundation::{GetLastError, HINSTANCE, HMODULE},
        System::{
            LibraryLoader::{GetProcAddress, LoadLibraryExW},
            SystemInformation::GetSystemDirectoryW,
        },
    },
    core::{GUID, HRESULT, PCSTR},
};

pub type DirectInput8CreateFn = unsafe extern "system" fn(
    HINSTANCE,
    u32,
    *const GUID,
    *mut *mut c_void,
    *mut c_void,
) -> HRESULT;
type DllCanUnloadNowFn = unsafe extern "system" fn() -> HRESULT;
type DllGetClassObjectFn =
    unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> HRESULT;
type DllRegistrationFn = unsafe extern "system" fn() -> HRESULT;
type GetdfDIJoystickFn = unsafe extern "system" fn() -> *const c_void;

static SYSTEM_DINPUT8: OnceLock<Result<SystemDinput8, u32>> = OnceLock::new();

struct SystemDinput8 {
    _module: HMODULE,
    direct_input_8_create: DirectInput8CreateFn,
    dll_can_unload_now: DllCanUnloadNowFn,
    dll_get_class_object: DllGetClassObjectFn,
    dll_register_server: DllRegistrationFn,
    dll_unregister_server: DllRegistrationFn,
    get_df_di_joystick: GetdfDIJoystickFn,
}

// The module remains loaded for the process lifetime and all function pointers
// refer to immutable executable memory in that module.
unsafe impl Send for SystemDinput8 {}
unsafe impl Sync for SystemDinput8 {}

impl SystemDinput8 {
    fn load() -> Result<Self, u32> {
        let mut system_path = vec![0u16; 32_768];
        let length = unsafe {
            GetSystemDirectoryW(
                system_path.as_mut_ptr(),
                system_path.len().try_into().unwrap(),
            )
        } as usize;
        if length == 0 || length >= system_path.len() {
            return Err(unsafe { GetLastError() });
        }
        system_path.truncate(length);
        system_path.extend("\\dinput8.dll\0".encode_utf16());
        let module = unsafe { LoadLibraryExW(system_path.as_ptr(), std::ptr::null_mut(), 0) };
        if module.is_null() {
            return Err(unsafe { GetLastError() });
        }
        Ok(Self {
            _module: module,
            direct_input_8_create: unsafe { resolve(module, c"DirectInput8Create")? },
            dll_can_unload_now: unsafe { resolve(module, c"DllCanUnloadNow")? },
            dll_get_class_object: unsafe { resolve(module, c"DllGetClassObject")? },
            dll_register_server: unsafe { resolve(module, c"DllRegisterServer")? },
            dll_unregister_server: unsafe { resolve(module, c"DllUnregisterServer")? },
            get_df_di_joystick: unsafe { resolve(module, c"GetdfDIJoystick")? },
        })
    }
}

fn system_dinput8() -> Result<&'static SystemDinput8, HRESULT> {
    SYSTEM_DINPUT8
        .get_or_init(SystemDinput8::load)
        .as_ref()
        .map_err(|error| hresult_from_win32(*error))
}

pub fn system_direct_input8_create() -> Result<DirectInput8CreateFn, HRESULT> {
    system_dinput8().map(|system| system.direct_input_8_create)
}

pub(crate) unsafe fn dll_can_unload_now() -> HRESULT {
    match system_dinput8() {
        Ok(system) => unsafe { (system.dll_can_unload_now)() },
        Err(error) => error,
    }
}

pub(crate) unsafe fn dll_get_class_object(
    class_id: *const GUID,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    match system_dinput8() {
        Ok(system) => unsafe { (system.dll_get_class_object)(class_id, interface_id, output) },
        Err(error) => error,
    }
}

pub(crate) unsafe fn dll_register_server() -> HRESULT {
    match system_dinput8() {
        Ok(system) => unsafe { (system.dll_register_server)() },
        Err(error) => error,
    }
}

pub(crate) unsafe fn dll_unregister_server() -> HRESULT {
    match system_dinput8() {
        Ok(system) => unsafe { (system.dll_unregister_server)() },
        Err(error) => error,
    }
}

pub(crate) unsafe fn get_df_di_joystick() -> *const c_void {
    match system_dinput8() {
        Ok(system) => unsafe { (system.get_df_di_joystick)() },
        Err(_) => std::ptr::null(),
    }
}

unsafe fn resolve<T: Copy>(module: HMODULE, name: &'static std::ffi::CStr) -> Result<T, u32> {
    let address = unsafe { GetProcAddress(module, name.as_ptr().cast::<u8>() as PCSTR) };
    let Some(address) = address else {
        return Err(unsafe { GetLastError() });
    };
    Ok(unsafe { std::mem::transmute_copy(&address) })
}

const fn hresult_from_win32(error: u32) -> HRESULT {
    if error == 0 {
        0
    } else {
        ((error & 0x0000_ffff) | 0x8007_0000) as i32
    }
}
