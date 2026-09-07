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
