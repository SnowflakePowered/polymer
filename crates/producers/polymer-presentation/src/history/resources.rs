use super::SelectionHistory;

/// Backend records supply identity and compatible capture requirements, without
/// giving history ownership of native allocation or destruction operations.
pub trait HistorySurface: Clone {
    type Id: Eq;
    type Layout: Clone + Eq;
    fn identity(&self) -> Self::Id;
    fn layout(&self) -> Self::Layout;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceChange {
    Retain { slot: usize },
    Replace { slot: usize },
    Add { slot: usize },
    Remove { slot: usize },
}
pub struct Reconciliation<T> {
    pub surfaces: Vec<T>,
    /// Stable logical slot for each corresponding surface. The source vector
    /// contains only live resources; missing slots remain holes in this mapping.
    pub slots: Vec<usize>,
    pub changes: Vec<SurfaceChange>,
}
pub struct ResourceHistory<T: HistorySurface> {
    surfaces: Vec<Option<T>>,
    slots: Vec<T::Layout>,
    layout: SelectionHistory<T::Layout>,
}
impl<T: HistorySurface> Default for ResourceHistory<T> {
    fn default() -> Self {
        Self {
            surfaces: Vec::new(),
            slots: Vec::new(),
            layout: SelectionHistory::default(),
        }
    }
}
impl<T: HistorySurface> ResourceHistory<T> {
    pub fn reset(&mut self) {
        self.surfaces.clear();
        self.slots.clear();
        self.layout.reset();
    }
    pub fn reconcile(
        &mut self,
        mut observed: Vec<T>,
        exact: bool,
        mut revalidate: impl FnMut(&mut T) -> bool,
    ) -> Reconciliation<T> {
        let previous = self.surfaces.clone();
        for source in &mut self.surfaces {
            if source.as_mut().is_some_and(|source| !revalidate(source)) {
                *source = None;
            }
        }
        observed.retain_mut(&mut revalidate);
        let current = self
            .surfaces
            .iter()
            .flatten()
            .map(T::layout)
            .collect::<Vec<_>>();
        let desired = self.layout.reconcile(
            &current,
            observed.iter().map(T::layout).collect(),
            exact,
            !current.is_empty(),
            Vec::new(),
        );
        let selected = self.select_resources(observed, desired, exact);
        let slots = self.assign_slots(&selected);
        let mut next = vec![None; self.slots.len()];
        for (source, &slot) in selected.iter().zip(&slots) {
            next[slot] = Some(source.clone());
        }
        let changes = (0..next.len().max(previous.len()))
            .filter_map(|slot| {
                let old = previous.get(slot).and_then(Option::as_ref);
                let new = next.get(slot).and_then(Option::as_ref);
                match (old, new) {
                    (Some(old), Some(new))
                        if old.identity() == new.identity() && old.layout() == new.layout() =>
                    {
                        Some(SurfaceChange::Retain { slot })
                    }
                    (Some(_), Some(_)) => Some(SurfaceChange::Replace { slot }),
                    (None, Some(_)) => Some(SurfaceChange::Add { slot }),
                    (Some(_), None) => Some(SurfaceChange::Remove { slot }),
                    (None, None) => None,
                }
            })
            .collect();
        self.surfaces = next;
        Reconciliation {
            surfaces: selected,
            slots,
            changes,
        }
    }

    fn select_resources(&self, observed: Vec<T>, desired: Vec<T::Layout>, exact: bool) -> Vec<T> {
        let mut remaining = desired;
        let mut selected = Vec::new();
        // Reserve every observed identity before considering held resources;
        // equal-size siblings must never both resolve to one observed resource.
        for source in observed {
            if selected
                .iter()
                .any(|old: &T| old.identity() == source.identity())
            {
                continue;
            }
            if let Some(index) = remaining
                .iter()
                .position(|layout| *layout == source.layout())
            {
                remaining.remove(index);
                selected.push(source);
            }
        }
        if exact {
            return selected;
        }
        for source in self.surfaces.iter().flatten() {
            if selected
                .iter()
                .any(|old| old.identity() == source.identity())
            {
                continue;
            }
            if let Some(index) = remaining
                .iter()
                .position(|layout| *layout == source.layout())
            {
                remaining.remove(index);
                selected.push(source.clone());
            }
        }
        selected
    }

