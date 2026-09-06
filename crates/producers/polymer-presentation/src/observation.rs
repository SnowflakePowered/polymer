//! Prevent producer-owned GPU operations from feeding application observations.
use std::{cell::Cell, marker::PhantomData, rc::Rc};

thread_local! { static CAPTURING: Cell<u32> = const { Cell::new(0) }; }

pub struct CaptureScope(PhantomData<Rc<()>>);

impl CaptureScope {
    pub fn enter() -> Self {
        CAPTURING.with(|depth| depth.set(depth.get() + 1));
        Self(PhantomData)
    }
    pub fn active() -> bool {
        CAPTURING.with(|depth| depth.get() != 0)
    }
}

impl Drop for CaptureScope {
    fn drop(&mut self) {
        CAPTURING.with(|depth| depth.set(depth.get() - 1));
    }
}
