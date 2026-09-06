use polymer_presentation::scheduling::SlotLease;
use std::sync::atomic::{AtomicU64, Ordering};
#[derive(Default)]
pub(super) struct SlotState {
    submitted: AtomicU64,
    published: AtomicU64,
}
impl SlotState {
    pub fn try_acquire(&self, producer: u64, consumer: u64, value: u64) -> Option<Lease<'_>> {
        (producer >= self.submitted.load(Ordering::Relaxed)
            && consumer >= self.published.load(Ordering::Relaxed))
        .then_some(Lease { state: self, value })
    }
    pub fn submitted(&self, value: u64) {
        self.submitted.store(value, Ordering::Relaxed);
    }
}
pub(super) struct Lease<'a> {
    state: &'a SlotState,
    value: u64,
}
impl SlotLease for Lease<'_> {
    type Error = std::convert::Infallible;
    fn finish(self) -> Result<(), Self::Error> {
        self.state.published.store(self.value, Ordering::Relaxed);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_signal_requires_producer_completion_but_no_unpublished_consumer_ack() {
        let slot = SlotState::default();
        {
            let _lease = slot.try_acquire(0, 0, 1).unwrap();
            slot.submitted(1);
        }
        assert!(slot.try_acquire(0, 0, 2).is_none());
        let retry = slot.try_acquire(1, 0, 2).unwrap();
        slot.submitted(2);
        retry.finish().unwrap();
        assert!(slot.try_acquire(1, 2, 3).is_none());
        assert!(slot.try_acquire(2, 0, 3).is_none());
        assert!(slot.try_acquire(2, 2, 3).is_some());
    }
}
