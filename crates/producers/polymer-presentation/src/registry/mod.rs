//! Window/source assignment using metadata supplied by the presentation backend.
use crate::Extent2D;

mod placements;
mod routing;
mod windows;
pub use routing::{PresentationTarget, RoutingSource, WindowPresentation};
pub use windows::{RegistrySource, SourceIdentity};
#[derive(Default)]
pub struct PresentationRegistry {
    #[cfg(feature = "producer")]
    pub(crate) window_event_generation: std::sync::atomic::AtomicU64,
    claim_lock: std::sync::Mutex<()>,
    placements: dashmap::DashMap<(u64, usize), placements::CachedPlacement>,
    routes: dashmap::DashMap<SourceIdentity, routing::CachedRoute>,
    route_slots: dashmap::DashMap<(u64, isize, usize), SourceIdentity>,
    selection_slots: dashmap::DashMap<(u64, isize, usize), routing::SelectionSlot>,
    assignments: dashmap::DashMap<usize, windows::Assignment>,
    windows: dashmap::DashMap<u64, windows::WindowObservation>,
    owners: dashmap::DashMap<SourceIdentity, (u64, u64)>,
}

impl PresentationRegistry {
    /// Explicit identity matches take precedence over observation order. Equal
    /// cardinalities allow the remaining observations to follow logical slots
    /// through page flips; otherwise unmatched placements remain unknown.
    pub fn pair_placements<I: Copy + Eq, P: Copy>(
        selected: impl IntoIterator<Item = I>,
        observed: &[(I, P)],
    ) -> Vec<Option<P>> {
        let selected = selected.into_iter().collect::<Vec<_>>();
        let mut used = vec![false; observed.len()];
        let mut placements = selected
            .iter()
            .map(|source| {
                // A partial identity supplied by an adapter must not assign
                // one observation to multiple independently selected layers.
                if selected
                    .iter()
                    .filter(|identity| *identity == source)
                    .count()
                    != 1
                {
                    return None;
                }
                let mut matching = observed
                    .iter()
                    .enumerate()
                    .filter(|(_, (identity, _))| identity == source);
                match (matching.next(), matching.next()) {
                    (Some((index, (_, placement))), None) => {
                        used[index] = true;
                        Some(*placement)
                    }
                    _ => None,
                }
            })
            .collect::<Vec<_>>();
        let unique_identities = selected
            .iter()
            .enumerate()
            .all(|(index, identity)| !selected[..index].contains(identity))
            && observed.iter().enumerate().all(|(index, (identity, _))| {
                !observed[..index]
                    .iter()
                    .any(|(earlier, _)| earlier == identity)
            });
        if observed.len() == selected.len() && unique_identities {
            let mut remaining = observed
                .iter()
                .zip(used)
                .filter_map(|((_, placement), used)| (!used).then_some(*placement));
            for placement in placements
                .iter_mut()
                .filter(|placement| placement.is_none())
            {
                *placement = remaining.next();
            }
        }
        placements
    }

    pub fn logical_surface_id(slot: usize) -> u64 {
        0x5347_0000_0000_0000 | slot as u64
    }
    /// Title-qualified requests claim their matching sources before positional
    /// requests consume the remaining stable source order.
    pub fn resolve<I: Copy + Eq, W: Copy + Default>(
        requested: &[usize],
        selection: &[(I, W)],
        mut matches: impl FnMut(usize, W) -> Option<bool>,
    ) -> Vec<(usize, I)> {
        let (mut resolved, mut used) =
            Self::resolve_title_requests(requested, selection, &mut matches);
        for (slot, &index) in requested.iter().enumerate() {
            if resolved[slot].is_some() || matches(slot, W::default()).is_some() {
                continue;
            }
            let candidate = selection
                .get(index)
                .filter(|(id, _)| !used.contains(id))
                .or_else(|| selection.iter().find(|(id, _)| !used.contains(id)));
            if let Some(&(id, _)) = candidate {
                resolved[slot] = Some(id);
                used.push(id);
            }
        }
        resolved
            .into_iter()
            .enumerate()
            .filter_map(|(slot, id)| id.map(|id| (slot, id)))
            .collect()
    }

