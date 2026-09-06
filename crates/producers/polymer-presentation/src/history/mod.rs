//! Hysteresis for capture layouts. Native resource validation remains with
//! callers and must happen before holding a previous source.

pub struct SelectionHistory<E> {
    pending: Vec<E>,
    consecutive: u32,
    missing: u32,
}

impl<E> Default for SelectionHistory<E> {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            consecutive: 0,
            missing: 0,
        }
    }
}

impl<E: Clone + PartialEq> SelectionHistory<E> {
    pub fn reconcile(
        &mut self,
        current: &[E],
        proposed: Vec<E>,
        exact: bool,
        tracked: bool,
        fallback: Vec<E>,
    ) -> Vec<E> {
        if exact || (!proposed.is_empty() && (current == proposed || !tracked)) {
            self.reset();
            return proposed;
        }
        if !proposed.is_empty() {
            self.missing = 0;
            if self.pending == proposed {
                self.consecutive += 1;
            } else {
                self.pending = proposed;
                self.consecutive = 1;
            }
            if self.consecutive >= 60 {
                self.consecutive = 0;
                return self.pending.clone();
            }
            return current.to_vec();
        }
        self.pending.clear();
        self.consecutive = 0;
        if tracked && self.missing < 120 {
            self.missing += 1;
            return current.to_vec();
        }
        self.reset();
        fallback
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.consecutive = 0;
        self.missing = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::SelectionHistory;
    #[test]
    fn exact_shrink_discards_old_layout_and_uncertain_changes_must_persist() {
        let mut history = SelectionHistory::default();
        for _ in 0..59 {
            assert_eq!(
                history.reconcile(&[1, 2], vec![3], false, true, vec![]),
                [1, 2]
            );
        }
        assert_eq!(
            history.reconcile(&[1, 2], vec![3], false, true, vec![]),
            [3]
        );
        assert_eq!(history.reconcile(&[1, 2], vec![1], true, true, vec![]), [1]);
        for _ in 0..120 {
            assert_eq!(history.reconcile(&[1], vec![], false, true, vec![]), [1]);
        }
        assert!(
            history
                .reconcile(&[1], vec![], false, true, vec![])
                .is_empty()
        );
    }
}

mod resources;
pub use resources::{HistorySurface, Reconciliation, ResourceHistory, SurfaceChange};
