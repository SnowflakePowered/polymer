use block2::RcBlock;
use objc2::runtime::ProtocolObject;
use objc2_metal::*;
use objc2_quartz_core::CAMetalDrawable;
use parking_lot::Mutex;
use std::{
    io,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
};

#[derive(Default)]
pub(super) struct Submissions {
    active: AtomicUsize,
    gpu: AtomicUsize,
    error: Mutex<Option<String>>,
    pub wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
struct Completion {
    owner: Arc<Submissions>,
    finished: AtomicU8,
}
impl Completion {
    fn finish(&self, flag: u8) {
        let previous = self.finished.fetch_or(flag, Ordering::AcqRel);
        if previous != 3 && (previous | flag) == 3 {
            self.owner.active.fetch_sub(1, Ordering::Release);
            let wake = self.owner.wake.lock().clone();
            if let Some(wake) = wake {
                wake();
            }
        }
    }
}
impl Submissions {
    pub fn gpu_pending(&self) -> bool {
        self.gpu.load(Ordering::Acquire) != 0
    }
    pub fn can_submit(&self) -> io::Result<bool> {
        if let Some(error) = &*self.error.lock() {
            return Err(io::Error::other(error.clone()));
        }
        Ok(self.gpu.load(Ordering::Acquire) < 2)
    }
    pub fn available(&self) -> io::Result<bool> {
        if let Some(error) = &*self.error.lock() {
            return Err(io::Error::other(error.clone()));
        }
        // Leave a spare drawable in CAMetalLayer's three-buffer pool. A slot is
        // returned only after both GPU completion and compositor presentation.
        Ok(self.active.load(Ordering::Acquire) < 2)
    }
    pub fn track(
        self: &Arc<Self>,
        commands: &ProtocolObject<dyn MTLCommandBuffer>,
        drawable: Option<&ProtocolObject<dyn CAMetalDrawable>>,
    ) {
        self.active.fetch_add(1, Ordering::AcqRel);
        self.gpu.fetch_add(1, Ordering::AcqRel);
        let completion = Arc::new(Completion {
            owner: self.clone(),
            finished: AtomicU8::new(if drawable.is_some() { 0 } else { 2 }),
        });
        let presented = completion.clone();
        let display =
            RcBlock::new(move |_: NonNull<ProtocolObject<dyn MTLDrawable>>| presented.finish(2));
        let done = RcBlock::new(
            move |buffer: NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
                let buffer = unsafe { buffer.as_ref() };
                if buffer.status() == MTLCommandBufferStatus::Error {
                    *completion.owner.error.lock() =
                        Some(format!("Metal submission failed: {:?}", buffer.error()));
                    completion.owner.gpu.fetch_sub(1, Ordering::Release);
                    completion.finish(3);
                    return;
                }
                completion.owner.gpu.fetch_sub(1, Ordering::Release);
                completion.finish(1);
                let wake = completion.owner.wake.lock().clone();
                if let Some(wake) = wake {
                    wake();
                }
            },
        );
        unsafe {
            if let Some(drawable) = drawable {
                drawable.addPresentedHandler(RcBlock::as_ptr(&display));
            }
            commands.addCompletedHandler(RcBlock::as_ptr(&done));
        }
    }
}
