use super::*;
use block2::RcBlock;
use std::{
    ptr::NonNull,
    sync::{Arc, Condvar, Mutex},
};

/// The producer's ready timeline, opened independently of any image import so
/// a consumer can learn of new frames from the shared event alone.
pub struct ProducerTimeline {
    event: Retained<ProtocolObject<dyn MTLSharedEvent>>,
    listener: Retained<MTLSharedEventListener>,
    closed: Arc<(Mutex<bool>, Condvar)>,
}

// SAFETY: MTLSharedEvent and MTLSharedEventListener are thread-safe.
unsafe impl Send for ProducerTimeline {}
unsafe impl Sync for ProducerTimeline {}

impl ProducerTimeline {
    pub fn open(resources: &MetalResources) -> io::Result<Self> {
        let device = MTLCreateSystemDefaultDevice()
            .ok_or_else(|| io::Error::other("Metal device unavailable"))?;
        let event = device
            .newSharedEventWithHandle(&resources.ready)
            .ok_or_else(|| io::Error::other("producer event import failed"))?;
        Ok(Self {
            event,
            listener: MTLSharedEventListener::new(),
            closed: Arc::default(),
        })
    }

    /// Blocks until the timeline passes `value` and returns its newest
    /// signalled value, or `None` once closed.
    pub fn wait_beyond(&self, value: u64) -> Option<u64> {
        let (closed, changed) = &*self.closed;
        let mut guard = closed.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            if *guard {
                return None;
            }
            let completed = self.event.signaledValue();
            if completed > value {
                return Some(completed);
            }
            let state = self.closed.clone();
            let notify = RcBlock::new(
                move |_: NonNull<ProtocolObject<dyn MTLSharedEvent>>, _: u64| {
                    let (lock, changed) = &*state;
                    let _guard = lock.lock();
                    changed.notify_all();
                },
            );
            // The listener's queue takes the lock before notifying, so the
            // notification cannot be lost between this check and the wait.
            unsafe {
                self.event.notifyListener_atValue_block(
                    &self.listener,
                    value + 1,
                    RcBlock::as_ptr(&notify),
                )
            };
            guard = changed
                .wait(guard)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    pub fn close(&self) {
        let (closed, changed) = &*self.closed;
        *closed.lock().unwrap_or_else(|error| error.into_inner()) = true;
        changed.notify_all();
    }
}
