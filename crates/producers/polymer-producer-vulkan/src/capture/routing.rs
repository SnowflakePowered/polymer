use super::*;

impl CaptureState {
    pub(super) fn presentation_targets(
        &self,
        domain: u64,
        owner_window: isize,
        owner_extent: vk::Extent2D,
        sources: &[tracking::PresentSource],
        requested_indices: &[usize],
        now: u64,
    ) -> (Vec<(usize, usize)>, Vec<PresentationTarget>) {
        let fallback = |index: usize| PresentationTarget {
            source_window: owner_window,
            presentation_extent: owner_extent,
            presentation_rect: sources
                .get(index)
                .and_then(|source| source.presentation_rect),
        };
        let same_layout = |candidate: &GraphPresentation| {
            let mut candidate_extents = candidate
                .sources
                .iter()
                .map(|source| (source.extent.width, source.extent.height))
                .collect::<Vec<_>>();
            let mut source_extents = sources
                .iter()
                .map(|source| (source.extent.width, source.extent.height))
                .collect::<Vec<_>>();
            candidate_extents.sort_unstable();
            source_extents.sort_unstable();
            candidate_extents == source_extents
        };
        let presentations = self
            .graph_presentations
            .iter()
            .map(|entry| entry.value().clone())
            .collect::<Vec<_>>();
        let mut by_window = HashMap::<isize, GraphPresentation>::new();
        for candidate in presentations.iter().filter(|candidate| {
            candidate.domain == domain
                && candidate.source_window != 0
                && now.saturating_sub(candidate.last_present)
                    <= polymer_presentation::registry::PresentationRegistry::OWNERSHIP_HORIZON
                && same_layout(candidate)
        }) {
            match by_window.entry(candidate.source_window) {
                std::collections::hash_map::Entry::Occupied(mut entry)
                    if entry.get().creation_order < candidate.creation_order =>
                {
                    entry.insert(candidate.clone());
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(candidate.clone());
                }
                _ => {}
            }
        }
        let mut candidates = by_window.values().cloned().collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|candidate| candidate.window_order);
        let assignment = (!sources.is_empty() && candidates.len() >= sources.len())
            .then(|| GraphPresentation::assign(sources, &candidates));
        let discovered = assignment.as_ref().map(|assignment| {
            assignment
                .iter()
                .copied()
                .enumerate()
                .map(|(index, candidate_index)| {
                    let candidate = &candidates[candidate_index];
                    PresentationTarget {
                        source_window: candidate.source_window,
                        presentation_extent: candidate.presentation_extent,
                        presentation_rect: candidate.placement(sources[index].extent),
                    }
                })
                .collect::<Vec<_>>()
        });
        drop(presentations);
        let layouts = &self.graph_target_layouts;
        if let Some(discovered) = discovered {
            for (source, target) in sources.iter().zip(discovered) {
                if let dashmap::mapref::entry::Entry::Vacant(entry) =
                    layouts.entry((domain, source.extent.width, source.extent.height))
                {
                    tracing::debug!(
                        "polymer: graph target layout {}x{} -> window {:#x} ({}x{})",
                        source.extent.width,
                        source.extent.height,
                        target.source_window,
                        target.presentation_extent.width,
                        target.presentation_extent.height,
                    );
                    entry.insert(target);
                }
            }
        }

