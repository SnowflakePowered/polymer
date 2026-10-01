use std::error::Error;

use crate::layout::{Extent, Point, Rect};
use polymer_protocol::FrameReady;
use polymer_runtime::SurfaceOffer;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

pub mod frame_ownership;

/// How a surface's pixels are sampled and drawn inside its layout rectangle.
///
/// Coordinates are in output pixels for `destination` and normalized,
/// post-presentation-transform coordinates for `source`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceContent {
    pub destination: [f32; 4],
    pub source: [f32; 4],
    pub opacity: f32,
}

impl SurfaceContent {
    pub fn fill(rectangle: Rect) -> Self {
        Self {
            destination: [
                rectangle.x as f32,
                rectangle.y as f32,
                rectangle.x as f32 + rectangle.width as f32,
                rectangle.y as f32 + rectangle.height as f32,
            ],
            source: [0.0, 0.0, 1.0, 1.0],
            opacity: 1.0,
        }
    }
}

/// Final shell-supplied placement for one received surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfacePlacement {
    pub connection: u64,
    pub surface_id: u64,
    pub rectangle: Rect,
    /// Visual placement and sampling. The layout rectangle above remains
    /// authoritative for producer sizing and the surface's pointer hit box.
    pub content: SurfaceContent,
    /// Radius of the analytic rounded-rectangle clip, in destination pixels.
    pub corner_radius: f32,
}

impl SurfacePlacement {
    pub fn new(connection: u64, surface_id: u64, rectangle: Rect, corner_radius: f32) -> Self {
        Self {
            connection,
            surface_id,
            rectangle,
            content: SurfaceContent::fill(rectangle),
            corner_radius,
        }
    }
}

/// Immutable instructions for one swapchain composition pass.
///
/// Receivers describe available surfaces through [`Presentation`]. The shell
/// resolves those surfaces into this plan; GPU compositors do not perform
/// layout or infer fallback rectangles.
#[derive(Clone, Debug, PartialEq)]
pub struct CompositingPlan {
    clear_color: [f32; 4],
    placements: Vec<SurfacePlacement>,
}

impl CompositingPlan {
    /// A plan of nothing but received streams.
    pub fn new(clear_color: [f32; 4], placements: Vec<SurfacePlacement>) -> Self {
        Self {
            clear_color,
            placements,
        }
    }

    pub const fn clear_color(&self) -> [f32; 4] {
        self.clear_color
    }

    pub fn placements(&self) -> impl Iterator<Item = &SurfacePlacement> {
        self.placements.iter()
    }

    pub fn rectangle(&self, connection: u64, surface_id: u64) -> Option<Rect> {
        self.placements()
            .find(|placement| {
                placement.connection == connection && placement.surface_id == surface_id
            })
            .map(|placement| placement.rectangle)
    }
}

/// Selects which received streams a window swapchain composites.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SwapchainMode {
    /// Composite every imported stream into one swapchain.
    #[default]
    Shared,
    /// Composite only this connection. Create one compositor/window per
    /// connection to give every stream its own swapchain.
    Dedicated(u64),
}

impl SwapchainMode {
    pub const fn accepts(self, connection: u64) -> bool {
        match self {
            Self::Shared => true,
            Self::Dedicated(selected) => selected == connection,
        }
    }
}

pub trait RenderWindow: HasDisplayHandle + HasWindowHandle {
    fn render_extent(&self) -> Extent;
}

impl RenderWindow for winit::window::Window {
    fn render_extent(&self) -> Extent {
        let size = self.inner_size();
        Extent::new(size.width, size.height)
    }
}

/// Metadata for a surface made available by a backend receiver.
#[derive(Clone, Copy, Debug)]
pub struct ReceivedSurface {
    pub connection: u64,
    pub surface_id: u64,
    pub target_window: i64,
    pub image_extent: Extent,
    pub quarter_turns: u8,
    pub flip_y: bool,
    pub frame: FrameReady,
}

impl ReceivedSurface {
    pub fn stream_extent(&self) -> Extent {
        if self.quarter_turns % 2 == 1 {
            Extent::new(self.image_extent.height, self.image_extent.width)
        } else {
            self.image_extent
        }
    }
}

pub fn image_extent(width: u32, height: u32, quarter_turns: u8) -> Extent {
    let (width, height) = (width.max(1), height.max(1));
    if quarter_turns % 2 == 1 {
        Extent::new(height, width)
    } else {
        Extent::new(width, height)
    }
}

#[derive(Clone, Copy)]
pub struct MappedPointer {
    pub connection: u64,
    pub surface_id: u64,
    pub target_window: i64,
    pub frame: FrameReady,
    pub stream_extent: Extent,
    pub position: Point,
    pub normalized_position: Point,
}

/// Snapshot of surfaces received for a particular output extent.
#[derive(Clone, Debug)]
pub struct Presentation {
    viewer_extent: Extent,
    surfaces: Vec<ReceivedSurface>,
}

impl Presentation {
    pub fn new(viewer_extent: Extent, surfaces: Vec<ReceivedSurface>) -> Self {
        Self {
            viewer_extent,
            surfaces,
        }
    }

    pub const fn viewer_extent(&self) -> Extent {
        self.viewer_extent
    }

    pub fn surfaces(&self) -> &[ReceivedSurface] {
        &self.surfaces
    }
}

pub trait Renderer {
    fn import_surface_offer(
        &mut self,
        offer: &SurfaceOffer,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>>;

    fn disconnect_stream(&mut self, connection: u64) -> Result<(), Box<dyn Error>>;
    fn set_latest(&mut self, connection: u64, frame: FrameReady);
    fn received_surfaces(&self) -> Vec<ReceivedSurface>;
    fn has_pending_frame(&self) -> bool;
    fn recreate_swapchain(&mut self, window: &impl RenderWindow) -> Result<bool, Box<dyn Error>>;

    /// Blocks until the backend has presentation capacity for another frame.
    ///
    /// Hosts call this *before* collecting the frames and input a composition
    /// pass is built from, so what reaches the screen is what was available at
    /// the start of the present slot rather than at the start of the previous
    /// one. Backends that already block inside their present path leave this as
    /// the default.
    fn begin_frame(&mut self) {}
    fn present(&mut self, plan: &CompositingPlan) -> Result<bool, Box<dyn Error>>;

    fn received_presentation(&self, viewer_extent: Extent) -> Presentation {
        Presentation::new(viewer_extent, self.received_surfaces())
    }
}
