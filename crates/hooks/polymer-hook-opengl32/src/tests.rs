use windows_sys::Win32::{
    Graphics::{
        Gdi::{GetDC, ReleaseDC},
        OpenGL::*,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_OVERLAPPEDWINDOW},
};

struct Renderbuffers {
    generate: unsafe extern "system" fn(i32, *mut u32),
    bind: unsafe extern "system" fn(u32, u32),
    storage: unsafe extern "system" fn(u32, u32, i32, i32),
    exists: unsafe extern "system" fn(u32) -> u8,
}
impl Renderbuffers {
    unsafe fn load() -> Self {
        unsafe {
            Self {
                generate: std::mem::transmute(
                    wglGetProcAddress(c"glGenRenderbuffers".as_ptr().cast()).unwrap(),
                ),
                bind: std::mem::transmute(
                    wglGetProcAddress(c"glBindRenderbuffer".as_ptr().cast()).unwrap(),
                ),
                storage: std::mem::transmute(
                    wglGetProcAddress(c"glRenderbufferStorage".as_ptr().cast()).unwrap(),
                ),
                exists: std::mem::transmute(
                    wglGetProcAddress(c"glIsRenderbuffer".as_ptr().cast()).unwrap(),
                ),
            }
        }
    }

    unsafe fn verify_deletion(&self, name: &std::ffi::CStr, replacement: *const ()) {
        unsafe {
            let procedure = wglGetProcAddress(name.as_ptr().cast()).unwrap();
            assert_eq!(
                procedure as *const (), replacement,
                "installed resolver must return the deletion observer"
            );
            let delete: unsafe extern "system" fn(i32, *const u32) = std::mem::transmute(procedure);
            let mut buffer = 0;
            (self.generate)(1, &mut buffer);
            (self.bind)(0x8D41, buffer);
            (self.storage)(0x8D41, GL_RGBA8, 32, 24);
            assert_ne!((self.exists)(buffer), 0);
            // Textures and renderbuffers occupy separate native namespaces.
            glBindTexture(GL_TEXTURE_2D, buffer);
            assert_ne!(glIsTexture(buffer), 0);
            delete(0, std::ptr::null());
            assert_ne!((self.exists)(buffer), 0);
            delete(1, &buffer);
            assert_eq!(
                (self.exists)(buffer),
                0,
                "observer must forward native deletion"
            );
            assert_ne!(glIsTexture(buffer), 0);
            glDeleteTextures(1, &buffer);
            (self.generate)(1, &mut buffer);
            (self.bind)(0x8D41, buffer);
            (self.storage)(0x8D41, GL_RGBA8, 48, 32);
            assert_ne!((self.exists)(buffer), 0);
            delete(1, &buffer);
            assert_eq!(glGetError(), GL_NO_ERROR);
        }
    }
}

#[test]
fn installed_renderbuffer_deletion_aliases_forward_and_preserve_texture_names() {
    unsafe {
        let class = "STATIC\0".encode_utf16().collect::<Vec<_>>();
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            32,
            32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        );
        assert!(!window.is_null());
        let dc = GetDC(window);
        let descriptor = PIXELFORMATDESCRIPTOR {
            nSize: std::mem::size_of::<PIXELFORMATDESCRIPTOR>() as u16,
            nVersion: 1,
            dwFlags: PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER,
            iPixelType: PFD_TYPE_RGBA,
            cColorBits: 32,
            ..Default::default()
        };
        let format = ChoosePixelFormat(dc, &descriptor);
        assert_ne!(format, 0);
        assert_ne!(SetPixelFormat(dc, format, &descriptor), 0);
        let context = wglCreateContext(dc);
        assert!(!context.is_null());
        assert_ne!(wglMakeCurrent(dc, context), 0);
        assert_eq!(crate::install::install(), 0);
        let buffers = Renderbuffers::load();
        buffers.verify_deletion(
            c"glDeleteRenderbuffers",
            crate::hooks::delete_renderbuffers as *const (),
        );
        buffers.verify_deletion(
            c"glDeleteRenderbuffersEXT",
            crate::hooks::delete_renderbuffers_ext as *const (),
        );
        assert_eq!(crate::install::uninstall(), 0);
        assert_ne!(
            wglMakeCurrent(std::ptr::null_mut(), std::ptr::null_mut()),
            0
        );
        assert_ne!(wglDeleteContext(context), 0);
        ReleaseDC(window, dc);
        assert_ne!(DestroyWindow(window), 0);
    }
}
