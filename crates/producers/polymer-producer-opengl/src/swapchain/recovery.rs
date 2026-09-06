use crate::interop::{Hdc, proc};
use std::ffi::c_void;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct RecoveryState {
    pub(super) surface: Option<RecoverySurface>,
    attempted: Option<Instant>,
}

impl RecoveryState {
    /// Called with the original context current on the presenting thread.
    pub(super) unsafe fn prepare(&mut self, dc: Hdc) {
        let now = Instant::now();
        if self.surface.is_some()
            || self.attempted.is_some_and(|attempt| {
                now.saturating_duration_since(attempt) < Duration::from_secs(1)
            })
        {
            return;
        }
        self.attempted = Some(now);
        self.surface = unsafe { RecoverySurface::new(dc, (1, 1)) };
    }
}

type Create = unsafe extern "system" fn(Hdc, i32, i32, i32, *const i32) -> *mut c_void;
type GetDc = unsafe extern "system" fn(*mut c_void) -> Hdc;
type ReleaseDc = unsafe extern "system" fn(*mut c_void, Hdc) -> i32;
type Destroy = unsafe extern "system" fn(*mut c_void) -> i32;
type Attributes = unsafe extern "system" fn(Hdc, i32, i32, u32, *const i32, *mut i32) -> i32;

#[link(name = "gdi32")]
unsafe extern "system" {
    fn GetPixelFormat(dc: Hdc) -> i32;
}

/// A one-pixel drawable on the original device and pixel format. It has no HWND
/// lifetime or window-thread ownership, and is used only for context retirement.
pub(super) struct RecoverySurface {
    buffer: usize,
    pub(super) dc: usize,
    release: ReleaseDc,
    destroy: Destroy,
}

impl RecoverySurface {
    /// The original context is current, so extension lookup is device-specific.
    pub(super) unsafe fn new(dc: Hdc, extent: (i32, i32)) -> Option<Self> {
        let create = unsafe { proc::<Create>(b"wglCreatePbufferARB\0") }?;
        let get_dc = unsafe { proc::<GetDc>(b"wglGetPbufferDCARB\0") }?;
        let release = unsafe { proc::<ReleaseDc>(b"wglReleasePbufferDCARB\0") }?;
        let destroy = unsafe { proc::<Destroy>(b"wglDestroyPbufferARB\0") }?;
        let attributes = unsafe { proc::<Attributes>(b"wglGetPixelFormatAttribivARB\0") }?;
        let format = unsafe { GetPixelFormat(dc) };
        let mut supported = 0;
        if unsafe { attributes(dc, format, 0, 1, &0x202d, &mut supported) } == 0 || supported == 0 {
            return None;
        }
        let buffer = unsafe { create(dc, format, extent.0, extent.1, &0) };
        if buffer.is_null() {
            return None;
        }
        let surface = Self {
            buffer: buffer as usize,
            dc: unsafe { get_dc(buffer) } as usize,
            release,
            destroy,
        };
        (surface.dc != 0).then_some(surface)
    }
}

impl Drop for RecoverySurface {
    fn drop(&mut self) {
        // The caller restores the previous binding before releasing this DC.
        unsafe {
            if self.dc != 0 {
                (self.release)(self.buffer as *mut c_void, self.dc as Hdc);
            }
            (self.destroy)(self.buffer as *mut c_void);
        }
    }
}
