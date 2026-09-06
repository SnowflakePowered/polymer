//! OpenGL resource/subresource adapter for presentation selection.
use super::{DrawCandidate, TargetKey};
use polymer_presentation::{
    DependencyEdge, Extent2D, GraphSnapshot, PresentationSelector, Surface, Target,
};

impl Surface for DrawCandidate {
    fn extent(&self) -> Option<Extent2D> {
        Some(Extent2D {
            width: self.width,
            height: self.height,
        })
    }
}

impl DrawCandidate {
    pub(crate) fn backbuffer(width: u32, height: u32, drawable: u64) -> Self {
        Self {
            texture: 0,
            target: 0,
            mip: 0,
            layer: -1,
            incarnation: drawable,
            width,
            height,
            allocation_extent: Some((width, height)),
            source_origin: (0, 0),
            orientation: (false, false),
            score: 0,
            bounds: super::QuadBounds {
                x: 0,
                y: 0,
                width,
                height,
            },
            window_bounds: true,
        }
    }
}

pub(super) struct SelectionView {
    graph: GraphSnapshot<TargetKey, DrawCandidate>,
}

impl SelectionView {
    pub(super) const PRESENT: TargetKey = TargetKey {
        renderbuffer: false,
        mip: 0,
        incarnation: 0,
        texture: 0,
        layer: -1,
    };

    pub(super) fn new(candidates: &[DrawCandidate], targets: Vec<Target<TargetKey>>) -> Self {
        let mut graph = GraphSnapshot::<TargetKey, DrawCandidate> {
            targets,
            ..GraphSnapshot::default()
        };
        for &candidate in candidates {
            let id = candidate.key();
            if let Some(old) = graph.surfaces.get(&id)
                && Surface::extent(old).is_some()
                && old.score >= candidate.score
            {
                continue;
            }
            graph.surfaces.insert(id, candidate);
            graph.resources.insert(
                id,
                (
                    TargetKey {
                        renderbuffer: id.renderbuffer,
                        mip: 0,
                        incarnation: id.incarnation,
                        texture: id.texture,
                        layer: 0,
                    },
                    id.mip as u32,
                    id.layer as u32,
                ),
            );
        }
        Self { graph }
    }

    pub(super) fn select(
        mut self,
        limit: usize,
        mappings: Vec<DependencyEdge<TargetKey>>,
        traverse: bool,
    ) -> (Vec<DrawCandidate>, bool) {
        for edge in mappings {
            self.graph
                .edges
                .entry(edge.destination)
                .or_default()
                .push(edge);
        }
        let selection = PresentationSelector::select(&self.graph, Self::PRESENT, limit, traverse);
        let candidates = selection
            .surfaces
            .into_iter()
            .filter_map(|source| {
                let mut candidate = self.graph.surfaces.get(&source.surface).copied()?;
                candidate.window_bounds = false;
                candidate.allocation_extent = Some((candidate.width, candidate.height));
                candidate.orientation = (source.flip_x, source.flip_y);
                candidate.source_origin = (source.source_rect.x, source.source_rect.y);
                candidate.width = source.source_rect.extent.width;
                candidate.height = source.source_rect.extent.height;
                if let Some(placement) = source.presentation_rect {
                    candidate.bounds = super::QuadBounds {
                        x: placement.x,
                        y: placement.y,
                        width: placement.extent.width,
                        height: placement.extent.height,
                    };
                    candidate.window_bounds = true;
                }
                Some(candidate)
            })
            .collect();
        (candidates, selection.exact)
    }

