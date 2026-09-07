//! CPU resource metadata for tests of the real shared policy. No native
//! devices, windows, hooks, handles, IPC, or graphics runtime are involved.
use crate::{
    history::{HistorySurface, Reconciliation, ResourceHistory},
    *,
};

#[derive(Default)]
pub(crate) struct CpuBackend {
    pub graph: GraphSnapshot<u32>,
    pub observations: ObservationStore<u32>,
    history: ResourceHistory<CpuSurface>,
}

impl CpuBackend {
    pub fn target(&mut self, id: u32, width: u32, height: u32, score: u64) {
        self.graph.surfaces.insert(id, Extent2D { width, height });
        self.graph.targets.push(Target {
            id,
            score,
            order: self.graph.targets.len() as u64,
        });
    }

    pub fn edge(&mut self, source: u32, destination: u32, seen: u64, exact: bool) {
        let mut edge = DependencyEdge::sampled(source, destination, seen);
        if exact {
            edge.evidence = Evidence::Blit;
            edge.whole_source = true;
        }
        self.graph.edges.entry(destination).or_default().push(edge);
    }

    pub fn selected(&self, limit: usize) -> Vec<u32> {
        PresentationSelector::select(&self.graph, 0, limit, true)
            .surfaces
            .iter()
            .map(|source| source.surface)
            .collect()
    }
}

