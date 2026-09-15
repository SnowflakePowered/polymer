use crate::{Extent2D, Rect};
use polymer_protocol::{FrameReady, PresentationTransform, StreamId};

pub struct CapturedFrame {
    pub stream_id: StreamId,
    pub slot: u32,
    pub extent: Extent2D,
    pub sync_value: u64,
    /// Captured at submission/readback, never replaced with publication time.
    pub timestamp_qpc: u64,
}

pub struct FramePlacement {
    pub extent: Extent2D,
    pub bounds: Rect,
    pub transform: PresentationTransform,
}

impl CapturedFrame {
    pub fn present(self, placement: FramePlacement) -> FrameReady {
        FrameReady {
            stream_id: self.stream_id,
            slot: self.slot,
            render_width: self.extent.width,
            render_height: self.extent.height,
            capture_x: placement.bounds.x.max(0) as u32,
            capture_y: placement.bounds.y.max(0) as u32,
            capture_width: placement.bounds.extent.width,
            capture_height: placement.bounds.extent.height,
            presentation_width: placement.extent.width,
            presentation_height: placement.extent.height,
            presentation_transform: placement.transform,
            frame_number: 0, // Assigned only when ProducerSession publishes it.
            sync_value: self.sync_value,
            timestamp_qpc: self.timestamp_qpc,
        }
    }
}

impl FramePlacement {
    pub fn new(extent: Extent2D, bounds: Option<Rect>, orientation: (bool, bool)) -> Self {
        Self {
            extent,
            bounds: bounds.unwrap_or(extent.rect()),
            transform: bounds.map_or(PresentationTransform::UNAVAILABLE, |bounds| {
                Self::mapped(bounds, orientation.0, orientation.1)
            }),
        }
    }

    /// Converts a normalized rectangle and reflections without losing signed origins.
    pub fn mapped(bounds: Rect, flip_x: bool, flip_y: bool) -> PresentationTransform {
        if !flip_x && !flip_y && bounds.x >= 0 && bounds.y >= 0 {
            return PresentationTransform::axis_aligned(
                bounds.x as u32,
                bounds.y as u32,
                bounds.extent.width,
                bounds.extent.height,
            );
        }
        let point = |x: i64, y: i64| polymer_protocol::PresentationPoint {
            x: x << polymer_protocol::PRESENTATION_COORD_FRACTIONAL_BITS,
            y: y << polymer_protocol::PRESENTATION_COORD_FRACTIONAL_BITS,
        };
        let (left, right) = (
            i64::from(bounds.x),
            i64::from(bounds.x) + i64::from(bounds.extent.width),
        );
        let (top, bottom) = (
            i64::from(bounds.y),
            i64::from(bounds.y) + i64::from(bounds.extent.height),
        );
        let (x0, x1) = if flip_x { (right, left) } else { (left, right) };
        let (y0, y1) = if flip_y { (bottom, top) } else { (top, bottom) };
        PresentationTransform::affine(point(x0, y0), point(x1, y0), point(x0, y1))
    }
}
