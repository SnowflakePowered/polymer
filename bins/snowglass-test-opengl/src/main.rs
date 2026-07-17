#![allow(unsafe_code)]

use std::{ffi::c_void, mem::size_of, ptr};
use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
    Graphics::{
        Gdi::GetDC,
        OpenGL::{
            ChoosePixelFormat, PFD_DOUBLEBUFFER, PFD_DRAW_TO_WINDOW, PFD_MAIN_PLANE,
            PFD_SUPPORT_OPENGL, PFD_TYPE_RGBA, PIXELFORMATDESCRIPTOR, SetPixelFormat, SwapBuffers,
            glClear, glClearColor, wglCreateContext, wglMakeCurrent,
        },
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{
        CS_OWNDC, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DispatchMessageW, MSG, PM_REMOVE,
        PeekMessageW, PostQuitMessage, RegisterClassW, SW_SHOW, ShowWindow, TranslateMessage,
        WM_DESTROY, WM_QUIT, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    },
};

const GL_COLOR_BUFFER_BIT: u32 = 0x0000_4000;

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn main() {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(ptr::null());
        let class_name = wide("SnowglassOpenGlTest");
        let class = WNDCLASSW {
            style: CS_OWNDC,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..Default::default()
        };
        assert_ne!(RegisterClassW(&class), 0, "RegisterClassW failed");
        let title = wide("Snowglass OpenGL capture test");
        let window = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            960,
            600,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null::<c_void>(),
        );
        assert!(!window.is_null(), "CreateWindowExW failed");
        let dc = GetDC(window);
        let pixel_format = PIXELFORMATDESCRIPTOR {
            nSize: size_of::<PIXELFORMATDESCRIPTOR>() as u16,
            nVersion: 1,
            dwFlags: PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER,
            iPixelType: PFD_TYPE_RGBA,
            cColorBits: 32,
            cAlphaBits: 8,
            cDepthBits: 24,
            iLayerType: PFD_MAIN_PLANE as u8,
            ..Default::default()
        };
        let format = ChoosePixelFormat(dc, &pixel_format);
        assert_ne!(format, 0, "ChoosePixelFormat failed");
        assert_ne!(
            SetPixelFormat(dc, format, &pixel_format),
            0,
            "SetPixelFormat failed"
        );
        let context = wglCreateContext(dc);
        assert!(!context.is_null(), "wglCreateContext failed");
        assert_ne!(wglMakeCurrent(dc, context), 0, "wglMakeCurrent failed");
        ShowWindow(window, SW_SHOW);

        let start = std::time::Instant::now();
        let mut message = MSG::default();
        loop {
            while PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                if message.message == WM_QUIT {
                    return;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let phase = start.elapsed().as_secs_f32();
            glClearColor(
                phase.sin().mul_add(0.5, 0.5),
                (phase * 0.7).sin().mul_add(0.5, 0.5),
                (phase * 1.3).sin().mul_add(0.5, 0.5),
                1.0,
            );
            glClear(GL_COLOR_BUFFER_BIT);
            SwapBuffers(dc);
        }
    }
}
