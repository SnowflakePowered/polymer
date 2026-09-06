use super::WindowCaptures;
use parking_lot::Mutex;
use polymer_presentation::registry::PresentationRegistry;

#[derive(Default)]
pub(super) struct CaptureStore {
    pub(super) entries: dashmap::DashMap<(usize, usize), Mutex<WindowCaptures>>,
    pub(super) recovery: dashmap::DashMap<usize, super::recovery::RecoveryState>,
}

impl CaptureStore {
    /// Called only after successful native deletion. Failed deletions leave
    /// entries available for retirement on a later context activation.
    pub(super) fn context_destroyed(&self, context: usize, registry: &PresentationRegistry) {
        self.entries.retain(|&(_, capture_context), stored| {
            if capture_context != context {
                return true;
            }
            let captures = stored.get_mut();
            registry.forget_owner(captures.owner_id);
            for stream in std::mem::take(&mut captures.streams) {
                stream.context_destroyed();
            }
            false
        });
        self.recovery.remove(&context);
    }

    /// The observed context must be current on the presenting thread.
    pub(super) unsafe fn prepare_context(&self, context: usize, dc: usize) {
        let mut recovery = self.recovery.entry(context).or_default();
        unsafe { recovery.prepare(dc as crate::interop::Hdc) };
    }
    /// The supplied context must be current on this thread, with presentation
    /// transactions serialized, until all native entries finish destruction.
    pub(super) unsafe fn retire_context(&self, context: usize, registry: &PresentationRegistry) {
        self.entries.retain(|&(_, capture_context), stored| {
            if capture_context != context {
                return true;
            }
            registry.forget_owner(stored.get_mut().owner_id);
            false
        });
    }

    /// The supplied GL context must be current on this thread. Presentation
    /// transactions must be serialized while native entries are retired.
    pub(super) unsafe fn retire_idle(
        &self,
        context: usize,
        now: u64,
        registry: &PresentationRegistry,
    ) {
        self.entries.retain(|&(_, capture_context), stored| {
            let captures = stored.get_mut();
            let retire = capture_context == context
                && now.saturating_sub(captures.last_swap) > PresentationRegistry::IDLE_HORIZON;
            if retire {
                registry.forget_owner(captures.owner_id);
            }
            !retire
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_retirement_waits_for_owning_context_and_preserves_recent_siblings() {
        let store = CaptureStore::default();
        let registry = PresentationRegistry::default();
        for (key, seen) in [((1, 10), 1), ((2, 10), 500), ((3, 20), 1)] {
            store.entries.insert(
                key,
                Mutex::new(WindowCaptures {
                    last_swap: seen,
                    ..Default::default()
                }),
            );
        }
        // Empty native streams make these context transitions computational.
        unsafe { store.retire_idle(10, 601, &registry) };
        assert_eq!(store.entries.len(), 3);
        unsafe { store.retire_idle(10, 602, &registry) };
        assert!(!store.entries.contains_key(&(1, 10)));
        assert!(store.entries.contains_key(&(2, 10)));
        unsafe { store.retire_context(10, &registry) };
        assert!(!store.entries.contains_key(&(2, 10)));
        assert!(store.entries.contains_key(&(3, 20)));
        unsafe { store.retire_idle(20, 603, &registry) };
        assert!(!store.entries.contains_key(&(3, 20)));
        assert!(store.entries.is_empty());
    }
}
