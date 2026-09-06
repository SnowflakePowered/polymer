//! Native metadata adapter for the common presentation selector.
use super::{PresentSource, Tracking};
use ash::vk;
use polymer_presentation::{GraphSnapshot, PresentationSelector};

impl Tracking {
    pub(super) fn select_presented(
        &self,
        presented_image: vk::Image,
        limit: usize,
    ) -> Vec<PresentSource> {
        let mut graph = GraphSnapshot::default();
        let (targets, edges) = self.surfaces.observations.snapshot(self.generation());
        let presented = self.surface_id(presented_image, 0, 0);
        graph.targets = targets;
        let ids = graph
            .targets
            .iter()
            .map(|target| target.id)
            .chain(
                edges
                    .iter()
                    .flat_map(|edge| [edge.source, edge.destination]),
            )
            .chain([presented])
            .collect::<std::collections::HashSet<_>>();
        for id in ids {
            if let Some(extent) = self.surface_extent(id) {
                graph.surfaces.insert(id, extent);
                graph
                    .resources
                    .insert(id, (self.surface_id(id.image, 0, 0), id.mip, id.layer));
            }
        }
        for edge in edges {
            graph
                .edges
                .entry(edge.destination)
                .or_insert_with(Vec::new)
                .push(edge);
        }
        let selection = PresentationSelector::select(
            &graph,
            presented,
            limit,
            polymer_hook::config::graph_tracking_enabled(),
        );
        selection
            .surfaces
            .into_iter()
            .filter(|source| source.surface != presented)
            .filter_map(|source| {
                let extent = *graph.surfaces.get(&source.surface)?;
                let layout = *self.surfaces.layouts.get(&source.surface)?;
                if matches!(
                    layout,
                    vk::ImageLayout::UNDEFINED | vk::ImageLayout::PREINITIALIZED
                ) {
                    return None;
                }
                Some(PresentSource {
                    logical_slot: None,
                    subresource: (source.surface.mip, source.surface.layer),
                    incarnation: source.surface.incarnation,
                    orientation: (source.flip_x, source.flip_y),
                    exact_selection: selection.exact,
                    image: source.surface.image,
                    source_offset: vk::Offset2D {
                        x: source.source_rect.x,
                        y: source.source_rect.y,
                    },
                    allocation_extent: vk::Extent2D {
                        width: extent.width,
                        height: extent.height,
                    },
                    extent: vk::Extent2D {
                        width: source.source_rect.extent.width,
                        height: source.source_rect.extent.height,
                    },
                    layout,
                    presentation_rect: source.presentation_rect.map(|rect| vk::Rect2D {
                        offset: vk::Offset2D {
                            x: rect.x,
                            y: rect.y,
                        },
                        extent: vk::Extent2D {
                            width: rect.extent.width,
                            height: rect.extent.height,
                        },
                    }),
                })
            })
            .collect()
    }
}
