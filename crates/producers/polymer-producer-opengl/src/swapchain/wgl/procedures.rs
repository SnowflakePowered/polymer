use super::WglSwapchain;
use crate::interop::{Hdc, Hglrc, proc};
use std::ffi::{CStr, c_void};

type Make = unsafe extern "system" fn(Hdc, Hdc, Hglrc) -> i32;
type Read = unsafe extern "system" fn() -> Hdc;

impl WglSwapchain {
    /// # Safety
    /// Name and result follow wglGetProcAddress's ABI.
    pub unsafe fn procedure(name: *const u8, address: *const c_void) -> *const c_void {
        if !Self::enabled()
            || name.is_null()
            || matches!(address as usize, 0 | 1 | 2 | 3 | usize::MAX)
        {
            return address;
        }
        match unsafe { CStr::from_ptr(name.cast()) }.to_bytes() {
            b"wglMakeContextCurrentARB" => Self::make_arb as *const c_void,
            b"wglMakeContextCurrentEXT" => Self::make_ext as *const c_void,
            b"wglGetCurrentReadDCARB" => Self::read_arb as *const c_void,
            b"wglGetCurrentReadDCEXT" => Self::read_ext as *const c_void,
            _ => address,
        }
    }

    // Resolve against the activated context's ICD, never a process-global
    // last-returned function pointer from an unrelated adapter/pixel format.
    unsafe extern "system" fn make_arb(draw: Hdc, read: Hdc, context: Hglrc) -> i32 {
        unsafe {
            Self::make_current(draw, read, context, |draw, read, context| {
                if context.is_null() {
                    return super::wglMakeCurrent(draw, context);
                }
                proc::<Make>(b"wglMakeContextCurrentARB\0")
                    .map_or(0, |make| make(draw, read, context))
            })
        }
    }
    unsafe extern "system" fn make_ext(draw: Hdc, read: Hdc, context: Hglrc) -> i32 {
        unsafe {
            Self::make_current(draw, read, context, |draw, read, context| {
                if context.is_null() {
                    return super::wglMakeCurrent(draw, context);
                }
                proc::<Make>(b"wglMakeContextCurrentEXT\0")
                    .map_or(0, |make| make(draw, read, context))
            })
        }
    }
    unsafe extern "system" fn read_arb() -> Hdc {
        let native = Self::native(|| unsafe {
            proc::<Read>(b"wglGetCurrentReadDCARB\0").map_or(std::ptr::null_mut(), |get| get())
        });
        Self::current_dc(native, true)
    }
    unsafe extern "system" fn read_ext() -> Hdc {
        let native = Self::native(|| unsafe {
            proc::<Read>(b"wglGetCurrentReadDCEXT\0").map_or(std::ptr::null_mut(), |get| get())
        });
        Self::current_dc(native, true)
    }
}
