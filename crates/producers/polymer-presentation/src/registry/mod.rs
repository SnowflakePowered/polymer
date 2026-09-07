//! Window/source assignment using metadata supplied by the presentation backend.

mod windows;
pub use windows::{RegistrySource, SourceAssignment, SourceIdentity};
#[derive(Default)]
pub struct PresentationRegistry {
    claim_lock: std::sync::Mutex<()>,
    assignments: dashmap::DashMap<usize, windows::Assignment>,
    windows: dashmap::DashMap<u64, windows::WindowObservation>,
    owners: dashmap::DashMap<SourceIdentity, (u64, u64)>,
}

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
