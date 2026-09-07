//! OpenGL resource/subresource adapter for presentation selection.
use super::{ContextTracking, DrawCandidate, TargetKey};
use polymer_presentation::{Extent2D, PresentationSelector, Surface};

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
            selection: polymer_presentation::SurfaceSelection {
                target_extent: Extent2D { width, height },
                source_rect: Extent2D { width, height }.rect(),
                presentation_rect: Some(Extent2D { width, height }.rect()),
                ..Default::default()
            },
            texture: 0,
            target: 0,
            mip: 0,
            layer: -1,
            incarnation: drawable,
            width,
            height,

            score: 0,
        }
    }
}

impl TargetKey {
    pub(super) const PRESENT: TargetKey = TargetKey {
        renderbuffer: false,
        mip: 0,
        incarnation: 0,
        texture: 0,
        layer: -1,
    };
}

impl ContextTracking {
    pub(super) fn select(
        &self,
        backbuffer: Option<DrawCandidate>,
        limit: usize,
        traverse: bool,
    ) -> (Vec<DrawCandidate>, bool) {
        let mut graph = self.observations.present(traverse);
        graph.resolve(TargetKey::PRESENT, |id| {
            let mut source = *self.resources.get(&id)?;
            if source.target == super::GL_RENDERBUFFER {
                let (width, height, _) = unsafe { self.measure_renderbuffer(source.texture) }?;
                source.width = width;
                source.height = height;
            } else {
                let (target, width, height) = *self.texture_info.get(&source.texture)?;
                source.target = target;
                source.width = width.checked_shr(source.mip as u32).unwrap_or(0).max(1);
                source.height = height.checked_shr(source.mip as u32).unwrap_or(0).max(1);
            }
            Some((
                source,
                (
                    TargetKey {
                        mip: 0,
                        layer: 0,
                        ..id
                    },
                    id.mip as u32,
                    id.layer as u32,
                ),
            ))
        });
        self.resources
            .retain(|id, _| traverse && graph.surfaces.contains_key(id));
        if let Some(backbuffer) = backbuffer {
            graph.surfaces.insert(TargetKey::PRESENT, backbuffer);
        }
        let selection = PresentationSelector::select(&graph, TargetKey::PRESENT, limit, traverse);
        let candidates = selection
            .surfaces
            .into_iter()
            .filter_map(|source| {
                let mut candidate = graph.surfaces.get(&source.surface).copied()?;
                candidate.selection = source.selection;
                // GL observation coordinates are bottom-up. Normalize placement
                // once; the native copy still uses the original source rectangle.
                candidate.selection.presentation_rect =
                    source.selection.presentation_rect.and_then(|mut bounds| {
                        bounds.y = i32::try_from(
                            i64::from(source.selection.target_extent.height)
                                - i64::from(bounds.y)
                                - i64::from(bounds.extent.height),
                        )
                        .ok()?;
                        Some(bounds)
                    });
                Some(candidate)
            })
            .collect();
        (candidates, selection.exact)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polymer_presentation::{DependencyEdge, Evidence, RegionMapping};

    #[test]
    fn selection_preserves_native_subresource_and_normalizes_window_y() {
        let mut source = crate::tracking::tests::candidate(3, 64, 64, 0);
        source.mip = 1;
        source.layer = 2;
        source.incarnation = 9;
        let crop = polymer_presentation::Rect {
            x: 4,
            y: 3,
            extent: Extent2D {
                width: 24,
                height: 32,
            },
        };
        let tracking = ContextTracking::default();
        tracking.resources.insert(source.key(), source);
        tracking
            .texture_info
            .insert(source.texture, (source.target, 128, 128));
        let store = &tracking.observations;
        store.record(DependencyEdge {
            evidence: Evidence::Copy,
            mapping: Some(RegionMapping {
                source: crop,
                destination: polymer_presentation::Rect {
                    x: 5,
                    y: 7,
                    extent: crop.extent,
                },
                flip_x: false,
                flip_y: true,
            }),
            ..DependencyEdge::sampled(source.key(), TargetKey::PRESENT, 1)
        });
        let (selected, exact) =
            tracking.select(Some(DrawCandidate::backbuffer(128, 96, 77)), 1, true);
        assert!(exact);
        assert_eq!(selected[0].key(), source.key());
        assert_eq!((selected[0].width, selected[0].height), (64, 64));
        assert_eq!(selected[0].selection.source_rect, crop);
        let placement = selected[0].selection.presentation_rect.unwrap();
        assert_eq!((placement.x, placement.y), (5, 57));
        assert!(selected[0].selection.flip_y);
    }
}
