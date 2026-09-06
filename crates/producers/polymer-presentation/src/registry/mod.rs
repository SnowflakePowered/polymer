//! Window/source assignment using metadata supplied by the presentation backend.

mod placements;
mod windows;
pub use windows::{RegistrySource, SourceIdentity};
#[derive(Default)]
pub struct PresentationRegistry {
    claim_lock: std::sync::Mutex<()>,
    placements: dashmap::DashMap<(u64, usize), placements::CachedPlacement>,
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
}
