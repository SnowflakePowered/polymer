use crate::interop::{Hdc, Hglrc, current_context};

type GetCurrentReadDc = unsafe extern "system" fn() -> Hdc;
type MakeContextCurrent = unsafe extern "system" fn(Hdc, Hdc, Hglrc) -> i32;

#[link(name = "opengl32")]
unsafe extern "system" {
    fn wglGetCurrentDC() -> Hdc;
    fn wglMakeCurrent(dc: Hdc, context: Hglrc) -> i32;
}

/// Temporary activation stays on the deleting thread. WGL rejects contexts
/// that are unavailable, including those current on another thread.
pub(super) struct ContextBinding {
    previous_context: Hglrc,
    previous_dc: Hdc,
    previous_read: Option<(Hdc, MakeContextCurrent)>,
    restore_required: bool,
}

impl ContextBinding {
    /// The context and candidate DCs originate from observed application handles.
    pub(super) unsafe fn acquire(
        context: usize,
        devices: impl IntoIterator<Item = usize>,
    ) -> Option<Self> {
        // Resolve these against the application context before activating a
        // possibly different device's context. WGL permits distinct read/draw
        // DCs, which wglMakeCurrent alone cannot restore.
        let read_functions = unsafe {
            crate::interop::proc::<GetCurrentReadDc>(b"wglGetCurrentReadDCARB\0")
                .zip(crate::interop::proc::<MakeContextCurrent>(
                    b"wglMakeContextCurrentARB\0",
                ))
                .or_else(|| {
                    crate::interop::proc::<GetCurrentReadDc>(b"wglGetCurrentReadDCEXT\0").zip(
                        crate::interop::proc::<MakeContextCurrent>(b"wglMakeContextCurrentEXT\0"),
                    )
                })
        };
        let mut previous = Self {
            previous_context: unsafe { current_context() },
            previous_dc: unsafe { wglGetCurrentDC() },
            previous_read: read_functions.map(|(get, make)| (unsafe { get() }, make)),
            restore_required: false,
        };
        if previous.previous_context as usize == context {
            return Some(previous);
        }
        for device in devices {
            previous.restore_required = true;
            if unsafe { wglMakeCurrent(device as Hdc, context as Hglrc) } != 0 {
                return Some(previous);
            }
        }
        // Failed activation can unbind the previous context. Drop restores it
        // even when no recorded DC could activate the retiring context.
        None
    }
}