    fn resolve_title_requests<I: Copy + Eq, W: Copy + Default>(
        requested: &[usize],
        selection: &[(I, W)],
        matches: &mut impl FnMut(usize, W) -> Option<bool>,
    ) -> (Vec<Option<I>>, Vec<I>) {
        let mut resolved = vec![None; requested.len()];
        let mut used = Vec::new();
        for (slot, &index) in requested.iter().enumerate() {
            if matches(slot, W::default()).is_none() {
                continue;
            }
            let matching = selection
                .iter()
                .filter(|(_, window)| matches(slot, *window) == Some(true))
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            let candidate = matching
                .get(index)
                .copied()
                .filter(|id| !used.contains(id))
                .or_else(|| {
                    matching
                        .get(index)
                        .and_then(|_| matching.iter().find(|id| !used.contains(id)).copied())
                });
            if let Some(candidate) = candidate {
                resolved[slot] = Some(candidate);
                used.push(candidate);
            }
        }
        (resolved, used)
    }

    pub fn placement_cost(source: Extent2D, target: Extent2D, covered: Option<Extent2D>) -> u128 {
        let direct = (u128::from(source.width) * u128::from(target.height))
            .abs_diff(u128::from(source.height) * u128::from(target.width));
        let transposed = (u128::from(source.height) * u128::from(target.height))
            .abs_diff(u128::from(source.width) * u128::from(target.width));
        let uncovered = target
            .area()
            .saturating_sub(covered.map_or(0, Extent2D::area).min(target.area()));
        (u128::from(covered.is_none()) << 120)
            | (direct.min(transposed) << 48)
            | u128::from(uncovered).min((1u128 << 48) - 1)
    }

    /// Minimum-cost injective assignment. A subset DP bounds work to 2^16
    /// rather than exploring all permutations of windows on a present thread.
    pub fn assign(costs: &[Vec<u128>]) -> Option<Vec<usize>> {
        let count = costs.first().map_or(0, Vec::len);
        if costs.len() > count || count > 16 || costs.iter().any(|row| row.len() != count) {
            return None;
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
            .map(|(_, path)| path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn placement_identity_survives_reordering_before_page_flip_inference() {
        let observed = [(11, "top"), (12, "bottom"), (13, "side")];
        assert_eq!(
            PresentationRegistry::pair_placements([13, 11, 12], &observed),
            [Some("side"), Some("top"), Some("bottom")],
        );
        // Two flipped resources occupy remaining slots, without moving the
        // surviving resource away from its explicit destination.
        assert_eq!(
            PresentationRegistry::pair_placements([21, 13, 22], &observed),
            [Some("top"), Some("side"), Some("bottom")],
        );
        // An extra overlay disables ordered inference but retains identity.
        assert_eq!(
            PresentationRegistry::pair_placements([21, 13], &observed),
            [None, Some("side")],
        );
        assert_eq!(
            PresentationRegistry::pair_placements([13, 13], &observed),
            [None, None],
        );
        // Equal cardinality does not disambiguate repeated identities, such
        // as an adapter's partial identity for independently selected layers.
        assert_eq!(
            PresentationRegistry::pair_placements([13, 13, 11], &observed),
            [None, None, Some("top")],
        );
        assert_eq!(
            PresentationRegistry::pair_placements(
                [11, 12, 13],
                &[(11, "top"), (12, "left"), (12, "right")]
            ),
            [Some("top"), None, None],
        );
    }

    #[test]
    fn assignment_avoids_greedy_window_choice_and_preserves_ties() {
        assert_eq!(
            PresentationRegistry::assign(&[vec![1, 2], vec![0, 100]]),
            Some(vec![1, 0])
        );
        assert_eq!(
            PresentationRegistry::assign(&[vec![1, 1], vec![1, 1]]),
            Some(vec![0, 1])
        );
        assert_eq!(PresentationRegistry::assign(&[vec![1], vec![2]]), None);
    }
}
