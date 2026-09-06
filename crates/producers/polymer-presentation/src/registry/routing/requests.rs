use super::{PresentationRegistry, PresentationTarget, RoutingSource, SourceIdentity};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(in crate::registry) struct SelectionSlot {
    identity: SourceIdentity,
    index: usize,
    seen: u64,
    target: PresentationTarget,
    active: bool,
}

impl PresentationRegistry {
    /// History slots belong to one presenter. Requested positional indices
    /// belong to the registry, including presenters with unrelated layouts.
    pub(super) fn selection_indices(
        &self,
        owner: isize,
        sources: &[RoutingSource],
        targets: &[PresentationTarget],
        now: u64,
    ) -> Vec<usize> {
        self.selection_slots
            .retain(|_, slot| now.saturating_sub(slot.seen) <= Self::IDLE_HORIZON);
        let mut ordered = sources.iter().enumerate().collect::<Vec<_>>();
        ordered.sort_by_key(|(index, source)| source.logical_slot.unwrap_or(*index));
        let mut indices = vec![0; sources.len()];
        // Reserve all identities before replacing any local slot. Otherwise
        // reordered resources can consume a sibling's newly written identity.
        let previous = self
            .selection_slots
            .iter()
            .map(|entry| (*entry.key(), *entry))
            .collect::<Vec<_>>();
        let existing = sources
            .iter()
            .map(|source| {
                previous
                    .iter()
                    .filter(|(_, entry)| entry.identity == source.identity)
                    .map(|(_, entry)| entry.index)
                    .min()
            })
            .collect::<Vec<_>>();
        let mut reserved = existing.iter().flatten().copied().collect::<Vec<_>>();
        // Duplicate presenters may share an index by identity. Once one changes
        // source, its local slot cannot take that index away from the other
        // presenter. Retain this reservation across the same idle grace period
        // as other logical slots; exact identity matches can still share it.
        reserved.extend(
            previous
                .iter()
                .filter(|((_, window, _), _)| *window != owner)
                .map(|(_, entry)| entry.index),
        );
        for mut entry in self
            .selection_slots
            .iter_mut()
            .filter(|entry| entry.key().1 == owner)
        {
            entry.active = false;
        }
        for (position, source) in ordered {
            let local = source.logical_slot.unwrap_or(position);
            let key = (source.identity.domain, owner, local);
            let retained = previous.iter().find_map(|(old_key, entry)| {
                (*old_key == key && !reserved.contains(&entry.index)).then_some(entry.index)
            });
            let displaced = previous
                .iter()
                .filter(|((domain, window, _), entry)| {
                    source.logical_slot.is_none()
                        && *domain == source.identity.domain
                        && *window == owner
                        && !reserved.contains(&entry.index)
                })
                .map(|(_, entry)| entry.index)
                .min();
            let index = existing[position]
                .or(retained)
                .or(displaced)
                .unwrap_or_else(|| {
                    (local..)
                        .find(|index| {
                            !reserved.contains(index)
                                && !self
                                    .selection_slots
                                    .iter()
                                    .any(|entry| entry.index == *index)
                        })
                        .expect("logical surface index space exhausted")
                });
            reserved.push(index);
            self.selection_slots.insert(
                key,
                SelectionSlot {
                    identity: source.identity,
                    index,
                    seen: now,
                    target: targets[position],
                    active: true,
                },
            );
            indices[position] = index;
        }
        indices
    }

    pub(super) fn resolve_routing_requests(
        &self,
        requested: &[usize],
        now: u64,
        matches: &mut impl FnMut(usize, isize) -> Option<bool>,
    ) -> Vec<(usize, usize)> {
        let mut global = BTreeMap::<usize, SelectionSlot>::new();
        for entry in self.selection_slots.iter().filter(|entry| {
            entry.active && now.saturating_sub(entry.seen) <= Self::OWNERSHIP_HORIZON
        }) {
            let selected = global.entry(entry.index).or_insert(*entry);
            if (entry.seen, entry.target.window) > (selected.seen, selected.target.window) {
                *selected = *entry;
            }
        }
        let selection = global
            .iter()
            .map(|(index, source)| (*index, source.target.window))
            .collect::<Vec<_>>();
        let (mut resolved, mut used) = Self::resolve_title_requests(requested, &selection, matches);
        for (slot, &index) in requested.iter().enumerate() {
            if matches(slot, 0).is_some() || !global.contains_key(&index) {
                continue;
            }
            let candidate = (!used.contains(&index))
                .then_some(index)
                .or_else(|| global.keys().find(|id| !used.contains(id)).copied());
            if let Some(candidate) = candidate {
                resolved[slot] = Some(candidate);
                used.push(candidate);
            }
        }
        resolved
            .into_iter()
            .enumerate()
            .filter_map(|(slot, index)| index.map(|index| (slot, index)))
            .collect()
    }
}
