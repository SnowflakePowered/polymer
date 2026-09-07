use super::ProducerWindows;
use crate::{
    history::{HistorySurface, Reconciliation, ResourceHistory},
    registry::{PresentationRegistry, RegistrySource, SourceIdentity},
};

/// Presentation metadata shared by DXGI producers; native capture stays with its owner.
pub struct CapturePreparation<'a> {
    pub registry: &'a PresentationRegistry,
    pub owner: u64,
    pub window: isize,
    pub generation: u64,
    pub requested: &'a [usize],
    pub tracking: bool,
}

impl CapturePreparation<'_> {
    pub fn prepare<T: HistorySurface<Id = (usize, u32)>>(
        &self,
        history: &mut ResourceHistory<T>,
        mut observed: Vec<T>,
        revalidate: impl FnMut(&mut T) -> bool,
        mut matches: impl FnMut(usize, &str) -> Option<bool>,
    ) -> (Reconciliation<T>, Vec<(usize, usize)>) {
        let exact =
            !observed.is_empty() && observed.iter_mut().all(|source| source.selection().exact);
        let mut reconciled = history.reconcile(observed, exact, revalidate);
        ProducerWindows::refresh(self.registry);
        let sources = reconciled
            .surfaces
            .iter_mut()
            .zip(&reconciled.slots)
            .map(|(source, &logical_slot)| {
                let (resource, subresource) = source.identity();
                RegistrySource {
                    logical_slot,
                    identity: SourceIdentity {
                        domain: 0,
                        resource: if resource == 0 {
                            self.owner
                        } else {
                            resource as u64
                        },
                        subresource,
                    },
                    extent: source.selection().source_rect.extent,
                }
            })
            .collect();
        self.registry
            .observe(self.owner, self.window, sources, self.generation);
        let assignments = if self.tracking && !reconciled.surfaces.is_empty() {
            self.registry.requests(
                self.owner,
                self.requested,
                self.generation,
                |slot, window| {
                    let title = polymer_platform_windows::window_title(
                        polymer_platform_windows::NativeWindow(window),
                    );
                    matches(slot, title.as_deref().unwrap_or_default())
                },
            )
        } else {
            vec![(0, 0)]
        };
        (reconciled, assignments)
    }
}
