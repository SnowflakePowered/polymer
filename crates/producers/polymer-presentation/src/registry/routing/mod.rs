use super::{PresentationRegistry, SourceIdentity};
use crate::{Extent2D, Rect};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub struct RoutingSource {
    pub identity: SourceIdentity,
    pub logical_slot: Option<usize>,
    pub extent: Extent2D,
    pub placement: Option<Rect>,
}

#[derive(Clone)]
pub struct WindowPresentation {
    pub window: isize,
    pub extent: Extent2D,
    pub creation_order: u64,
    pub window_order: usize,
    pub seen: u64,
    pub sources: Vec<RoutingSource>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentationTarget {
    pub window: isize,
    pub extent: Extent2D,
    pub placement: Option<Rect>,
}

#[derive(Clone, Copy)]
pub(super) struct CachedRoute {
    target: PresentationTarget,
    seen: u64,
}

mod requests;
pub(super) use requests::SelectionSlot;

impl WindowPresentation {
    fn shares_layout(&self, sources: &[RoutingSource]) -> bool {
        // A shared overlay does not prove that the other equal-sized sources
        // are shared too. Compare identities together with their own extents;
        // established logical-slot routes handle page flips separately.
        let layout = |sources: &[RoutingSource]| {
            let mut layout = sources
                .iter()
                .map(|source| {
                    (
                        source.identity.domain,
                        source.identity.resource,
                        source.identity.subresource,
                        source.extent.width,
                        source.extent.height,
                    )
                })
                .collect::<Vec<_>>();
            layout.sort_unstable();
            layout
        };
        !sources.is_empty() && layout(&self.sources) == layout(sources)
    }

    fn target(&self, source: &RoutingSource) -> PresentationTarget {
        PresentationTarget {
            window: self.window,
            extent: self.extent,
            placement: self
                .sources
                .iter()
                .find(|candidate| candidate.identity == source.identity)
                .and_then(|source| source.placement),
        }
    }
}

impl PresentationRegistry {
    fn routing_candidates(
        observations: &[WindowPresentation],
        sources: &[RoutingSource],
        now: u64,
    ) -> Vec<WindowPresentation> {
        let mut windows = HashMap::<isize, WindowPresentation>::new();
        for candidate in observations.iter().filter(|candidate| {
            candidate.window != 0 && now.saturating_sub(candidate.seen) <= Self::OWNERSHIP_HORIZON
        }) {
            let current = windows
                .entry(candidate.window)
                .or_insert_with(|| candidate.clone());
            if (current.creation_order, current.seen) < (candidate.creation_order, candidate.seen) {
                *current = candidate.clone();
            }
        }
        // A replacement observation supersedes its predecessor even when its
        // layout no longer matches. Filtering first could resurrect the old
        // swapchain as evidence for assigning a source to this native window.
        windows.retain(|_, candidate| candidate.shares_layout(sources));
        let mut candidates = windows.into_values().collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| (candidate.window_order, candidate.window));
        candidates
    }

    fn discover_routes(
        sources: &[RoutingSource],
        candidates: &[WindowPresentation],
    ) -> Vec<PresentationTarget> {
        if sources.is_empty() || candidates.len() < sources.len() {
            return Vec::new();
        }
        let costs = sources
            .iter()
            .map(|source| {
                candidates
                    .iter()
                    .map(|candidate| {
                        Self::placement_cost(
                            source.extent,
                            candidate.extent,
                            candidate.target(source).placement.map(|rect| rect.extent),
                        )
                    })
                    .collect()
            })
            .collect::<Vec<_>>();
        Self::assign(&costs).map_or_else(Vec::new, |assignment| {
            assignment
                .into_iter()
                .enumerate()
                .map(|(index, candidate)| candidates[candidate].target(&sources[index]))
                .collect()
        })
    }

