use super::{PresentationRegistry, RegistrySource, SourceIdentity};
use crate::{Extent2D, Rect};

pub(super) struct CachedPlacement {
    identity: SourceIdentity,
    source_extent: Extent2D,
    window_extent: Extent2D,
    bounds: Rect,
    seen: u64,
}

impl PresentationRegistry {
    /// Retains unambiguous window placement through short observation gaps and
    /// resource page flips. Callers supply validated logical slots and normalize
    /// API coordinates before recording; this method makes no native queries.
    pub fn remember_placements(
        &self,
        owner: u64,
        window_extent: Extent2D,
        sources: &[RegistrySource],
        now: u64,
    ) -> Vec<Option<Rect>> {
        let _guard = self
            .claim_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.placements.retain(|(old_owner, slot), value| {
            now.saturating_sub(value.seen) <= Self::OWNERSHIP_HORIZON
                && (*old_owner != owner
                    || (value.window_extent == window_extent
                        && sources.iter().any(|source| source.logical_slot == *slot)))
        });
        sources
            .iter()
            .map(|source| {
                let key = (owner, source.logical_slot);
                if let Some(bounds) = source.placement {
                    self.placements.insert(
                        key,
                        CachedPlacement {
                            identity: source.identity,
                            source_extent: source.extent,
                            window_extent,
                            bounds,
                            seen: now,
                        },
                    );
                    return Some(bounds);
                }
                let retained = self
                    .placements
                    .get(&key)
                    .filter(|cached| {
                        cached.identity.domain == source.identity.domain
                            && cached.identity.subresource == source.identity.subresource
                            && cached.source_extent == source.extent
                    })
                    .map(|cached| cached.bounds);
                if retained.is_none() {
                    self.placements.remove(&key);
                }
                retained
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn placements_follow_logical_slots_but_expire_on_resize_removal_and_stale_evidence() {
        let registry = PresentationRegistry::default();
        let extent = Extent2D {
            width: 640,
            height: 480,
        };
        let mut sources = [0, 1].map(|slot| RegistrySource {
            logical_slot: slot,
            identity: SourceIdentity {
                domain: 7,
                resource: slot as u64 + 1,
                subresource: slot as u32,
            },
            extent: Extent2D {
                width: 320,
                height: 240,
            },
            placement: Some(Rect {
                x: slot as i32 * 320,
                y: -20,
                extent: Extent2D {
                    width: 320,
                    height: 240,
                },
            }),
        });
        let original = registry.remember_placements(1, extent, &sources, 1);
        for source in &mut sources {
            source.identity.resource += 10;
            source.placement = None;
        }
        sources.reverse();
        assert_eq!(
            registry.remember_placements(1, extent, &sources, 2),
            [original[1], original[0]]
        );
        assert_eq!(
            registry.remember_placements(2, extent, &sources, 2),
            [None, None]
        );
        assert_eq!(
            registry.remember_placements(1, extent, &sources[..1], 3),
            [original[1]]
        );
        assert_eq!(
            registry.remember_placements(1, extent, &sources, 4),
            [original[1], None]
        );
        sources[0].identity.subresource = 9;
        assert_eq!(
            registry.remember_placements(1, extent, &sources, 5),
            [None, None]
        );
        sources[0].identity.subresource = 1;
        assert_eq!(
            registry.remember_placements(1, extent, &sources, 6),
            [None, None]
        );
        sources[0].placement = original[1];
        registry.remember_placements(1, extent, &sources, 1);
        sources[0].placement = None;
        assert_eq!(
            registry.remember_placements(1, extent, &sources, 242),
            [None, None]
        );
        sources[0].placement = original[1];
        registry.remember_placements(1, extent, &sources, 243);
        sources[0].placement = None;
        assert_eq!(
            registry.remember_placements(
                1,
                Extent2D {
                    width: 800,
                    height: 600
                },
                &sources,
                244
            ),
            [None, None]
        );
    }
}
