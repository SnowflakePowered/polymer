//! Deterministic presentation-window selection.
pub struct PresentationRegistry;
impl PresentationRegistry {
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
        let mut used: Vec<I> = Vec::new();
        for (slot, _) in requested.iter().enumerate() {
            if matches(slot, W::default()).is_none() {
                continue;
            }
            let matching = selection
                .iter()
                .filter(|(_, window)| matches(slot, *window) == Some(true))
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            let candidate = matching.iter().find(|id| !used.contains(*id)).copied();
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
    fn title_requests_claim_windows_before_positional_requests() {
        let selection = [(10, "GamePad"), (20, "TV")];
        let result = PresentationRegistry::resolve(&[0, 1], &selection, |slot, title| {
            (slot == 1).then_some(title == "GamePad")
        });
        assert_eq!(result, [(0, 20), (1, 10)]);
        let missing = PresentationRegistry::resolve(&[0, 1], &selection, |slot, title| {
            (slot == 1).then_some(title == "Missing")
        });
        assert_eq!(missing, [(0, 10)]);
    }
}
