use super::PresentationRegistry;
use crate::{Extent2D, Rect};
use std::collections::HashSet;

/// Each registry belongs to one backend/device/context domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceIdentity {
    pub domain: u64,
    pub resource: u64,
    pub subresource: u32,
}
#[derive(Clone, Copy)]
pub struct RegistrySource {
    pub logical_slot: usize,
    pub identity: SourceIdentity,
    pub extent: Extent2D,
    pub placement: Option<Rect>,
}
pub(super) struct WindowObservation {
    window: isize,
    order: u64,
    seen: u64,
    sources: Vec<RegistrySource>,
}
#[derive(Clone, Copy)]
pub(super) struct Assignment {
    owner: u64,
    logical_slot: usize,
    identity: SourceIdentity,
    requested_index: usize,
    seen: u64,
}

type ObservedSource = (u64, isize, u64, usize, RegistrySource);
struct Reservations {
    reserved: HashSet<usize>,
    used: HashSet<SourceIdentity>,
    resolved: Vec<(usize, u64, usize, SourceIdentity)>,
}

impl PresentationRegistry {
    pub const OWNERSHIP_HORIZON: u64 = 240;
    pub const IDLE_HORIZON: u64 = 600;
    /// Claims a proven source set atomically; unrelated sources have independent owners.
    pub fn claim(&self, owner: u64, sources: &[SourceIdentity], now: u64) -> bool {
        let _guard = self
            .claim_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if sources.iter().any(|source| {
            self.owners.get(source).is_some_and(|claim| {
                claim.0 != owner && now.saturating_sub(claim.1) <= Self::OWNERSHIP_HORIZON
            })
        }) {
            return false;
        }
        for &source in sources {
            self.owners.insert(source, (owner, now));
        }
        true
    }
    /// Observes a presenting owner, using a clock supplied by its domain.
    pub fn observe(&self, owner: u64, window: isize, sources: Vec<RegistrySource>, now: u64) {
        let _guard = self
            .claim_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.windows
            .retain(|_, value| now.saturating_sub(value.seen) <= Self::IDLE_HORIZON);
        self.owners
            .retain(|_, (_, seen)| now.saturating_sub(*seen) <= Self::OWNERSHIP_HORIZON);
        let order = self.windows.get(&owner).map_or(now, |old| old.order);
        self.windows.insert(
            owner,
            WindowObservation {
                window,
                order,
                seen: now,
                sources,
            },
        );
    }
    pub fn forget_owner(&self, owner: u64) {
        let _guard = self
            .claim_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.assignments
            .retain(|_, assignment| assignment.owner != owner);
        self.windows.remove(&owner);
        self.placements
            .retain(|(old_owner, _), _| *old_owner != owner);
        self.owners.retain(|_, (old, _)| *old != owner);
    }
    /// Native adapters validate this metadata without holding registry guards.
    pub fn observed_windows(&self) -> Vec<isize> {
        let mut windows = self
            .windows
            .iter()
            .map(|entry| entry.window)
            .collect::<Vec<_>>();
        windows.sort_unstable();
        windows.dedup();
        windows
    }

    /// Fresh presentation windows in first-observation order. Multiple owners
    /// of one native window contribute one entry; stale owners cannot keep a
    /// dead fallback slot alive. The adapter supplies destruction notifications.
    pub fn ordered_windows(&self, now: u64) -> Vec<isize> {
        let mut observations = self
            .windows
            .iter()
            .filter(|entry| {
                entry.window != 0 && now.saturating_sub(entry.seen) <= Self::OWNERSHIP_HORIZON
            })
            .map(|entry| (entry.order, entry.window, *entry.key()))
            .collect::<Vec<_>>();
        observations.sort_unstable();
        let mut seen = HashSet::new();
        observations
            .into_iter()
            .filter_map(|(_, window, _)| seen.insert(window).then_some(window))
            .collect()
    }

