//! Native metadata adapter for the common presentation selector.
use super::{PresentSource, Tracking};
use ash::vk;
use polymer_presentation::PresentationSelector;

impl Tracking {
    pub(super) fn select_presented(
        &self,
        presented_image: vk::Image,
        limit: usize,
    ) -> Vec<PresentSource> {
        let presented = self.surface_id(presented_image, 0, 0);
        let mut graph = self
            .surfaces
            .observations
            .present(polymer_hook::config::graph_tracking_enabled());
        graph.resolve(presented, |id| {
            Some((
                self.surface_extent(id)?,
                (self.surface_id(id.image, 0, 0), id.mip, id.layer),
            ))
        });
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
                    selection: source.selection,
                    image: source.surface.image,
                    allocation_extent: vk::Extent2D {
                        width: extent.width,
                        height: extent.height,
                    },
                    layout,
                })
            })
            .collect()
    }
}
