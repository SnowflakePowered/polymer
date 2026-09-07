//! Slot ordering, independent of how a backend acquires GPU ownership.

#[derive(Default)]
pub struct SlotScheduler {
    next: usize,
}

impl SlotScheduler {
    pub fn try_acquire<L>(
        &self,
        count: usize,
        mut acquire: impl FnMut(usize) -> Option<L>,
    ) -> Option<(usize, L)> {
        self.candidates(count)
            .find_map(|index| acquire(index).map(|lease| (index, lease)))
    }

    /// Probe each slot once, without changing state on a busy/failed capture.
    pub fn candidates(&self, count: usize) -> impl Iterator<Item = usize> + use<> {
        let next = self.next;
        (0..count).map(move |offset| (next + offset) % count)
    }

    /// Call only after submission and its ownership handoff both succeed.
    pub fn commit(&mut self, slot: usize, count: usize) {
        self.next = if count == 0 { 0 } else { (slot + 1) % count };
    }
}