    fn retained_route(&self, owner_window: isize, source: &RoutingSource) -> Option<CachedRoute> {
        self.routes
            .get(&source.identity)
            .map(|entry| *entry)
            .or_else(|| {
                let slot = source.logical_slot?;
                let previous =
                    *self
                        .route_slots
                        .get(&(source.identity.domain, owner_window, slot))?;
                self.routes.get(&previous).map(|entry| *entry)
            })
    }

    fn routing_targets(
        &self,
        owner: PresentationTarget,
        sources: &[RoutingSource],
        candidates: &[WindowPresentation],
        now: u64,
    ) -> Vec<PresentationTarget> {
        self.routes
            .retain(|_, route| now.saturating_sub(route.seen) <= Self::OWNERSHIP_HORIZON);
        self.route_slots
            .retain(|_, source| self.routes.contains_key(source));
        let discovered = Self::discover_routes(sources, candidates);
        // Read every old slot before publishing replacements, so page flips
        // cannot consume a sibling's newly assigned identity in the same batch.
        let retained = sources
            .iter()
            .map(|source| self.retained_route(owner.window, source))
            .collect::<Vec<_>>();
        sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let cached = retained[index].or_else(|| {
                    discovered.get(index).map(|target| CachedRoute {
                        target: *target,
                        seen: now,
                    })
                });
                let Some(mut cached) = cached else {
                    return PresentationTarget {
                        placement: source.placement,
                        ..owner
                    };
                };
                if let Some(live) = candidates
                    .iter()
                    .find(|candidate| candidate.window == cached.target.window)
                {
                    cached.target = live.target(source);
                    cached.seen = live.seen;
                }
                self.routes.insert(source.identity, cached);
                if let Some(slot) = source.logical_slot {
                    self.route_slots.insert(
                        (source.identity.domain, owner.window, slot),
                        source.identity,
                    );
                }
                cached.target
            })
            .collect()
    }

    /// Resolves only supplied observations and title-match results. Native
    /// adapters perform all window queries before entering this computation.
    pub fn route_presentations(
        &self,
        owner: PresentationTarget,
        sources: &[RoutingSource],
        observations: &[WindowPresentation],
        requested: &[usize],
        now: u64,
        mut matches: impl FnMut(usize, isize) -> Option<bool>,
    ) -> (Vec<(usize, usize)>, Vec<PresentationTarget>) {
        let _guard = self
            .claim_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let candidates = Self::routing_candidates(observations, sources, now);
        let targets = self.routing_targets(owner, sources, &candidates, now);
        let selection_indices = self.selection_indices(owner.window, sources, &targets, now);
        let resolved = self
            .resolve_routing_requests(requested, now, &mut matches)
            .into_iter()
            .filter_map(|(slot, global)| {
                selection_indices
                    .iter()
                    .position(|index| *index == global)
                    .map(|index| (slot, index))
            })
            .collect::<Vec<_>>();
        let targets = resolved.iter().map(|(_, index)| targets[*index]).collect();
        (resolved, targets)
    }

    /// Window destruction is distinct from swapchain replacement or a short gap.
    pub fn forget_routing_window(&self, window: isize) {
        let _guard = self
            .claim_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.routes.retain(|_, route| route.target.window != window);
        self.selection_slots
            .retain(|(_, owner, _), _| *owner != window);
        self.route_slots
            .retain(|(_, owner, _), source| *owner != window && self.routes.contains_key(source));
    }

    /// Native device/context destruction invalidates identities immediately,
    /// even if that domain's raw handle is reused before the expiry horizon.
    pub fn forget_routing_domain(&self, domain: u64) {
        let _guard = self
            .claim_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.routes.retain(|source, _| source.domain != domain);
        self.selection_slots
            .retain(|(owner_domain, _, _), _| *owner_domain != domain);
        self.route_slots
            .retain(|(owner_domain, _, _), _| *owner_domain != domain);
    }
}

#[cfg(test)]
mod tests;
