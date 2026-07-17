use std::{ffi::c_void, sync::OnceLock};
use windows_sys::{
    Win32::{
        Foundation::{GetLastError, HMODULE},
        System::{
            LibraryLoader::{GetProcAddress, LoadLibraryExW},
            SystemInformation::GetSystemDirectoryW,
        },
    },
    core::PCSTR,
};

pub(crate) type D3D11CreateDeviceFn = unsafe extern "system" fn(
    *mut c_void,
    i32,
    HMODULE,
    u32,
    *const i32,
    u32,
    u32,
    *mut *mut c_void,
    *mut i32,
    *mut *mut c_void,
) -> i32;

pub(crate) type D3D11CreateDeviceAndSwapChainFn = unsafe extern "system" fn(
    *mut c_void,
    i32,
    HMODULE,
    u32,
    *const i32,
    u32,
    u32,
    *const c_void,
    *mut *mut c_void,
    *mut *mut c_void,
    *mut i32,
    *mut *mut c_void,
) -> i32;
struct SystemD3D11 {
    _module: HMODULE,
    create_device: D3D11CreateDeviceFn,
    create_device_and_swapchain: D3D11CreateDeviceAndSwapChainFn,
}

unsafe impl Send for SystemD3D11 {}
unsafe impl Sync for SystemD3D11 {}

static SYSTEM_D3D11: OnceLock<Result<SystemD3D11, u32>> = OnceLock::new();

impl SystemD3D11 {
    fn load() -> Result<Self, u32> {
        let mut path = vec![0u16; 32_768];
        let length = unsafe { GetSystemDirectoryW(path.as_mut_ptr(), path.len() as u32) } as usize;
        if length == 0 || length >= path.len() {
            return Err(unsafe { GetLastError() });
        }
        path.truncate(length);
        path.extend("\\d3d11.dll\0".encode_utf16());
        let module = unsafe { LoadLibraryExW(path.as_ptr(), std::ptr::null_mut(), 0) };
        if module.is_null() {
            return Err(unsafe { GetLastError() });
        }
        Ok(Self {
            _module: module,
            create_device: unsafe { resolve(module, c"D3D11CreateDevice")? },
            create_device_and_swapchain: unsafe {
                resolve(module, c"D3D11CreateDeviceAndSwapChain")?
            },
        })
    }
}

fn system_d3d11() -> Result<&'static SystemD3D11, i32> {
    SYSTEM_D3D11
        .get_or_init(SystemD3D11::load)
        .as_ref()
        .map_err(|error| hresult_from_win32(*error))
}

pub(crate) fn system_create_device() -> Result<D3D11CreateDeviceFn, i32> {
    system_d3d11().map(|system| system.create_device)
}

pub(crate) fn system_create_device_and_swapchain() -> Result<D3D11CreateDeviceAndSwapChainFn, i32> {
    system_d3d11().map(|system| system.create_device_and_swapchain)
}

unsafe fn resolve<T: Copy>(module: HMODULE, name: &'static std::ffi::CStr) -> Result<T, u32> {
    let Some(address) = (unsafe { GetProcAddress(module, name.as_ptr().cast::<u8>() as PCSTR) })
    else {
        return Err(unsafe { GetLastError() });
    };
    Ok(unsafe { std::mem::transmute_copy(&address) })
}

const fn hresult_from_win32(error: u32) -> i32 {
    if error == 0 {
        0
    } else {
        ((error & 0xffff) | 0x8007_0000) as i32
    }
}
