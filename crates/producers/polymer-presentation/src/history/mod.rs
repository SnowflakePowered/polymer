/// Backend records supply identity and compatible capture requirements, without
/// giving history ownership of native allocation or destruction operations.
pub trait HistorySurface: Clone {
    type Id: Eq;
    type Layout: Clone + Eq;
    fn identity(&self) -> Self::Id;
    fn layout(&self) -> Self::Layout;
    fn selection(&mut self) -> &mut crate::SurfaceSelection;
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

struct Slot<T: HistorySurface> {
    surface: Option<T>,
    layout: T::Layout,
    placement_age: u16,
}

pub struct ResourceHistory<T: HistorySurface> {
    slots: Vec<Slot<T>>,
    pending: Vec<T::Layout>,
    consecutive: u32,
    missing: u32,
}
impl<T: HistorySurface> Default for ResourceHistory<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            pending: Vec::new(),
            consecutive: 0,
            missing: 0,
        }
    }
}
impl<T: HistorySurface> ResourceHistory<T> {
    pub fn reset(&mut self) {
        self.slots.clear();
        self.reset_pending();
    }
    pub fn reconcile(
        &mut self,
        mut observed: Vec<T>,
        exact: bool,
        mut revalidate: impl FnMut(&mut T) -> bool,
    ) -> Reconciliation<T> {
        let previous = self
            .slots
            .iter_mut()
            .map(|slot| {
                slot.surface
                    .as_mut()
                    .map(|source| (source.identity(), source.layout(), *source.selection()))
            })
            .collect::<Vec<_>>();
        for slot in &mut self.slots {
            let source = &mut slot.surface;
            if source.as_mut().is_some_and(|source| !revalidate(source)) {
                *source = None;
            }
        }
        observed.retain_mut(&mut revalidate);
        let current = self
            .slots
            .iter()
            .filter_map(|slot| slot.surface.as_ref())
            .map(T::layout)
            .collect::<Vec<_>>();
        let desired =
            self.reconcile_layout(&current, observed.iter().map(T::layout).collect(), exact);
        let target_extent = observed
            .first_mut()
            .map(|source| source.selection().target_extent);
        let observed_placements = observed
            .iter_mut()
            .filter_map(|source| {
                source
                    .selection()
                    .presentation_rect
                    .is_some()
                    .then(|| source.identity())
            })
            .collect::<Vec<_>>();
        let mut selected = self.select_resources(observed, desired, exact);
        let slots = self.assign_slots(&selected);
        self.retain_placements(
            &mut selected,
            &slots,
            &observed_placements,
            &previous,
            target_extent,
        );
        for slot in &mut self.slots {
            slot.surface = None;
        }
        let mut changed_slots = Vec::new();
        for (source, &slot) in selected.iter().zip(&slots) {
            if !previous
                .get(slot)
                .and_then(Option::as_ref)
                .is_some_and(|(id, layout, _)| {
                    *id == source.identity() && *layout == source.layout()
                })
            {
                changed_slots.push(slot);
            }
            self.slots[slot].surface = Some(source.clone());
        }
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
        previous: &[Option<(T::Id, T::Layout, crate::SurfaceSelection)>],
        target_extent: Option<crate::Extent2D>,
    ) {
        for (source, &slot) in selected.iter_mut().zip(slots) {
            if let Some(extent) = target_extent {
                source.selection().target_extent = extent;
            }
            if observed_placements.contains(&source.identity()) {
                self.slots[slot].placement_age = 0;
                continue;
            }
            self.slots[slot].placement_age = self.slots[slot].placement_age.saturating_add(1);
            let old = previous
                .get(slot)
                .and_then(Option::as_ref)
                .filter(|(_, layout, _)| *layout == source.layout())
                .map(|(_, _, selection)| *selection);
            let selection = source.selection();
            selection.presentation_rect = None;
            if let Some(old) = old.filter(|old| {
                old.target_extent == selection.target_extent
                    && self.slots[slot].placement_age <= 240
            }) {
                selection.presentation_rect = old.presentation_rect;
                selection.flip_x = old.flip_x;
                selection.flip_y = old.flip_y;
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
        // Reserve observed identities before held sources, cloning only a held
        // resource that actually fills a missing compatible layout.
        let held = self
            .slots
            .iter()
            .filter(|_| !exact)
            .filter_map(|slot| slot.surface.as_ref())
            .map(std::borrow::Cow::Borrowed);
        for source in observed
            .into_iter()
            .map(std::borrow::Cow::Owned)
            .chain(held)
        {
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
                selected.push(source.into_owned());
            }
        }
        selected
    }

    fn assign_slots(&mut self, selected: &[T]) -> Vec<usize> {
        let mut assigned = selected
            .iter()
            .map(|source| {
                self.slots.iter().position(|old| {
                    old.surface
                        .as_ref()
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
                .slots
                .iter()
                .enumerate()
                .filter(|(slot, _)| !occupied.contains(slot))
                .min_by_key(|(slot, old)| {
                    let priority = if old
                        .surface
                        .as_ref()
                        .is_some_and(|source| source.layout() == layout)
                    {
                        0
                    } else if old.layout == layout {
                        1
                    } else {
                        2
                    };
                    (priority, *slot)
                })
                .map_or(self.slots.len(), |(slot, _)| slot);
            if slot == self.slots.len() {
                self.slots.push(Slot {
                    surface: None,
                    layout,
                    placement_age: 0,
                });
            }
            *assigned = Some(slot);
            occupied.insert(slot);
        }
        let assigned = assigned.into_iter().map(Option::unwrap).collect::<Vec<_>>();
        for (source, &slot) in selected.iter().zip(&assigned) {
            self.slots[slot].layout = source.layout();
        }
        assigned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::CpuSurface;
    #[test]
    fn uncertain_layout_changes_and_missing_sources_must_persist() {
        let mut history = ResourceHistory::default();
        history.reconcile(vec![CpuSurface::new(1, 400, 240)], true, |_| true);
        for _ in 0..59 {
            assert_eq!(
                history
                    .reconcile(vec![CpuSurface::new(2, 320, 240)], false, |_| true)
                    .surfaces[0]
                    .id,
                1
            );
        }
        assert_eq!(
            history
                .reconcile(vec![CpuSurface::new(2, 320, 240)], false, |_| true)
                .surfaces[0]
                .id,
            2
        );
        for _ in 0..120 {
            assert_eq!(history.reconcile(vec![], false, |_| true).surfaces[0].id, 2);
        }
        assert!(
            history
                .reconcile(vec![], false, |_| true)
                .surfaces
                .is_empty()
        );
    }
    #[test]
    fn page_flips_keep_slots_invalid_sources_leave_immediately_and_exact_shrink_is_complete() {
        let mut history = ResourceHistory::default();
        history.reconcile(
            vec![CpuSurface::new(1, 400, 240), CpuSurface::new(2, 320, 240)],
            true,
            |_| true,
        );
        let flipped = history.reconcile(vec![CpuSurface::new(3, 400, 240)], false, |_| true);
        assert_eq!(
            flipped.surfaces.iter().map(|v| v.id).collect::<Vec<_>>(),
            [3, 2]
        );
        assert_eq!(flipped.changed_slots, [0]);
        let invalid = history.reconcile(vec![], false, |v| v.id != 3);
        assert_eq!(
            invalid.surfaces.iter().map(|v| v.id).collect::<Vec<_>>(),
            [2]
        );
        assert_eq!(invalid.slots, [1]);
        assert!(invalid.changed_slots.is_empty());
        let shrunk = history.reconcile(vec![CpuSurface::new(5, 240, 240)], true, |_| true);
        assert_eq!(
            shrunk.surfaces.iter().map(|v| v.id).collect::<Vec<_>>(),
            [5]
        );
    }
    #[test]
    fn equal_size_reorder_and_page_flips_reserve_surviving_identities_before_slots() {
        let mut history = ResourceHistory::default();
        history.reconcile(
            vec![CpuSurface::new(1, 400, 240), CpuSurface::new(2, 400, 240)],
            true,
            |_| true,
        );
        let changed = history.reconcile(
            vec![CpuSurface::new(3, 400, 240), CpuSurface::new(1, 400, 240)],
            true,
            |_| true,
        );
        assert_eq!(changed.slots, [1, 0]);
        let removed = history.reconcile(vec![CpuSurface::new(3, 400, 240)], true, |_| true);
        assert_eq!(removed.slots, [1]);
        let resized = history.reconcile(vec![CpuSurface::new(3, 500, 240)], true, |_| true);
        assert_eq!(resized.slots, [1]);
        let added = history.reconcile(
            vec![CpuSurface::new(4, 400, 240), CpuSurface::new(3, 500, 240)],
            true,
            |_| true,
        );
        assert_eq!(added.slots, [0, 1]);
    }
}
