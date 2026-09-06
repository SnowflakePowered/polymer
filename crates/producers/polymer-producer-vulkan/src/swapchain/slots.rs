use ash::vk;
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
    ) -> Option<()> {
        if !producer_completed
            || consumer_completed.unwrap_or(0) < self.release_value.load(Ordering::Relaxed)
        {
            return None;
        }
        self.pending.store(false, Ordering::Relaxed);
        Some(())
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
impl SlotState {
    pub fn published(&self, value: u64) {
        self.initialized.store(true, Ordering::Relaxed);
        self.release_value.store(value, Ordering::Relaxed);
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
            .try_acquire(2, |index| slots[index].try_acquire(true, Some(0)))
            .unwrap();
        assert!(
            slots[0]
                .submitted(Err(vk::Result::ERROR_OUT_OF_HOST_MEMORY))
                .is_err()
        );
        let _ = first;
        assert!(!slots[0].pending());
        assert!(!slots[0].initialized());
        let retry = scheduler
            .try_acquire(2, |index| slots[index].try_acquire(true, Some(0)))
            .unwrap();
        assert_eq!(retry.0, 0);
        slots[0].submitted(Ok(())).unwrap();
        slots[retry.0].published(1);
        scheduler.commit(retry.0, 2);
        assert!(slots[0].pending());
        assert!(slots[0].try_acquire(true, Some(0)).is_none());
        assert!(slots[0].try_acquire(true, None).is_none());
        assert!(slots[0].try_acquire(false, Some(1)).is_none());
        let next = scheduler
            .try_acquire(2, |index| slots[index].try_acquire(true, Some(0)))
            .unwrap();
        assert_eq!(next.0, 1);
        let _ = next; // Recording failure: no submission and no ring advancement.
        assert!(!slots[1].pending());
        assert!(slots[0].try_acquire(true, Some(1)).is_some());
        assert!(!slots[0].pending());
        assert!(
            slots[0]
                .submitted(Err(vk::Result::ERROR_DEVICE_LOST))
                .is_err()
        );
        assert!(slots[0].pending());
    }
}
