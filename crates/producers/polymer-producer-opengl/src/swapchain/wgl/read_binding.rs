use crate::interop::{Hdc, Hglrc, proc};

type Read = unsafe extern "system" fn() -> Hdc;
type Make = unsafe extern "system" fn(Hdc, Hdc, Hglrc) -> i32;

#[derive(Clone, Copy)]
pub(super) struct ReadFunctions {
    get: Read,
    make: Make,
}
impl ReadFunctions {
    pub unsafe fn load() -> Option<Self> {
        unsafe {
            proc::<Read>(b"wglGetCurrentReadDCARB\0")
                .zip(proc::<Make>(b"wglMakeContextCurrentARB\0"))
                .or_else(|| {
                    proc::<Read>(b"wglGetCurrentReadDCEXT\0")
                        .zip(proc::<Make>(b"wglMakeContextCurrentEXT\0"))
                })
                .map(|(get, make)| Self { get, make })
        }
    }

    pub unsafe fn for_presentation(&self, draw: Hdc, context: Hglrc) -> Result<ReadBinding, ()> {
        let read = unsafe { (self.get)() };
        let restore = read != draw;
        if restore && unsafe { (self.make)(draw, draw, context) } == 0 {
            return Err(());
        }
        Ok(ReadBinding {
            draw,
            read,
            context,
            make: self.make,
            restore,
        })
    }
}

/// A separate application read drawable must not become the capture source.
pub(super) struct ReadBinding {
    draw: Hdc,
    read: Hdc,
    context: Hglrc,
    make: Make,
    restore: bool,
}
impl Drop for ReadBinding {
    fn drop(&mut self) {
        if self.restore && unsafe { (self.make)(self.draw, self.read, self.context) } == 0 {
            tracing::error!("could not restore OpenGL read drawable after presentation");
        }
    }
}
