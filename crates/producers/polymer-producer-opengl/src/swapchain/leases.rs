use super::ImageSlot;
use polymer_presentation::scheduling::SlotLease;
use std::sync::atomic::{AtomicU64, Ordering};

impl ImageSlot {
    pub(super) fn try_acquire(
        &self,
        consumer_completed: u64,
        value: u64,
    ) -> Option<ExternalLease<'_>> {
        // Consumer release follows its wait for the producer timeline, so this
        // acknowledgement also proves completion of the previous GL submission.
        (self.release_value.load(Ordering::Relaxed) <= consumer_completed).then_some(
            ExternalLease {
                release: &self.release_value,
                value,
            },
        )
    }
}
pub(super) struct ExternalLease<'a> {
    release: &'a AtomicU64,
    value: u64,
}
impl SlotLease for ExternalLease<'_> {
    type Error = std::convert::Infallible;
    fn finish(self) -> Result<(), Self::Error> {
        self.release.store(self.value, Ordering::Relaxed);
        Ok(())
    }
}