    pub(super) fn with_backbuffer(mut self, backbuffer: Option<DrawCandidate>) -> Self {
        if let Some(backbuffer) = backbuffer {
            self.graph.surfaces.insert(Self::PRESENT, backbuffer);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polymer_presentation::Evidence;

    #[test]
    fn default_framebuffer_uses_observed_crop_and_preserves_drawable_identity() {
        let backbuffer = DrawCandidate::backbuffer(128, 96, 77);
        let crop = polymer_presentation::Rect {
            x: 8,
            y: 12,
            extent: Extent2D {
                width: 64,
                height: 48,
            },
        };
        let mut raster = DependencyEdge::sampled(SelectionView::PRESENT, SelectionView::PRESENT, 1);
        raster.coverage = Some(crop);
        for traverse in [false, true] {
            let (selected, exact) = SelectionView::new(&[], Vec::new())
                .with_backbuffer(Some(backbuffer))
                .select(1, vec![raster], traverse);
            assert!(!exact);
            assert_eq!(selected.len(), 1);
            assert_eq!(
                (
                    selected[0].texture,
                    selected[0].target,
                    selected[0].incarnation
                ),
                (0, 0, 77)
            );
            assert_eq!(selected[0].source_origin, (8, 12));
            assert_eq!((selected[0].width, selected[0].height), (64, 48));
            assert_eq!(selected[0].allocation_extent, Some((128, 96)));
            assert!(selected[0].window_bounds);
        }
    }

    #[test]
    fn shared_target_scores_and_order_override_cached_metadata_ranking() {
        let first = crate::tracking::tests::candidate(1, 320, 240, 10_000);
        let second = crate::tracking::tests::candidate(2, 640, 480, 1);
        let targets = vec![
            Target {
                id: first.key(),
                score: 1,
                order: 9,
            },
            Target {
                id: second.key(),
                score: 100,
                order: 2,
            },
        ];
        let edges = [first, second]
            .map(|source| DependencyEdge::sampled(source.key(), SelectionView::PRESENT, 10));
        let (selected, _) =
            SelectionView::new(&[first, second], targets.clone()).select(1, edges.to_vec(), true);
        assert_eq!(
            selected
                .iter()
                .map(|source| source.texture)
                .collect::<Vec<_>>(),
            [2]
        );
        let second = DrawCandidate {
            width: first.width,
            height: first.height,
            ..second
        };
        let (selected, _) =
            SelectionView::new(&[first, second], targets).select(2, edges.to_vec(), true);
        assert_eq!(
            selected
                .iter()
                .map(|source| source.texture)
                .collect::<Vec<_>>(),
            [2, 1]
        );
    }

    #[test]
    fn renderbuffer_composite_preserves_source_crops_and_separate_texture_namespace() {
        use polymer_presentation::RegionMapping;
        let left = crate::tracking::tests::candidate(3, 64, 64, 0);
        let right = crate::tracking::tests::candidate(4, 64, 64, 0);
        let mut buffer = crate::tracking::tests::candidate(3, 56, 48, 0);
        buffer.target = super::super::GL_RENDERBUFFER;
        assert_ne!(left.key(), buffer.key());
        let mut edges = Vec::new();
        for (source, width, height, x) in [(left, 32, 48, 0), (right, 24, 32, 32)] {
            let mut edge = DependencyEdge::sampled(source.key(), buffer.key(), 1);
            edge.evidence = Evidence::Copy;
            edge.mapping = Some(RegionMapping {
                source: polymer_presentation::Rect {
                    x: 4,
                    y: 8,
                    extent: Extent2D { width, height },
                },
                destination: polymer_presentation::Rect {
                    x,
                    y: 0,
                    extent: Extent2D { width, height },
                },
                flip_x: false,
                flip_y: false,
            });
            edge.coverage = edge.mapping.map(|mapping| mapping.destination);
            edges.push(edge);
        }
        let mut present = DependencyEdge::sampled(buffer.key(), SelectionView::PRESENT, 1);
        present.evidence = Evidence::Blit;
        present.mapping = Some(RegionMapping {
            source: Extent2D {
                width: 56,
                height: 48,
            }
            .rect(),
            destination: Extent2D {
                width: 112,
                height: 96,
            }
            .rect(),
            flip_x: false,
            flip_y: true,
        });
        present.coverage = present.mapping.map(|mapping| mapping.destination);
        edges.push(present);
        let (selected, exact) =
            SelectionView::new(&[left, right, buffer], Vec::new()).select(2, edges, true);
        assert!(exact);
        assert_eq!(selected.len(), 2);
        assert_eq!(
            selected
                .iter()
                .map(|source| source.key())
                .collect::<Vec<_>>(),
            [left.key(), right.key()]
        );
        assert_eq!((selected[0].width, selected[0].height), (32, 48));
        assert_eq!((selected[1].width, selected[1].height), (24, 32));
        assert!(selected.iter().all(|source| source.source_origin == (4, 8)
            && source.orientation == (false, true)
            && source.window_bounds));
    }

    #[test]
    fn mapped_mip_crop_retains_native_subresource_and_orientation() {
        let candidate = DrawCandidate {
            mip: 1,
            incarnation: 9,
            texture: 3,
            target: super::super::GL_TEXTURE_2D_ARRAY,
            layer: 2,
            width: 64,
            height: 64,
            bounds: super::super::QuadBounds {
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            },
            window_bounds: false,
            score: 0,
            orientation: (false, false),
            source_origin: (0, 0),
            allocation_extent: None,
        };
        let mut edge = DependencyEdge::sampled(candidate.key(), SelectionView::PRESENT, 1);
        for traverse in [false, true] {
            let targets = vec![Target {
                id: candidate.key(),
                score: 100,
                order: 7,
            }];
            let (unpresented, _) =
                SelectionView::new(&[candidate], targets.clone()).select(1, Vec::new(), traverse);
            assert!(
                unpresented.is_empty(),
                "an unrelated render target became a screen"
            );
            let (presented, _) =
                SelectionView::new(&[candidate], targets).select(1, vec![edge], traverse);
            assert_eq!(presented.len(), 1);
            assert_eq!(presented[0].key(), candidate.key());
            let (sampled, _) =
                SelectionView::new(&[candidate], Vec::new()).select(1, vec![edge], traverse);
            assert!(
                sampled.is_empty(),
                "an uploaded sampled asset was treated as rendered content"
            );
        }
        edge.evidence = Evidence::Copy;
        let crop = polymer_presentation::Rect {
            x: 4,
            y: 3,
            extent: Extent2D {
                width: 24,
                height: 32,
            },
        };
        edge.mapping = Some(polymer_presentation::RegionMapping {
            source: crop,
            destination: polymer_presentation::Rect {
                x: -5,
                y: -7,
                extent: crop.extent,
            },
            flip_x: false,
            flip_y: true,
        });
        let (selected, exact) =
            SelectionView::new(&[candidate], Vec::new()).select(1, vec![edge], true);
        assert!(exact);
        assert_eq!(selected.len(), 1);
        assert_eq!(
            (selected[0].mip, selected[0].layer, selected[0].incarnation),
            (1, 2, 9)
        );
        assert_eq!(selected[0].source_origin, (4, 3));
        assert_eq!((selected[0].width, selected[0].height), (24, 32));
        assert_eq!(selected[0].orientation, (false, true));
        assert_eq!((selected[0].bounds.x, selected[0].bounds.y), (-5, -7));
    }
}
