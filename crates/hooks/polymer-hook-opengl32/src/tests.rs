use polymer_producer_opengl::WglSwapchain;
use std::{ffi::c_void, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Gdi::*, OpenGL::*},
    UI::WindowsAndMessaging::*,
};

struct Installed;
impl Drop for Installed {
    fn drop(&mut self) {
        assert_eq!(crate::install::uninstall(), 0);
    }
}

struct Window {
    hwnd: HWND,
    dc: HDC,
    context: HGLRC,
}
impl Window {
    unsafe fn new(title: &str, width: i32, height: i32) -> Self {
        let title = title.encode_utf16().chain([0]).collect::<Vec<_>>();
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                windows_sys::w!("STATIC"),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                320,
                240,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        assert!(!hwnd.is_null());
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        assert_ne!(
            unsafe { AdjustWindowRectEx(&mut rect, WS_POPUP, 0, 0) },
            0
        );
        assert_ne!(
            unsafe {
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    0,
                    0,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOMOVE | SWP_NOZORDER,
                )
            },
            0
        );
        let dc = unsafe { GetDC(hwnd) };
        let descriptor = PIXELFORMATDESCRIPTOR {
            nSize: std::mem::size_of::<PIXELFORMATDESCRIPTOR>() as u16,
            nVersion: 1,
            dwFlags: PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER,
            iPixelType: PFD_TYPE_RGBA,
            cColorBits: 32,
            cAlphaBits: 8,
            cDepthBits: 24,
            cStencilBits: 8,
            ..Default::default()
        };
        let format = unsafe { ChoosePixelFormat(dc, &descriptor) };
        assert_ne!(format, 0);
        assert_ne!(unsafe { SetPixelFormat(dc, format, &descriptor) }, 0);
        let context = unsafe { wglCreateContext(dc) };
        assert!(!context.is_null());
        Self { hwnd, dc, context }
    }

    unsafe fn bind(&self) -> HDC {
        assert_ne!(unsafe { wglMakeCurrent(self.dc, self.context) }, 0);
        assert_eq!(unsafe { wglGetCurrentDC() }, self.dc);
        WglSwapchain::native(|| unsafe { wglGetCurrentDC() })
    }

    unsafe fn clear(&self, color: [f32; 4]) {
        unsafe {
            glClearColor(color[0], color[1], color[2], color[3]);
            glClear(GL_COLOR_BUFFER_BIT);
        }
    }

    unsafe fn pixel(&self, x: i32, y: i32) -> [u8; 4] {
        let mut pixel = [0u8; 4];
        unsafe {
            glReadPixels(
                x,
                y,
                1,
                1,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                pixel.as_mut_ptr().cast(),
            );
        }
        assert_eq!(unsafe { glGetError() }, GL_NO_ERROR);
        pixel
    }

    unsafe fn extent(&self) -> [i32; 2] {
        let mut rect = RECT::default();
        assert_ne!(unsafe { GetClientRect(self.hwnd, &mut rect) }, 0);
        [rect.right, rect.bottom]
    }

    unsafe fn delete_context(&mut self) {
        assert_ne!(unsafe { wglMakeCurrent(null_mut(), null_mut()) }, 0);
        assert_ne!(unsafe { wglDeleteContext(self.context) }, 0);
        self.context = null_mut();
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            if !self.context.is_null() {
                self.delete_context();
            }
            ReleaseDC(self.hwnd, self.dc);
            DestroyWindow(self.hwnd);
        }
    }
}

/// Exercise the actual detours and driver, including a read drawable distinct
/// from the presenting drawable. Run alone: configuration is process-global.
#[test]
#[ignore = "requires a Windows OpenGL ICD with WGL_ARB_pbuffer; run with --test-threads=1"]
fn native_pbuffer_survives_resize_rebind_and_separate_read_dc() {
    unsafe {
        std::env::set_var(
            polymer_hook::config::RENDER_CONFIGURATION,
            r#"{"surfaces":[{"index":0,"pattern":"^pbuffer-regression-A$"},{"index":1,"pattern":"^pbuffer-regression-B$"}]}"#,
        );
        assert_eq!(crate::install::install(), 0);
        let _installed = Installed;
        let mut left = Window::new("pbuffer-regression-A", 64, 48);
        let right = Window::new("pbuffer-regression-B", 96, 72);
        let left_buffer = left.bind();
        assert_ne!(left_buffer, left.dc);
        assert_eq!(left.extent(), [64, 48]);
        left.clear([1.0, 0.0, 0.0, 1.0]);
        assert_eq!(left.pixel(63, 47), [255, 0, 0, 255]);

        for [width, height] in [[900, 700], [32, 64]] {
            assert_ne!(
                SetWindowPos(
                    left.hwnd,
                    null_mut(),
                    0,
                    0,
                    width,
                    height,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE
                ),
                0
            );
            assert_ne!(WglSwapchain::native(|| left.extent()), [64, 48]);
            assert_eq!(left.extent(), [64, 48]);
            assert_ne!(wglMakeCurrent(null_mut(), null_mut()), 0);
            assert!(wglGetCurrentDC().is_null());
            assert_eq!(left.bind(), left_buffer);
            // This pixel is outside the shrunken HWND, but inside the pbuffer.
            left.clear([1.0, 0.0, 0.0, 1.0]);
            assert_eq!(left.pixel(63, 47), [255, 0, 0, 255]);
        }

        let right_buffer = right.bind();
        assert_ne!(right_buffer, left_buffer);
        assert_eq!(right.extent(), [96, 72]);
        right.clear([0.0, 0.0, 1.0, 1.0]);
        type Make = unsafe extern "system" fn(HDC, HDC, HGLRC) -> i32;
        type Read = unsafe extern "system" fn() -> HDC;
        let make = wglGetProcAddress(c"wglMakeContextCurrentARB".as_ptr().cast())
            .expect("WGL_ARB_make_current_read");
        let get =
            wglGetProcAddress(c"wglGetCurrentReadDCARB".as_ptr().cast()).expect("read DC query");
        let make: Make = std::mem::transmute(make);
        let get: Read = std::mem::transmute(get);
        assert_ne!(make(left.dc, right.dc, left.context), 0);
        assert_eq!(wglGetCurrentDC(), left.dc);
        assert_eq!(get(), right.dc);
        assert_eq!(left.pixel(63, 47), [0, 0, 255, 255]);
        assert_eq!(WglSwapchain::present(left.dc.cast::<c_void>()), left_buffer);
        assert_eq!(get(), right.dc);
        assert_eq!(left.pixel(63, 47), [0, 0, 255, 255]);
        left.delete_context();
        assert_eq!(right.bind(), right_buffer);
        assert_eq!(right.pixel(95, 71), [0, 0, 255, 255]);
    }
}
