//! Slot ordering, independent of how a backend acquires GPU ownership.

#[derive(Default)]
pub struct SlotScheduler {
    next: usize,
}

/// A backend-owned acquisition. Drop releases uncommitted ownership; finish
/// publishes the native ownership handoff after successful GPU submission.
pub trait SlotLease {
    type Error;
    fn finish(self) -> Result<(), Self::Error>;
}
impl<L: SlotLease> SlotLease for Option<L> {
    type Error = L::Error;
    fn finish(self) -> Result<(), Self::Error> {
        self.map_or(Ok(()), L::finish)
    }
}
pub struct SlotReservation<'a, L: SlotLease> {
    scheduler: &'a mut SlotScheduler,
    lease: L,
    index: usize,
    count: usize,
}

/// An acquisition held across a backend-defined boundary such as BeginPaint /
/// EndPaint. The backend retains exclusive ownership until commit or Drop.
pub struct DetachedSlotReservation<L: SlotLease> {
    lease: L,
    index: usize,
    count: usize,
}
impl<L: SlotLease> DetachedSlotReservation<L> {
    pub fn lease(&self) -> &L {
        &self.lease
    }
    /// Allows native preparation to fail while retaining acquired ownership.
    /// The reservation still commits bookkeeping only through `commit`.
    pub fn lease_mut(&mut self) -> &mut L {
        &mut self.lease
    }
    pub fn commit(self, scheduler: &mut SlotScheduler) -> Result<usize, L::Error> {
        self.lease.finish()?;
        scheduler.commit(self.index, self.count);
        Ok(self.index)
    }
}
impl<L: SlotLease> SlotReservation<'_, L> {
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn detach(self) -> DetachedSlotReservation<L> {
        DetachedSlotReservation {
            lease: self.lease,
            index: self.index,
            count: self.count,
        }
    }
    pub fn commit(self) -> Result<usize, L::Error> {
        self.lease.finish()?;
        self.scheduler.commit(self.index, self.count);
        Ok(self.index)
    }
}
impl SlotScheduler {
    pub fn try_acquire<L: SlotLease>(
        &mut self,
        count: usize,
        mut acquire: impl FnMut(usize) -> Option<L>,
    ) -> Option<SlotReservation<'_, L>> {
        for index in self.candidates(count) {
            if let Some(lease) = acquire(index) {
                return Some(SlotReservation {
                    scheduler: self,
                    lease,
                    index,
                    count,
                });
            }
        }
        None
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

#[cfg(test)]
mod tests {
    use super::SlotScheduler;
    #[test]
    fn failed_attempt_does_not_advance_and_success_rotates_fairly() {
        let mut scheduler = SlotScheduler::default();
        assert_eq!(scheduler.candidates(3).find(|slot| *slot == 1), Some(1));
        assert_eq!(scheduler.candidates(3).next(), Some(0));
        scheduler.commit(1, 3);
        assert_eq!(scheduler.candidates(3).collect::<Vec<_>>(), [2, 0, 1]);
        assert_eq!(scheduler.candidates(0).next(), None);
    }
}

#[cfg(test)]
mod lease_tests {
    use super::*;
    use std::cell::Cell;
    struct Lease<'a> {
        owned: &'a Cell<bool>,
        fail: bool,
    }
    impl Drop for Lease<'_> {
        fn drop(&mut self) {
            self.owned.set(false);
        }
    }
    impl SlotLease for Lease<'_> {
        type Error = ();
        fn finish(self) -> Result<(), ()> {
            if self.fail { Err(()) } else { Ok(()) }
        }
    }
    #[test]
    fn failed_submission_releases_ownership_without_committing_the_ring() {
        let owned = Cell::new(false);
        let mut scheduler = SlotScheduler::default();
        for fail in [true, false] {
            let reservation = scheduler
                .try_acquire(3, |index| {
                    (index == 1).then(|| {
                        owned.set(true);
                        Lease {
                            owned: &owned,
                            fail,
                        }
                    })
                })
                .unwrap()
                .detach();
            assert!(owned.get());
            assert_eq!(scheduler.candidates(3).next(), Some(0));
            assert_eq!(reservation.commit(&mut scheduler).is_err(), fail);
            assert!(!owned.get());
            assert_eq!(
                scheduler.candidates(3).next(),
                Some(if fail { 0 } else { 2 })
            );
        }
        assert!(scheduler.try_acquire(3, |_| None::<Lease<'_>>).is_none());
        let before = scheduler.candidates(3).collect::<Vec<_>>();
        let failed = scheduler
            .try_acquire(3, |_| {
                owned.set(true);
                Some(Lease {
                    owned: &owned,
                    fail: true,
                })
            })
            .unwrap();
        assert!(failed.commit().is_err());
        assert!(!owned.get());
        assert_eq!(scheduler.candidates(3).collect::<Vec<_>>(), before);
        // Abandoning an acquired slot before submission must likewise release
        // ownership without making the next capture skip it.
        let abandoned = scheduler
            .try_acquire(3, |_| {
                owned.set(true);
                Some(Lease {
                    owned: &owned,
                    fail: false,
                })
            })
            .unwrap();
        drop(abandoned);
        assert!(!owned.get());
        assert_eq!(scheduler.candidates(3).collect::<Vec<_>>(), before);
    }
}