    fn assign_slots(&mut self, selected: &[T]) -> Vec<usize> {
        let mut assigned = selected
            .iter()
            .map(|source| {
                self.surfaces.iter().position(|old| {
                    old.as_ref()
                        .is_some_and(|old| old.identity() == source.identity())
                })
            })
            .collect::<Vec<_>>();
        let mut occupied = assigned
            .iter()
            .flatten()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        for (source, assigned) in selected.iter().zip(&mut assigned) {
            if assigned.is_some() {
                continue;
            }
            let layout = source.layout();
            let slot = self
                .surfaces
                .iter()
                .enumerate()
                .position(|(slot, old)| {
                    !occupied.contains(&slot)
                        && old.as_ref().is_some_and(|old| old.layout() == layout)
                })
                .or_else(|| {
                    self.slots
                        .iter()
                        .enumerate()
                        .position(|(slot, old)| !occupied.contains(&slot) && *old == layout)
                })
                .or_else(|| (0..self.slots.len()).find(|slot| !occupied.contains(slot)))
                .unwrap_or(self.slots.len());
            if slot == self.slots.len() {
                self.slots.push(layout);
            }
            *assigned = Some(slot);
            occupied.insert(slot);
        }
        let assigned = assigned.into_iter().map(Option::unwrap).collect::<Vec<_>>();
        for (source, &slot) in selected.iter().zip(&assigned) {
            self.slots[slot] = source.layout();
        }
        assigned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone)]
    struct Source(u32, u32, bool);
    impl HistorySurface for Source {
        type Id = u32;
        type Layout = u32;
        fn identity(&self) -> u32 {
            self.0
        }
        fn layout(&self) -> u32 {
            self.1
        }
    }
    #[test]
    fn page_flips_keep_slots_invalid_sources_leave_immediately_and_exact_shrink_is_complete() {
        let mut history = ResourceHistory::default();
        history.reconcile(
            vec![Source(1, 400, true), Source(2, 320, true)],
            true,
            |v| v.2,
        );
        let flipped = history.reconcile(vec![Source(3, 400, true)], false, |v| v.2);
        assert_eq!(
            flipped.surfaces.iter().map(|v| v.0).collect::<Vec<_>>(),
            [3, 2]
        );
        assert_eq!(flipped.changes[0], SurfaceChange::Replace { slot: 0 });
        let invalid = history.reconcile(vec![], false, |v| v.0 != 3);
        assert_eq!(
            invalid.surfaces.iter().map(|v| v.0).collect::<Vec<_>>(),
            [2]
        );
        assert_eq!(invalid.slots, [1]);
        assert!(invalid.changes.contains(&SurfaceChange::Remove { slot: 0 }));
        assert!(invalid.changes.contains(&SurfaceChange::Retain { slot: 1 }));
        let shrunk = history.reconcile(vec![Source(5, 240, true)], true, |v| v.2);
        assert_eq!(shrunk.surfaces.iter().map(|v| v.0).collect::<Vec<_>>(), [5]);
    }
    #[test]
    fn equal_size_reorder_and_page_flips_reserve_surviving_identities_before_slots() {
        let mut history = ResourceHistory::default();
        history.reconcile(
            vec![Source(1, 400, true), Source(2, 400, true)],
            true,
            |v| v.2,
        );
        let changed = history.reconcile(
            vec![Source(3, 400, true), Source(1, 400, true)],
            true,
            |v| v.2,
        );
        assert_eq!(changed.slots, [1, 0]);
        let removed = history.reconcile(vec![Source(3, 400, true)], true, |v| v.2);
        assert_eq!(removed.slots, [1]);
        let resized = history.reconcile(vec![Source(3, 500, true)], true, |v| v.2);
        assert_eq!(resized.slots, [1]);
        let added = history.reconcile(
            vec![Source(4, 400, true), Source(3, 500, true)],
            true,
            |v| v.2,
        );
        assert_eq!(added.slots, [0, 1]);
    }
}