    /// Explicit destruction invalidates assignments immediately, independently
    /// of the swapchain owner's lifetime and the short-gap grace period.
    pub fn forget_window(&self, window: isize) {
        let owners = self
            .windows
            .iter()
            .filter_map(|entry| (entry.window == window).then_some(*entry.key()))
            .collect::<Vec<_>>();
        for owner in owners {
            self.forget_owner(owner);
        }
        self.forget_routing_window(window);
    }
    /// Selects across windows before deciding which owner publishes each source.
    /// Identical dimensions never establish duplicate ownership.
    pub fn requests(
        &self,
        owner: u64,
        requested: &[usize],
        now: u64,
        mut matches: impl FnMut(usize, isize) -> Option<bool>,
    ) -> Vec<(usize, usize)> {
        let _guard = self
            .claim_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let observations = self.observed_sources(now);
        let Reservations {
            reserved,
            used,
            mut resolved,
        } = self.retain_assignments(requested, &observations, now, &mut matches);
        let selection = observations
            .iter()
            .filter(|(_, _, _, _, source)| !used.contains(&source.identity))
            .map(|(_, window, _, _, source)| (source.identity, *window))
            .collect::<Vec<_>>();
        for (slot, identity) in Self::resolve(requested, &selection, |slot, window| {
            if reserved.contains(&slot) {
                Some(false)
            } else {
                matches(slot, window)
            }
        }) {
            let &(candidate, _, _, index, source) = observations
                .iter()
                .find(|(_, _, _, _, source)| source.identity == identity)
                .unwrap();
            self.remember_assignment(slot, requested[slot], candidate, source);
            resolved.push((slot, candidate, index, identity));
        }
        resolved.sort_by_key(|(slot, ..)| *slot);
        resolved
            .into_iter()
            .filter_map(|(slot, candidate, index, identity)| {
                if candidate != owner {
                    return None;
                }
                self.owners.insert(identity, (owner, now));
                Some((slot, index))
            })
            .collect()
    }

