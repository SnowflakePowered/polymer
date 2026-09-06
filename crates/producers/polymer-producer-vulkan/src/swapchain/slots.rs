use ash::vk;
use polymer_presentation::scheduling::SlotLease;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Host bookkeeping is separate from the native fence's signaled state: a
/// failed submit can leave a reset fence with no operation that will signal it.
#[derive(Default)]
pub(super) struct SlotState {
    pending: AtomicBool,
    initialized: AtomicBool,
    release_value: AtomicU64,
}
impl SlotState {
    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Relaxed)
    }
    pub fn initialized(&self) -> bool {
        self.initialized.load(Ordering::Relaxed)
    }
    pub fn try_acquire(
        &self,
        producer_completed: bool,
        consumer_completed: Option<u64>,
        value: u64,
    ) -> Option<Lease<'_>> {
        if !producer_completed
            || consumer_completed.unwrap_or(0) < self.release_value.load(Ordering::Relaxed)
        {
            return None;
        }
        self.pending.store(false, Ordering::Relaxed);
        Some(Lease { state: self, value })
    }
    pub fn submitted(&self, result: Result<(), vk::Result>) -> Result<(), vk::Result> {
        // OOM leaves submitted resources and synchronization untouched. Device
        // loss counts as pending for resource lifetime purposes (Vulkan spec).
        self.pending.store(
            !matches!(
                result,
                Err(vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)
            ),
            Ordering::Relaxed,
        );
        result
    }
}

pub(super) struct Lease<'a> {
    state: &'a SlotState,
    value: u64,
}
impl SlotLease for Lease<'_> {
    type Error = std::convert::Infallible;
    fn finish(self) -> Result<(), Self::Error> {
        self.state.initialized.store(true, Ordering::Relaxed);
        self.state
            .release_value
            .store(self.value, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polymer_presentation::scheduling::SlotScheduler;
    #[test]
    fn failed_submit_is_retryable_and_retirement_tracks_only_pending_work() {
        let slots = [SlotState::default(), SlotState::default()];
        let mut scheduler = SlotScheduler::default();
        let first = scheduler
            .try_acquire(2, |index| slots[index].try_acquire(true, Some(0), 1))
            .unwrap();
        assert!(
            slots[0]
                .submitted(Err(vk::Result::ERROR_OUT_OF_HOST_MEMORY))
                .is_err()
        );
        drop(first);
        assert!(!slots[0].pending());
        assert!(!slots[0].initialized());
        let retry = scheduler
            .try_acquire(2, |index| slots[index].try_acquire(true, Some(0), 1))
            .unwrap();
        assert_eq!(retry.index(), 0);
        slots[0].submitted(Ok(())).unwrap();
        retry.commit().unwrap();
        assert!(slots[0].pending());
        assert!(slots[0].try_acquire(true, Some(0), 2).is_none());
        assert!(slots[0].try_acquire(true, None, 2).is_none());
        assert!(slots[0].try_acquire(false, Some(1), 2).is_none());
        let next = scheduler
            .try_acquire(2, |index| slots[index].try_acquire(true, Some(0), 2))
            .unwrap();
        assert_eq!(next.index(), 1);
        drop(next); // Recording failure: no submission and no ring advancement.
        assert!(!slots[1].pending());
        assert!(slots[0].try_acquire(true, Some(1), 2).is_some());
        assert!(!slots[0].pending());
        assert!(
            slots[0]
                .submitted(Err(vk::Result::ERROR_DEVICE_LOST))
                .is_err()
        );
        assert!(slots[0].pending());
    }
}
