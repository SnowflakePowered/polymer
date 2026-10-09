use std::sync::mpsc::{self, SyncSender};
use std::time::Duration;

use super::{FinalizationReason, ResourceLease};
use crate::VfsResult;

pub(super) struct FinalizationRetry {
    sender: SyncSender<Box<dyn ResourceLease>>,
}

impl FinalizationRetry {
    pub(super) fn reserve() -> VfsResult<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Box<dyn ResourceLease>>(1);
        std::thread::Builder::new()
            .name("polymer-storage-finalization".to_owned())
            .spawn(move || {
                if let Ok(lease) = receiver.recv() {
                    Self::run(lease);
                }
            })?;
        Ok(Self { sender })
    }

    pub(super) fn retain(self, lease: impl ResourceLease + 'static) {
        if let Err(error) = self.sender.send(Box::new(lease)) {
            tracing::error!("storage finalization worker stopped; resources remain retained");
            std::mem::forget(error.0);
        }
    }

    fn run(lease: Box<dyn ResourceLease>) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            while lease.finalize(FinalizationReason::Cancelled).is_err() {
                std::thread::sleep(Duration::from_secs(2));
            }
        }));
        if result.is_err() {
            tracing::error!("storage finalization panicked; resources remain retained");
            std::mem::forget(lease);
        }
    }
}
