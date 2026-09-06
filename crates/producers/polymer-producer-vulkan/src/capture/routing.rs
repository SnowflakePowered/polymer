use super::{GraphPresentation, PresentationTarget, tracking};
use ash::vk;
use polymer_presentation::{
    Extent2D, Rect,
    registry::{RoutingSource, SourceIdentity, WindowPresentation},
};

impl tracking::PresentSource {
    pub(super) fn routing_source(&self, domain: u64) -> RoutingSource {
        RoutingSource {
            identity: SourceIdentity {
                domain,
                resource: self.incarnation,
                subresource: self.subresource.0 | (self.subresource.1 << 8),
            },
            logical_slot: self.logical_slot,
            extent: Extent2D {
                width: self.extent.width,
                height: self.extent.height,
            },
            placement: self.presentation_rect.map(|rect| Rect {
                x: rect.offset.x,
                y: rect.offset.y,
                extent: Extent2D {
                    width: rect.extent.width,
                    height: rect.extent.height,
                },
            }),
        }
    }
}

impl GraphPresentation {
    pub(super) fn observation(&self) -> WindowPresentation {
        WindowPresentation {
            window: self.source_window,
            extent: Extent2D {
                width: self.presentation_extent.width,
                height: self.presentation_extent.height,
            },
            creation_order: self.creation_order,
            window_order: self.window_order,
            seen: self.last_present,
            sources: self
                .sources
                .iter()
                .map(|source| source.routing_source(self.domain))
                .collect(),
        }
    }
}

impl From<polymer_presentation::registry::PresentationTarget> for PresentationTarget {
    fn from(target: polymer_presentation::registry::PresentationTarget) -> Self {
        Self {
            source_window: target.window,
            presentation_extent: vk::Extent2D {
                width: target.extent.width,
                height: target.extent.height,
            },
            presentation_rect: target.placement.map(|rect| vk::Rect2D {
                offset: vk::Offset2D {
                    x: rect.x,
                    y: rect.y,
                },
                extent: vk::Extent2D {
                    width: rect.extent.width,
                    height: rect.extent.height,
                },
            }),
        }
    }
}
