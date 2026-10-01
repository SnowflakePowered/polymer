use parking_lot::{Condvar, Mutex};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

/// Wakes `vkAcquireNextImageKHR` waiters when another thread presents or
/// retires the swapchain, changing which application leases are free.
#[derive(Default)]
pub(super) struct AcquireWakeup {
    epoch: AtomicU64,
    wait: Mutex<()>,
    changed: Condvar,
}

impl AcquireWakeup {
    pub(super) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub(super) fn wait(&self, observed: u64, timeout: Duration) {
        let mut guard = self.wait.lock();
        if self.epoch() == observed {
            self.changed.wait_for(&mut guard, timeout);
        }
    }

    pub(super) fn notify(&self) {
        let _guard = self.wait.lock();
        self.epoch.fetch_add(1, Ordering::Release);
        self.changed.notify_all();
    }
}