#[derive(Clone)]
pub(crate) struct CpuSurface {
    pub id: u32,
    allocation: Extent2D,
    subresource: (u32, u32),
    pub selection: SurfaceSelection,
}
impl CpuSurface {
    pub fn new(id: u32, width: u32, height: u32) -> Self {
        let allocation = Extent2D { width, height };
        Self {
            id,
            allocation,
            subresource: (0, 0),
            selection: SurfaceSelection {
                source_rect: allocation.rect(),
                target_extent: allocation,
                ..Default::default()
            },
        }
    }
    pub fn registry_source(&self, logical_slot: usize) -> crate::registry::RegistrySource {
        crate::registry::RegistrySource {
            logical_slot,
            identity: crate::registry::SourceIdentity {
                domain: 0,
                resource: u64::from(self.id),
                subresource: 0,
            },
            extent: self.selection.source_rect.extent,
        }
    }
}
impl HistorySurface for CpuSurface {
    type Id = u32;
    type Layout = (Extent2D, (u32, u32));
    fn identity(&self) -> u32 {
        self.id
    }
    fn layout(&self) -> Self::Layout {
        (self.selection.source_rect.extent, self.subresource)
    }
    fn selection(&mut self) -> &mut SurfaceSelection {
        &mut self.selection
    }
}
impl CpuBackend {
    pub fn set_observations(&mut self, snapshot: GraphSnapshot<u32>) {
        self.graph.targets = snapshot.targets;
        self.graph.edges = snapshot.edges;
    }
    pub fn submit(&self, commands: &[DependencyEdge<u32>]) {
        for (order, edge) in commands.iter().enumerate() {
            self.observations.record(DependencyEdge {
                generation: self.observations.generation(),
                order: order as u64,
                ..*edge
            });
        }
    }
    pub fn present(&mut self, limit: usize, persistent: bool) -> Reconciliation<CpuSurface> {
        self.set_observations(self.observations.present(persistent));
        let graph = &self.graph;
        let result = PresentationSelector::select(graph, 0, limit, persistent);
        let surfaces = result
            .surfaces
            .into_iter()
            .map(|source| CpuSurface {
                id: source.surface,
                allocation: graph.surfaces[&source.surface],
                subresource: graph.subresource(source.surface),
                selection: source.selection,
            })
            .collect();
        self.history.reconcile(surfaces, result.exact, |source| {
            self.graph.surfaces.get(&source.id).is_some_and(|extent| {
                *extent == source.allocation
                    && source.selection.source_rect.intersect(extent.rect())
                        == Some(source.selection.source_rect)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{PresentationRegistry, SourceIdentity};
    use polymer_protocol::StreamId;

    #[test]
    fn submitted_screens_keep_crop_placement_slots_and_frame_metadata_through_replacement() {
        let mut backend = CpuBackend::default();
        let allocation = Extent2D {
            width: 512,
            height: 512,
        };
        backend.graph.surfaces.extend([
            (
                0,
                Extent2D {
                    width: 800,
                    height: 600,
                },
            ),
            (1, allocation),
            (2, allocation),
        ]);
        let crop = Rect {
            x: 8,
            y: 16,
            extent: Extent2D {
                width: 320,
                height: 240,
            },
        };
        let commands = [1, 2].map(|source| DependencyEdge {
            evidence: Evidence::Copy,
            mapping: Some(RegionMapping {
                source: crop,
                destination: Rect {
                    x: -20,
                    y: (source as i32 - 1) * 300,
                    extent: crop.extent,
                },
                flip_x: source == 2,
                flip_y: false,
            }),
            ..DependencyEdge::sampled(source, 0, 0)
        });
        assert_eq!(backend.present(2, true).surfaces[0].id, 0);
        backend.submit(&commands);
        let selected = backend.present(2, true);
        assert_eq!(selected.slots, [0, 1]);
        assert_eq!(
            selected
                .surfaces
                .iter()
                .map(|source| source.id)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        let registry = PresentationRegistry::default();
        registry.observe(
            10,
            100,
            selected.registry_sources(|source| {
                (
                    SourceIdentity {
                        domain: 7,
                        resource: source.id as u64,
                        subresource: 0,
                    },
                    source.selection.source_rect.extent,
                )
            }),
            1,
        );
        let assignments = registry.assign(&[1, 0], 1, |_, _| None);
        let frames = assignments
            .iter()
            .map(|assignment| {
                let source = &selected.surfaces[assignment.index];
                CapturedFrame {
                    stream_id: StreamId(assignment.slot as u64),
                    slot: assignment.slot as u32,
                    extent: source.selection.source_rect.extent,
                    sync_value: 42,
                    timestamp_qpc: 123,
                }
                .present(source.selection.frame_placement())
            })
            .collect::<Vec<_>>();
        assert_eq!(frames.len(), 2);
        assert_eq!(
            (
                frames[0].render_width,
                frames[0].capture_height,
                frames[0].capture_y
            ),
            (300, 240, 300)
        );
        assert_eq!(
            (frames[0].presentation_width, frames[0].presentation_height),
            (800, 600)
        );
        assert_eq!((frames[0].sync_value, frames[0].timestamp_qpc), (42, 123));
        assert_eq!(
            frames[0].presentation_transform,
            FramePlacement::mapped(
                selected.surfaces[1].selection.presentation_rect.unwrap(),
                true,
                false
            )
        );
        backend.graph.surfaces.remove(&1);
        backend.observations.forget(1);
        let remaining = backend.present(2, true);
        assert_eq!(remaining.slots, [1]);
        assert_eq!(remaining.surfaces[0].id, 2);
        backend.graph.surfaces.insert(3, allocation);
        backend.submit(&[DependencyEdge {
            source: 3,
            ..commands[0]
        }]);
        let replaced = backend.present(2, true);
        assert_eq!(replaced.slots, [0, 1]);
        assert!(replaced.changed(0));
        assert!(!replaced.changed(1));
    }

    #[test]
    fn signed_reflected_placement_survives_legacy_unsigned_frame_fields() {
        use polymer_protocol::{PresentationPoint, PresentationTransform};
        let extent = Extent2D {
            width: 640,
            height: 480,
        };
        let bounds = Rect {
            x: -12,
            y: -8,
            extent: Extent2D {
                width: 320,
                height: 240,
            },
        };
        for (flip_x, flip_y, corners) in [
            (false, false, [(-12, -8), (308, -8), (-12, 232)]),
            (true, false, [(308, -8), (-12, -8), (308, 232)]),
            (false, true, [(-12, 232), (308, 232), (-12, -8)]),
        ] {
            let frame = CapturedFrame {
                stream_id: StreamId(1),
                slot: 0,
                extent: bounds.extent,
                sync_value: 1,
                timestamp_qpc: 2,
            }
            .present(FramePlacement::new(extent, Some(bounds), (flip_x, flip_y)));
            assert_eq!((frame.capture_x, frame.capture_y), (0, 0));
            let expected = corners.map(|(x, y)| PresentationPoint::from_pixels(x, y));
            let actual = frame.presentation_transform;
            assert_eq!(
                [actual.upper_left, actual.upper_right, actual.lower_left],
                expected
            );
        }
        assert_eq!(
            FramePlacement::new(extent, None, (false, false)).transform,
            PresentationTransform::UNAVAILABLE
        );
    }

    #[test]
    fn held_placement_expires_and_cannot_survive_a_window_resize() {
        let mut backend = CpuBackend::default();
        let extent = Extent2D {
            width: 320,
            height: 240,
        };
        backend.graph.surfaces.extend([(0, extent), (1, extent)]);
        let known = DependencyEdge {
            evidence: Evidence::Copy,
            mapping: Some(RegionMapping {
                source: extent.rect(),
                destination: extent.rect(),
                flip_x: true,
                flip_y: false,
            }),
            ..DependencyEdge::sampled(1, 0, 0)
        };
        backend.submit(&[known]);
        backend.present(1, false);
        for frame in 1..=241 {
            backend.observations.target(
                Target {
                    id: 1,
                    score: extent.area(),
                    order: 0,
                },
                backend.observations.generation(),
            );
            backend.submit(&[DependencyEdge::sampled(1, 0, 0)]);
            let selected = backend.present(1, false);
            assert_eq!(
                selected.surfaces[0].selection.presentation_rect.is_some(),
                frame <= 240
            );
        }
        backend.submit(&[known]);
        backend.present(1, false);
        backend.graph.surfaces.insert(
            0,
            Extent2D {
                width: 640,
                height: 480,
            },
        );
        backend.observations.target(
            Target {
                id: 1,
                score: extent.area(),
                order: 0,
            },
            backend.observations.generation(),
        );
        backend.submit(&[DependencyEdge::sampled(1, 0, 0)]);
        assert!(
            backend.present(1, false).surfaces[0]
                .selection
                .presentation_rect
                .is_none()
        );
        backend.submit(&[known]);
        backend.present(1, false);
        let resized = Extent2D {
            width: 960,
            height: 720,
        };
        backend.graph.surfaces.insert(0, resized);
        // Hysteresis keeps the old screen while the observed fallback changes.
        let held = backend.present(1, false);
        assert_eq!(held.surfaces[0].id, 1);
        assert_eq!(held.surfaces[0].selection.target_extent, resized);
        assert!(held.surfaces[0].selection.presentation_rect.is_none());
    }
}