        let mut resolved = vec![None; requested_indices.len()];
        let mut used = Vec::new();
        if let Some(assignment) = &assignment {
            for slot in 0..requested_indices.len() {
                if !config::tracked_render_target_has_surface_pattern(slot) {
                    continue;
                }
                let Some(candidate_index) = candidates.iter().position(|candidate| {
                    window_title(NativeWindow(candidate.source_window)).is_some_and(|title| {
                        config::tracked_render_target_surface_matches(slot, &title) == Some(true)
                    })
                }) else {
                    continue;
                };
                let Some(source_index) = assignment
                    .iter()
                    .position(|&assigned| assigned == candidate_index)
                else {
                    continue;
                };
                resolved[slot] = Some((source_index, Some(candidate_index)));
                used.push(source_index);
            }
        }
        for (slot, &requested_index) in requested_indices.iter().enumerate() {
            if resolved[slot].is_some() || config::tracked_render_target_has_surface_pattern(slot) {
                continue;
            }
            let requested_source = sources.iter().enumerate().position(|(index, source)| {
                source.logical_slot.unwrap_or(index) == requested_index
            });
            let source_index = requested_source
                .filter(|index| !used.contains(index))
                .or_else(|| {
                    requested_source
                        .and_then(|_| (0..sources.len()).find(|index| !used.contains(index)))
                });
            if let Some(source_index) = source_index {
                resolved[slot] = Some((source_index, None));
                used.push(source_index);
            }
        }
        let resolved = resolved
            .into_iter()
            .enumerate()
            .filter_map(|(slot, resolved)| {
                resolved.map(|(index, candidate)| (slot, index, candidate))
            })
            .collect::<Vec<_>>();
        let targets = resolved
            .iter()
            .map(|&(_, index, matched_candidate)| {
                if let Some(candidate_index) = matched_candidate {
                    let candidate = &candidates[candidate_index];
                    return PresentationTarget {
                        source_window: candidate.source_window,
                        presentation_extent: candidate.presentation_extent,
                        presentation_rect: candidate.placement(sources[index].extent),
                    };
                }
                let Some(source) = sources.get(index) else {
                    return fallback(index);
                };
                let key = (domain, source.extent.width, source.extent.height);
                let Some(cached) = layouts.get(&key).map(|entry| *entry) else {
                    return fallback(index);
                };
                // Only the HWND is sticky; the window's geometry is not. A
                // resize recreates the swapchain with a new extent, and
                // coordinates sent against the discovery-time extent would
                // land on the old letterbox. Re-derive extent and placement
                // from the window's live presentation whenever one exists.
                let Some(live) = by_window.get(&cached.source_window) else {
                    return cached;
                };
                let refreshed = PresentationTarget {
                    source_window: cached.source_window,
                    presentation_extent: live.presentation_extent,
                    presentation_rect: live
                        .sources
                        .get(index)
                        .and_then(|source| source.presentation_rect),
                };
                layouts.insert(key, refreshed);
                refreshed
            })
            .collect();
        (
            resolved
                .into_iter()
                .map(|(slot, index, _)| (slot, index))
                .collect(),
            targets,
        )
    }
}

impl GraphPresentation {
    fn assign(sources: &[tracking::PresentSource], candidates: &[GraphPresentation]) -> Vec<usize> {
        let costs = sources
            .iter()
            .map(|source| {
                candidates
                    .iter()
                    .map(|candidate| candidate.cost(source.extent))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let count = costs.first().map_or(0, Vec::len);
        if costs.len() > count || count > 16 || costs.iter().any(|row| row.len() != count) {
            return (0..sources.len()).collect();
        }
        let mut states = vec![None::<(u128, Vec<usize>)>; 1 << count];
        states[0] = Some((0, Vec::new()));
        for mask in 0usize..states.len() {
            let Some((cost, path)) = states[mask].clone() else {
                continue;
            };
            if path.len() == costs.len() {
                continue;
            }
            for (candidate, &candidate_cost) in costs[path.len()].iter().enumerate() {
                if mask & (1 << candidate) != 0 {
                    continue;
                }
                let next = mask | (1 << candidate);
                let next_cost = cost.saturating_add(candidate_cost);
                let mut next_path = path.clone();
                next_path.push(candidate);
                if states[next]
                    .as_ref()
                    .is_none_or(|old| (next_cost, &next_path) < (old.0, &old.1))
                {
                    states[next] = Some((next_cost, next_path));
                }
            }
        }
        states
            .into_iter()
            .flatten()
            .filter(|(_, path)| path.len() == costs.len())
            .min()
            .map_or_else(|| (0..sources.len()).collect(), |(_, path)| path)
    }
    fn cost(&self, source: vk::Extent2D) -> u128 {
        let target = self.presentation_extent;
        let direct = (u128::from(source.width) * u128::from(target.height))
            .abs_diff(u128::from(source.height) * u128::from(target.width));
        let transposed = (u128::from(source.height) * u128::from(target.height))
            .abs_diff(u128::from(source.width) * u128::from(target.width));
        let aspect_error = direct.min(transposed);
        let presentation_rect = self.placement(source);
        let missing_placement = u128::from(presentation_rect.is_none());
        let target_area = u128::from(target.width) * u128::from(target.height);
        let covered_area = presentation_rect.map_or(0, |rect| {
            u128::from(rect.extent.width) * u128::from(rect.extent.height)
        });
        let uncovered_area = target_area.saturating_sub(covered_area.min(target_area));
        (missing_placement << 120) | (aspect_error << 48) | uncovered_area.min((1u128 << 48) - 1)
    }

    fn placement(&self, source_extent: vk::Extent2D) -> Option<vk::Rect2D> {
        self.sources
            .iter()
            .find(|source| source.extent == source_extent)
            .and_then(|source| source.presentation_rect)
    }
}
