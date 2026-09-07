use std::sync::atomic::{AtomicUsize, Ordering};

// This static is local to the active producer DLL. Quarantined generations
// keep their permits, so repeated reconnect/allocation cannot grow forever.
static GENERATIONS: AtomicUsize = AtomicUsize::new(0);
const MAX_GENERATIONS: usize = 32;

pub(crate) struct GenerationPermit;
impl GenerationPermit {
    pub fn acquire() -> Option<Self> {
        GENERATIONS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_GENERATIONS).then_some(count + 1)
            })
            .ok()
            .map(|_| Self)
    }
}
impl Drop for GenerationPermit {
    fn drop(&mut self) {
        GENERATIONS.fetch_sub(1, Ordering::AcqRel);
    }
}
