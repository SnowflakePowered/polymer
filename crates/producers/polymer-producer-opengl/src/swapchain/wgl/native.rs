use crate::interop::{Hdc, proc};
use polymer_presentation::Extent2D;
use std::ffi::c_void;

type Create = unsafe extern "system" fn(Hdc, i32, i32, i32, *const i32) -> *mut c_void;
type GetDc = unsafe extern "system" fn(*mut c_void) -> Hdc;
type Release = unsafe extern "system" fn(*mut c_void, Hdc) -> i32;
type Destroy = unsafe extern "system" fn(*mut c_void) -> i32;
type Query = unsafe extern "system" fn(*mut c_void, i32, *mut i32) -> i32;
type Attribute = unsafe extern "system" fn(Hdc, i32, i32, u32, *const i32, *mut i32) -> i32;

/// Owns a native default framebuffer, independent of the application's HWND.
pub(super) struct Pbuffer {
    handle: usize,
    pub dc: usize,
    release: Release,
    destroy: Destroy,
    query: Query,
}

impl Pbuffer {
    pub unsafe fn new(dc: Hdc, format: i32, extent: Extent2D) -> Result<Self, String> {
        let create: Create =
            unsafe { proc(b"wglCreatePbufferARB\0") }.ok_or("WGL_ARB_pbuffer unavailable")?;
        let get: GetDc =
            unsafe { proc(b"wglGetPbufferDCARB\0") }.ok_or("missing pbuffer DC function")?;
        let release = unsafe { proc(b"wglReleasePbufferDCARB\0") }
            .ok_or("missing pbuffer release function")?;
        let destroy: Destroy =
            unsafe { proc(b"wglDestroyPbufferARB\0") }.ok_or("missing pbuffer destroy function")?;
        let query =
            unsafe { proc(b"wglQueryPbufferARB\0") }.ok_or("missing pbuffer query function")?;
        let attribute: Attribute = unsafe { proc(b"wglGetPixelFormatAttribivARB\0") }
            .ok_or("WGL_ARB_pixel_format unavailable")?;
        let mut supported = 0;
        if unsafe { attribute(dc, format, 0, 1, &0x202D, &mut supported) } == 0 || supported == 0 {
            return Err(format!("pixel format {format} cannot render to a pbuffer"));
        }
        // Never accept WGL_PBUFFER_LARGEST_ARB's smaller allocation.
        let handle = unsafe {
            create(
                dc,
                format,
                extent.width as i32,
                extent.height as i32,
                [0].as_ptr(),
            )
        };
        if handle.is_null() {
            return Err(format!(
                "wglCreatePbufferARB: {}",
                std::io::Error::last_os_error()
            ));
        }
        let native_dc = unsafe { get(handle) };
        if native_dc.is_null() {
            unsafe { destroy(handle) };
            return Err("wglGetPbufferDCARB failed".into());
        }
        let buffer = Self {
            handle: handle as usize,
            dc: native_dc as usize,
            release,
            destroy,
            query,
        };
        if buffer.dimension(0x2034)? != extent.width as i32
            || buffer.dimension(0x2035)? != extent.height as i32
        {
            return Err("driver returned an unexpected pbuffer extent".into());
        }
        Ok(buffer)
    }

    fn dimension(&self, attribute: i32) -> Result<i32, String> {
        let mut value = 0;
        if unsafe { (self.query)(self.handle as _, attribute, &mut value) } == 0 {
            return Err("wglQueryPbufferARB failed".into());
        }
        Ok(value)
    }

    pub fn lost(&self) -> bool {
        self.dimension(0x2036).map_or(true, |lost| lost != 0)
    }
}

impl Drop for Pbuffer {
    fn drop(&mut self) {
        unsafe {
            (self.release)(self.handle as _, self.dc as _);
            (self.destroy)(self.handle as _);
        }
    }
}
