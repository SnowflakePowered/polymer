/// Backend records supply identity and compatible capture requirements, without
/// giving history ownership of native allocation or destruction operations.
pub trait HistorySurface: Clone {
    type Id: Eq;
    type Layout: Clone + Eq;
    fn identity(&self) -> Self::Id;
    fn layout(&self) -> Self::Layout;
    fn selection(&mut self) -> Option<&mut crate::SurfaceSelection> {
        None
    }
}
pub struct Reconciliation<T> {
    pub surfaces: Vec<T>,
    /// Stable logical slot for each corresponding surface. The source vector
    /// contains only live resources; missing slots remain holes in this mapping.
    pub slots: Vec<usize>,
    pub changed_slots: Vec<usize>,
}
impl<T> Reconciliation<T> {
    pub fn registry_sources(
        &self,
        mut describe: impl FnMut(&T) -> (crate::registry::SourceIdentity, crate::Extent2D),
    ) -> Vec<crate::registry::RegistrySource> {
        self.surfaces
            .iter()
            .zip(&self.slots)
            .map(|(source, &logical_slot)| {
                let (identity, extent) = describe(source);
                crate::registry::RegistrySource {
                    logical_slot,
                    identity,
                    extent,
                }
            })
            .collect()
    }

    pub fn changed(&self, index: usize) -> bool {
        self.slots
            .get(index)
            .is_some_and(|slot| self.changed_slots.contains(slot))
    }
}

pub struct ResourceHistory<T: HistorySurface> {
    surfaces: Vec<Option<T>>,
    slots: Vec<T::Layout>,
    pending: Vec<T::Layout>,
    consecutive: u32,
    missing: u32,
    placement_age: Vec<u16>,
}
impl<T: HistorySurface> Default for ResourceHistory<T> {
    fn default() -> Self {
        Self {
            surfaces: Vec::new(),
            slots: Vec::new(),
            pending: Vec::new(),
            consecutive: 0,
            missing: 0,
            placement_age: Vec::new(),
        }
    }
}
impl<T: HistorySurface> ResourceHistory<T> {
    pub fn reset(&mut self) {
        self.surfaces.clear();
        self.slots.clear();
        self.placement_age.clear();
        self.reset_pending();
    }
    pub fn reconcile(
        &mut self,
        mut observed: Vec<T>,
        exact: bool,
        mut revalidate: impl FnMut(&mut T) -> bool,
    ) -> Reconciliation<T> {
        let mut previous = self.surfaces.clone();
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
        let desired =
            self.reconcile_layout(&current, observed.iter().map(T::layout).collect(), exact);
        let target_extent = observed
            .iter_mut()
            .find_map(|source| source.selection().map(|selection| selection.target_extent));
        let observed_placements = observed
            .iter_mut()
            .filter_map(|source| {
                source
                    .selection()
                    .is_some_and(|selection| selection.presentation_rect.is_some())
                    .then(|| source.identity())
            })
            .collect::<Vec<_>>();
        let mut selected = self.select_resources(observed, desired, exact);
        let slots = self.assign_slots(&selected);
        self.retain_placements(
            &mut selected,
            &slots,
            &observed_placements,
            &mut previous,
            target_extent,
        );
        let mut next = vec![None; self.slots.len()];
        for (source, &slot) in selected.iter().zip(&slots) {
            next[slot] = Some(source.clone());
        }
        let changed_slots = slots
            .iter()
            .copied()
            .filter(|&slot| {
                let new = next[slot].as_ref().unwrap();
                !previous
                    .get(slot)
                    .and_then(Option::as_ref)
                    .is_some_and(|old| {
                        old.identity() == new.identity() && old.layout() == new.layout()
                    })
            })
            .collect();
        self.surfaces = next;
        Reconciliation {
            surfaces: selected,
            slots,
            changed_slots,
        }
    }

    fn retain_placements(
        &mut self,
        selected: &mut [T],
        slots: &[usize],
        observed_placements: &[T::Id],
        previous: &mut [Option<T>],
        target_extent: Option<crate::Extent2D>,
    ) {
        self.placement_age.resize(self.slots.len(), 0);
        for (source, &slot) in selected.iter_mut().zip(slots) {
            if let (Some(selection), Some(extent)) = (source.selection(), target_extent) {
                selection.target_extent = extent;
            }
            if observed_placements.contains(&source.identity()) {
                self.placement_age[slot] = 0;
                continue;
            }
            self.placement_age[slot] = self.placement_age[slot].saturating_add(1);
            let old = previous
                .get_mut(slot)
                .and_then(Option::as_mut)
                .filter(|old| old.layout() == source.layout())
                .and_then(T::selection)
                .copied();
            if let Some(selection) = source.selection() {
                selection.presentation_rect = None;
                if let Some(old) = old.filter(|old| {
                    old.target_extent == selection.target_extent && self.placement_age[slot] <= 240
                }) {
                    selection.presentation_rect = old.presentation_rect;
                    selection.flip_x = old.flip_x;
                    selection.flip_y = old.flip_y;
                }
            }
        }
    }

    fn reconcile_layout(
        &mut self,
        current: &[T::Layout],
        proposed: Vec<T::Layout>,
        exact: bool,
    ) -> Vec<T::Layout> {
        if exact || (!proposed.is_empty() && (current == proposed || current.is_empty())) {
            self.reset_pending();
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
        if !current.is_empty() && self.missing < 120 {
            self.missing += 1;
            return current.to_vec();
        }
        self.reset_pending();
        Vec::new()
    }

    fn reset_pending(&mut self) {
        self.pending.clear();
        self.consecutive = 0;
        self.missing = 0;
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
    fn uncertain_layout_changes_and_missing_sources_must_persist() {
        let mut history = ResourceHistory::default();
        history.reconcile(vec![Source(1, 400, true)], true, |v| v.2);
        for _ in 0..59 {
            assert_eq!(
                history
                    .reconcile(vec![Source(2, 320, true)], false, |v| v.2)
                    .surfaces[0]
                    .0,
                1
            );
        }
        assert_eq!(
            history
                .reconcile(vec![Source(2, 320, true)], false, |v| v.2)
                .surfaces[0]
                .0,
            2
        );
        for _ in 0..120 {
            assert_eq!(history.reconcile(vec![], false, |v| v.2).surfaces[0].0, 2);
        }
        assert!(
            history
                .reconcile(vec![], false, |v| v.2)
                .surfaces
                .is_empty()
        );
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
        assert_eq!(flipped.changed_slots, [0]);
        let invalid = history.reconcile(vec![], false, |v| v.0 != 3);
        assert_eq!(
            invalid.surfaces.iter().map(|v| v.0).collect::<Vec<_>>(),
            [2]
        );
        assert_eq!(invalid.slots, [1]);
        assert!(invalid.changed_slots.is_empty());
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
