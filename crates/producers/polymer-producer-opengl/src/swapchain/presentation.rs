use crate::tracking::{DrawCandidate, QuadBounds};
use polymer_presentation::{Extent2D, Rect, producer::FramePlacement};
use polymer_protocol::PresentationTransform;

impl DrawCandidate {
    /// Normalize OpenGL's bottom-up window coordinates once for every transport.
    /// Offscreen observations have no established window placement.
    pub(super) fn frame_placement(source: Option<&Self>, extent: Extent2D) -> FramePlacement {
        let mut placement = FramePlacement {
            extent,
            bounds: extent.rect(),
            transform: PresentationTransform::UNAVAILABLE,
        };
        let Some(source) = source else {
            placement.transform = FramePlacement::mapped(placement.bounds, false, false);
            return placement;
        };
        if !source.window_bounds {
            return placement;
        }
        if let Some(bounds) = source.bounds.presentation_rect(extent) {
            placement.bounds = bounds;
            placement.transform = FramePlacement::mapped(
                placement.bounds,
                source.orientation.0,
                source.orientation.1,
            );
        }
        placement
    }
}

impl QuadBounds {
    pub(super) fn presentation_rect(self, window: Extent2D) -> Option<Rect> {
        Some(Rect {
            x: self.x,
            y: i32::try_from(i64::from(window.height) - i64::from(self.y) - i64::from(self.height))
                .ok()?,
            extent: Extent2D {
                width: self.width,
                height: self.height,
            },
        })
    }

    pub(super) fn from_presentation_rect(bounds: Rect, window: Extent2D) -> Option<Self> {
        Some(Self {
            x: bounds.x,
            y: i32::try_from(
                i64::from(window.height) - i64::from(bounds.y) - i64::from(bounds.extent.height),
            )
            .ok()?,
            width: bounds.extent.width,
            height: bounds.extent.height,
        })
    }
}