impl Drop for ContextBinding {
    fn drop(&mut self) {
        if self.restore_required {
            let restored = unsafe {
                match self.previous_read {
                    Some((read, make)) => make(self.previous_dc, read, self.previous_context),
                    None => wglMakeCurrent(self.previous_dc, self.previous_context),
                }
            };
            if restored == 0 {
                tracing::error!(
                    "polymer: could not restore OpenGL context after capture retirement"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::{
        Graphics::{
            Gdi::{GetDC, ReleaseDC},
            OpenGL::*,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_OVERLAPPEDWINDOW},
    };

    #[test]
    fn native_context_activation_restores_bindings_and_rejects_cross_thread_ownership() {
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
            let first = wglCreateContext(dc);
            let second = wglCreateContext(dc);
            assert!(!first.is_null() && !second.is_null());
            assert_ne!(super::wglMakeCurrent(dc, first), 0);
            crate::timeline::tests::abandoned_setup_releases_imported_timeline();
            crate::interop::nv_dx::Ring::verify_locked_retirement();
            use super::super::native_capture::{NativeCaptureFixture, Transport};
            for transport in [Transport::Pbo, Transport::Nv, Transport::Ext] {
                NativeCaptureFixture::capture(window as isize, transport, false, false);
                NativeCaptureFixture::capture(window as isize, transport, true, false);
                let drawable = super::super::recovery::RecoverySurface::new(dc, (64, 64)).unwrap();
                assert_ne!(super::wglMakeCurrent(drawable.dc as Hdc, first), 0);
                NativeCaptureFixture::capture(window as isize, transport, false, true);
                assert_ne!(super::wglMakeCurrent(dc, first), 0);
            }
            let read_surface = super::super::recovery::RecoverySurface::new(dc, (1, 1)).unwrap();
            let make_read =
                crate::interop::proc::<MakeContextCurrent>(b"wglMakeContextCurrentARB\0").unwrap();
            let get_read =
                crate::interop::proc::<GetCurrentReadDc>(b"wglGetCurrentReadDCARB\0").unwrap();
            assert_ne!(make_read(dc, read_surface.dc as Hdc, first), 0);
            {
                let _binding = ContextBinding::acquire(second as usize, [dc as usize]).unwrap();
                assert_eq!(current_context(), second);
                let owned = second as usize;
                let device = dc as usize;
                std::thread::spawn(move || {
                    assert!(ContextBinding::acquire(owned, [device]).is_none());
                    assert!(current_context().is_null());
                })
                .join()
                .unwrap();
            }
            assert_eq!(current_context(), first);
            assert_eq!(super::wglGetCurrentDC(), dc);
            assert_eq!(get_read(), read_surface.dc as Hdc);
            assert!(ContextBinding::acquire(second as usize, []).is_none());
            assert_eq!(current_context(), first);
            assert_eq!(get_read(), read_surface.dc as Hdc);
            assert!(ContextBinding::acquire(second as usize, [0]).is_none());
            assert_eq!(current_context(), first);
            assert_eq!(get_read(), read_surface.dc as Hdc);
            let key = (dc as usize, second as usize);
            super::super::captures()
                .entries
                .insert(key, parking_lot::Mutex::new(Default::default()));
            super::super::before_delete_context(second as usize);
            assert!(!super::super::captures().entries.contains_key(&key));
            assert_eq!(current_context(), first);
            assert_eq!(get_read(), read_surface.dc as Hdc);
            assert_ne!(super::wglMakeCurrent(dc, first), 0);
            drop(read_surface);
            assert_ne!(wglDeleteContext(second), 0);
            // Simulate an unsupported recovery surface and an unusable saved
            // DC. A failed activation must not retire the context prematurely;
            // successful native deletion must invalidate all remaining entries.
            let lost = wglCreateContext(dc);
            assert!(!lost.is_null());
            let lost_key = (0, lost as usize);
            let store = super::super::captures();
            store
                .entries
                .insert(lost_key, parking_lot::Mutex::new(Default::default()));
            store.recovery.insert(lost as usize, Default::default());
            super::super::before_delete_context(lost as usize);
            assert!(store.entries.contains_key(&lost_key));
            assert_eq!(current_context(), first);
            assert_ne!(wglDeleteContext(lost), 0);
            super::super::after_delete_context(lost as usize);
            assert!(!store.entries.contains_key(&lost_key));
            assert!(!store.recovery.contains_key(&(lost as usize)));
            assert_eq!(current_context(), first);
            let store = super::super::captures();
            store.recovery.insert(first as usize, Default::default());
            store.prepare_context(first as usize, dc as usize);
            assert!(
                store
                    .recovery
                    .get(&(first as usize))
                    .unwrap()
                    .surface
                    .is_some(),
                "native pixel format does not support a recovery pbuffer"
            );
            assert_ne!(
                super::wglMakeCurrent(std::ptr::null_mut(), std::ptr::null_mut()),
                0
            );
            ReleaseDC(window, dc);
            DestroyWindow(window);
            // No application DC remains. Cleanup must activate the context
            // through its retained, device-compatible pbuffer.
            let orphan = (0, first as usize);
            store
                .entries
                .insert(orphan, parking_lot::Mutex::new(Default::default()));
            super::super::before_delete_context(first as usize);
            assert!(!store.entries.contains_key(&orphan));
            assert!(!store.recovery.contains_key(&(first as usize)));
            assert!(current_context().is_null());
            assert_ne!(wglDeleteContext(first), 0);
        }
    }
}