    fn observed_sources(&self, now: u64) -> Vec<ObservedSource> {
        let mut observations = self
            .windows
            .iter()
            .flat_map(|entry| {
                let owner = *entry.key();
                let window = entry.window;
                let order = entry.order;
                let fresh = now.saturating_sub(entry.seen) <= Self::OWNERSHIP_HORIZON;
                entry
                    .sources
                    .iter()
                    .copied()
                    .enumerate()
                    .filter(move |_| fresh)
                    .map(move |(index, source)| (owner, window, order, index, source))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        observations.sort_by_key(|(owner, window, order, index, source)| {
            (
                u32::MAX - source.extent.height,
                u32::MAX - source.extent.width,
                *order,
                *owner,
                *window,
                *index,
            )
        });
        // Prefer the established presenting owner while it remains fresh.
        observations.retain(|(candidate, _, _, _, source)| {
            self.owners.get(&source.identity).is_none_or(|claim| {
                claim.0 == *candidate || now.saturating_sub(claim.1) > Self::OWNERSHIP_HORIZON
            })
        });
        let mut seen = HashSet::new();
        observations.retain(|(_, _, _, _, source)| seen.insert(source.identity));
        observations
    }

    fn retain_assignments(
        &self,
        requested: &[usize],
        observations: &[ObservedSource],
        now: u64,
        matches: &mut impl FnMut(usize, isize) -> Option<bool>,
    ) -> Reservations {
        self.assignments.retain(|slot, assignment| {
            requested.get(*slot) == Some(&assignment.requested_index)
                && now.saturating_sub(assignment.seen) <= Self::OWNERSHIP_HORIZON
        });
        let mut reserved = HashSet::new();
        let mut used = HashSet::new();
        let mut resolved = Vec::new();
        for (slot, &requested_index) in requested.iter().enumerate() {
            let Some(assignment) = self.assignments.get(&slot).map(|value| *value) else {
                continue;
            };
            let found = observations
                .iter()
                .find(|(_, _, _, _, source)| source.identity == assignment.identity)
                .or_else(|| {
                    observations.iter().find(|(candidate, _, _, _, source)| {
                        *candidate == assignment.owner
                            && source.logical_slot == assignment.logical_slot
                    })
                });
            if let Some(&(candidate, window, _, index, source)) = found {
                if matches(slot, window) == Some(false) {
                    self.assignments.remove(&slot);
                    continue;
                }
                if !used.insert(source.identity) {
                    continue;
                }
                self.remember_assignment(slot, requested_index, candidate, source);
                resolved.push((slot, candidate, index, source.identity));
            }
            reserved.insert(slot);
        }
        Reservations {
            reserved,
            used,
            resolved,
        }
    }

    fn remember_assignment(
        &self,
        slot: usize,
        requested_index: usize,
        owner: u64,
        source: RegistrySource,
    ) {
        let seen = self.windows.get(&owner).map_or(0, |window| window.seen);
        self.assignments.insert(
            slot,
            Assignment {
                owner,
                logical_slot: source.logical_slot,
                identity: source.identity,
                requested_index,
                seen,
            },
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn source(resource: u64) -> RegistrySource {
        RegistrySource {
            logical_slot: 0,
            identity: SourceIdentity {
                domain: 0,
                resource,
                subresource: 0,
            },
            extent: Extent2D {
                width: 320,
                height: 240,
            },
            placement: None,
        }
    }
    #[test]
    fn destroyed_window_releases_slots_while_its_swapchain_owner_is_still_alive() {
        let registry = PresentationRegistry::default();
        registry.observe(1, 10, vec![source(5)], 1);
        registry.observe(2, 20, vec![source(6)], 2);
        assert_eq!(registry.requests(1, &[0, 1], 2, |_, _| None), [(0, 0)]);
        assert_eq!(registry.requests(2, &[0, 1], 2, |_, _| None), [(1, 0)]);
        registry.forget_window(10);
        assert_eq!(registry.observed_windows(), [20]);
        registry.observe(3, 30, vec![source(5)], 3);
        assert_eq!(registry.requests(3, &[0, 1], 3, |_, _| None), [(0, 0)]);
        assert_eq!(registry.requests(2, &[0, 1], 3, |_, _| None), [(1, 0)]);
    }

    #[test]
    fn fallback_window_order_deduplicates_owners_and_retires_destroyed_or_stale_windows() {
        let registry = PresentationRegistry::default();
        registry.observe(1, 30, vec![], 1);
        registry.observe(2, 10, vec![], 2);
        registry.observe(3, 30, vec![], 3);
        registry.observe(4, 0, vec![], 4);
        registry.observe(2, 10, vec![], 5);
        assert_eq!(registry.ordered_windows(5), [30, 10]);
        registry.forget_window(30);
        assert_eq!(registry.ordered_windows(5), [10]);
        registry.observe(5, 30, vec![], 6);
        assert_eq!(registry.ordered_windows(6), [10, 30]);
        assert_eq!(registry.ordered_windows(245), [10, 30]);
        assert_eq!(registry.ordered_windows(246), [30]);
        assert!(registry.ordered_windows(247).is_empty());
    }

    #[test]
    fn simultaneous_equal_sources_have_deterministic_global_indices_and_duplicate_owner() {
        for owners in [[2, 1], [1, 2]] {
            let registry = PresentationRegistry::default();
            for owner in owners {
                registry.observe(owner, owner as isize * 10, vec![source(owner)], 7);
            }
            assert_eq!(registry.requests(2, &[0, 1], 7, |_, _| None), [(1, 0)]);
            assert_eq!(registry.requests(1, &[0, 1], 7, |_, _| None), [(0, 0)]);

            let registry = PresentationRegistry::default();
            for owner in owners {
                registry.observe(owner, owner as isize * 10, vec![source(9)], 7);
            }
            assert!(registry.requests(2, &[0], 7, |_, _| None).is_empty());
            assert_eq!(registry.requests(1, &[0], 7, |_, _| None), [(0, 0)]);
        }
    }

    #[test]
    fn equal_windows_stay_independent_and_only_shared_identity_claims_expire() {
        let registry = PresentationRegistry::default();
        registry.observe(1, 10, vec![source(5)], 1);
        registry.observe(2, 20, vec![source(6)], 2);
        assert_eq!(registry.requests(1, &[0, 1], 2, |_, _| None), [(0, 0)]);
        assert_eq!(registry.requests(2, &[0, 1], 2, |_, _| None), [(1, 0)]);
        registry.observe(2, 20, vec![source(5)], 3);
        assert!(registry.requests(2, &[0], 3, |_, _| None).is_empty());
        registry.observe(2, 20, vec![source(5)], 243);
        assert_eq!(registry.requests(2, &[0], 243, |_, _| None), [(0, 0)]);
        registry.forget_owner(2);
        assert!(registry.requests(2, &[0], 244, |_, _| None).is_empty());
    }
    impl crate::history::HistorySurface for RegistrySource {
        type Id = SourceIdentity;
        type Layout = Extent2D;
        fn identity(&self) -> SourceIdentity {
            self.identity
        }
        fn layout(&self) -> Extent2D {
            self.extent
        }
    }
    #[test]
    fn history_slots_survive_removal_page_flip_resize_and_window_reorder() {
        let mut history = crate::history::ResourceHistory::default();
        let registry = PresentationRegistry::default();
        let first = history.reconcile(vec![source(1), source(2)], true, |_| true);
        let observations = first
            .surfaces
            .into_iter()
            .zip(first.slots)
            .map(|(mut source, slot)| {
                source.logical_slot = slot;
                source
            })
            .collect();
        registry.observe(1, 10, observations, 1);
        assert_eq!(
            registry.requests(1, &[0, 1], 1, |_, _| None),
            [(0, 0), (1, 1)]
        );
        let missing = history.reconcile(vec![], false, |source| source.identity.resource != 1);
        assert_eq!(missing.slots, [1]);
        let mut kept = missing.surfaces[0];
        kept.logical_slot = missing.slots[0];
        registry.observe(1, 10, vec![kept], 2);
        assert_eq!(registry.requests(1, &[0, 1], 2, |_, _| None), [(1, 0)]);
        let flipped = history.reconcile(vec![source(3)], true, |_| true);
        assert_eq!(flipped.slots, [1]);
        let mut kept = flipped.surfaces[0];
        kept.logical_slot = flipped.slots[0];
        registry.observe(1, 10, vec![kept], 3);
        assert_eq!(registry.requests(1, &[0, 1], 3, |_, _| None), [(1, 0)]);
        kept.extent.height = 800;
        registry.observe(2, 20, vec![source(4)], 4);
        registry.observe(1, 10, vec![kept], 5);
        assert_eq!(registry.requests(1, &[0, 1], 5, |_, _| None), [(1, 0)]);
        // Destroying a presenting owner releases its slots, without renaming
        // existing assignments belonging to surviving windows.
        registry.forget_owner(1);
        assert_eq!(registry.requests(2, &[0, 1], 6, |_, _| None), [(0, 0)]);
    }
    #[test]
    fn title_precedence_and_expiry_apply_to_established_assignments() {
        let registry = PresentationRegistry::default();
        registry.observe(1, 10, vec![source(1)], 1);
        registry.observe(2, 20, vec![source(2)], 2);
        let matches = |slot, window| (slot == 0).then_some(window == 20);
        assert_eq!(registry.requests(1, &[0, 0], 2, matches), [(1, 0)]);
        assert_eq!(registry.requests(2, &[0, 0], 2, matches), [(0, 0)]);
        registry.observe(1, 10, vec![source(1)], 243);
        assert_eq!(registry.requests(1, &[0, 0], 243, matches), [(1, 0)]);
        assert!(registry.requests(2, &[0, 0], 243, matches).is_empty());
        registry.observe(3, 20, vec![source(3)], 244);
        assert_eq!(registry.requests(3, &[0, 0], 244, matches), [(0, 0)]);
    }
}
