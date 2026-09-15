use std::error::Error;

use crate::layout::{Extent, Point, Rect};
use polymer_protocol::{FrameReady, PresentationMappingKind, ResizeWindow};
use polymer_runtime::SurfaceOffer;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

pub mod d3d12;
mod frame_ownership;
pub mod vulkan;
pub mod webrtc;

pub use d3d12::{D3d12Compositor, D3d12RenderPass, RasterSurface};
pub use vulkan::{VulkanCompositor, VulkanRenderPass};
pub use webrtc::{VideoSwapchain, WebRtcCompositor};

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

    /// Computes guest-window resize requests against this host window's
    /// current drawable extent.
    fn resize_windows(
        &self,
        surfaces: &[ReceivedSurface],
        plan: &CompositingPlan,
    ) -> Vec<ResizeWindow> {
        Presentation::new(self.render_extent(), surfaces.to_vec()).resize_windows(plan)
    }
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

/// Returns the visual extent of a received image. A pre-composition capture
/// can have a storage aspect ratio that differs from the quad that displayed
/// it (for example, emulated pixels with a non-square pixel aspect ratio), so
/// prefer the graph-provided presentation bounds when they are available.
pub(crate) fn frame_image_extent(
    frame: FrameReady,
    imported_width: u32,
    imported_height: u32,
    quarter_turns: u8,
) -> Extent {
    let (stored_width, stored_height) = crate::fit_inside(
        frame.render_width.max(1),
        frame.render_height.max(1),
        imported_width,
        imported_height,
    );
    let (width, height) = if frame.presentation_transform.kind
        != PresentationMappingKind::Unavailable
        && frame.capture_width > 0
        && frame.capture_height > 0
    {
        (frame.capture_width, frame.capture_height)
    } else {
        (stored_width, stored_height)
    };
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

    /// Returns the producer client extents which fill the current presentation.
    pub fn resize_windows(&self, plan: &CompositingPlan) -> Vec<ResizeWindow> {
        if self.viewer_extent.is_empty() || self.surfaces.is_empty() {
            return Vec::new();
        }
        self.surfaces
            .iter()
            .filter_map(|surface| {
                let rectangle = plan.rectangle(surface.connection, surface.surface_id)?;
                (surface.target_window != 0 && !rectangle.is_empty())
                    .then_some((surface, rectangle))
            })
            .map(|(surface, rectangle)| {
                let (width, height) = if surface.quarter_turns % 2 == 1 {
                    (rectangle.height, rectangle.width)
                } else {
                    (rectangle.width, rectangle.height)
                };
                ResizeWindow {
                    target_window: surface.target_window,
                    width,
                    height,
                }
            })
            .collect()
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

    fn resize_windows(
        &self,
        window: &impl RenderWindow,
        plan: &CompositingPlan,
    ) -> Vec<ResizeWindow> {
        window.resize_windows(&self.received_surfaces(), plan)
    }
}

impl Renderer for D3d12Compositor {
    fn import_surface_offer(
        &mut self,
        offer: &SurfaceOffer,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        D3d12Compositor::import_stream(self, offer.connection_id(), offer.stream(), quarter_turns)?;
        self.set_buffer_release_sender(offer);
        Ok(())
    }

    fn disconnect_stream(&mut self, connection: u64) -> Result<(), Box<dyn Error>> {
        D3d12Compositor::disconnect_stream(self, connection)
    }

    fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        D3d12Compositor::set_latest(self, connection, frame);
    }

    fn received_surfaces(&self) -> Vec<ReceivedSurface> {
        D3d12Compositor::received_surfaces(self)
    }

    fn has_pending_frame(&self) -> bool {
        D3d12Compositor::has_pending_frame(self)
    }

    fn recreate_swapchain(&mut self, window: &impl RenderWindow) -> Result<bool, Box<dyn Error>> {
        self.resize(window)
    }

    fn begin_frame(&mut self) {
        D3d12Compositor::begin_frame(self);
    }

    fn present(&mut self, plan: &CompositingPlan) -> Result<bool, Box<dyn Error>> {
        D3d12Compositor::present(self, plan)
    }
}

impl Renderer for VulkanCompositor {
    fn import_surface_offer(
        &mut self,
        offer: &SurfaceOffer,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        VulkanCompositor::import_stream(
            self,
            offer.connection_id(),
            offer.stream(),
            quarter_turns,
        )?;
        self.set_buffer_release_sender(offer);
        Ok(())
    }

    fn disconnect_stream(&mut self, connection: u64) -> Result<(), Box<dyn Error>> {
        VulkanCompositor::disconnect_stream(self, connection).map_err(Into::into)
    }

    fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        VulkanCompositor::set_latest(self, connection, frame);
    }

    fn received_surfaces(&self) -> Vec<ReceivedSurface> {
        VulkanCompositor::received_surfaces(self)
    }

    fn has_pending_frame(&self) -> bool {
        VulkanCompositor::has_pending_frame(self)
    }

    fn recreate_swapchain(&mut self, window: &impl RenderWindow) -> Result<bool, Box<dyn Error>> {
        VulkanCompositor::recreate_swapchain(self, window)
    }

    fn present(&mut self, plan: &CompositingPlan) -> Result<bool, Box<dyn Error>> {
        VulkanCompositor::present(self, plan)
    }
}
