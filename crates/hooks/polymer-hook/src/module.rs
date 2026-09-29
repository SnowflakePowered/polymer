use std::ffi::CStr;
use windows_sys::Win32::{
    Foundation::HMODULE,
    System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
};

pub fn loaded(name: &[u16]) -> Option<HMODULE> {
    let module = unsafe { GetModuleHandleW(name.as_ptr()) };
    (!module.is_null()).then_some(module)
}

/// # Safety
///
/// `T` must be the function pointer type matching the ABI of the named export.
pub unsafe fn symbol<T: Copy>(module: HMODULE, name: &CStr) -> Option<T> {
    let address = unsafe { GetProcAddress(module, name.as_ptr().cast()) }?;
    debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of_val(&address));
    Some(unsafe { std::mem::transmute_copy(&address) })
}
