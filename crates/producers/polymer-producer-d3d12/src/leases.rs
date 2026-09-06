use std::sync::atomic::{AtomicU64, Ordering};
#[derive(Default)]
pub(super) struct SlotState {
    submitted: AtomicU64,
    published: AtomicU64,
}
impl SlotState {
    pub fn available(&self, producer: u64, consumer: u64) -> bool {
        producer >= self.submitted.load(Ordering::Relaxed)
            && consumer >= self.published.load(Ordering::Relaxed)
    }
    pub fn submitted(&self, value: u64) {
        self.submitted.store(value, Ordering::Relaxed);
    }
    pub fn published(&self, value: u64) {
        self.published.store(value, Ordering::Relaxed);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_signal_requires_producer_completion_but_no_unpublished_consumer_ack() {
        let slot = SlotState::default();
        assert!(slot.available(0, 0));
        slot.submitted(1);
        assert!(!slot.available(0, 0));
        assert!(slot.available(1, 0));
        slot.submitted(2);
        slot.published(2);
        assert!(!slot.available(1, 2));
        assert!(!slot.available(2, 0));
        assert!(slot.available(2, 2));
    }
}
