use std::error::Error;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use snowglass_input::mouse::layout::{Extent, Point, Rect};
use snowglass_ipc::StreamReady;
use snowglass_protocol::{FrameReady, ResizeWindow};

pub mod d3d12;
pub mod vulkan;
pub mod webrtc;

pub use d3d12::D3d12Compositor;
pub use vulkan::VulkanCompositor;
pub use webrtc::{VideoSwapchain, WebRtcCompositor};

/// Final shell-supplied placement for one received surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfacePlacement {
    pub connection: u64,
    pub surface_id: u64,
    pub rectangle: Rect,
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
    pub fn new(clear_color: [f32; 4], placements: Vec<SurfacePlacement>) -> Self {
        Self {
            clear_color,
            placements,
        }
    }

    pub const fn clear_color(&self) -> [f32; 4] {
        self.clear_color
    }

    pub fn placements(&self) -> &[SurfacePlacement] {
        &self.placements
    }

    pub fn rectangle(&self, connection: u64, surface_id: u64) -> Option<Rect> {
        self.placements
            .iter()
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
    fn import_stream(
        &mut self,
        connection: u64,
        stream: &StreamReady,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>>;

    fn disconnect_stream(&mut self, connection: u64) -> Result<(), Box<dyn Error>>;
    fn set_latest(&mut self, connection: u64, frame: FrameReady);
    fn received_surfaces(&self) -> Vec<ReceivedSurface>;
    fn has_pending_frame(&self) -> bool;
    fn recreate_swapchain(&mut self, window: &impl RenderWindow) -> Result<bool, Box<dyn Error>>;
    fn draw(
        &mut self,
        output: &egui::FullOutput,
        primitives: &[egui::ClippedPrimitive],
        plan: &CompositingPlan,
    ) -> Result<bool, Box<dyn Error>>;
    fn draw_game(&mut self, plan: &CompositingPlan) -> Result<bool, Box<dyn Error>> {
        let context = egui::Context::default();
        let output = context.run(egui::RawInput::default(), |_| {});
        let primitives = context.tessellate(output.shapes.clone(), output.pixels_per_point);
        self.draw(&output, &primitives, plan)
    }

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
    fn import_stream(
        &mut self,
        connection: u64,
        stream: &StreamReady,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        D3d12Compositor::import_stream(self, connection, stream, quarter_turns)
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

    fn draw(
        &mut self,
        output: &egui::FullOutput,
        primitives: &[egui::ClippedPrimitive],
        plan: &CompositingPlan,
    ) -> Result<bool, Box<dyn Error>> {
        D3d12Compositor::draw(self, output, primitives, plan)
    }
}

impl Renderer for VulkanCompositor {
    fn import_stream(
        &mut self,
        connection: u64,
        stream: &StreamReady,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        VulkanCompositor::import_stream(self, connection, stream, quarter_turns)
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

    fn draw(
        &mut self,
        output: &egui::FullOutput,
        primitives: &[egui::ClippedPrimitive],
        plan: &CompositingPlan,
    ) -> Result<bool, Box<dyn Error>> {
        VulkanCompositor::draw(self, output, primitives, plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snowglass_protocol::{PresentationTransform, StreamId};

    fn surface(quarter_turns: u8) -> ReceivedSurface {
        ReceivedSurface {
            connection: 1,
            surface_id: 2,
            target_window: 3,
            image_extent: if quarter_turns % 2 == 1 {
                Extent::new(360, 640)
            } else {
                Extent::new(640, 360)
            },
            quarter_turns,
            flip_y: false,
            frame: FrameReady {
                stream_id: StreamId(4),
                slot: 0,
                render_width: 640,
                render_height: 360,
                capture_x: 0,
                capture_y: 0,
                capture_width: 640,
                capture_height: 360,
                presentation_width: 640,
                presentation_height: 360,
                presentation_transform: PresentationTransform::axis_aligned(0, 0, 640, 360),
                frame_number: 1,
                sync_value: 1,
                timestamp_qpc: 1,
            },
        }
    }

    #[test]
    fn resize_windows_fits_the_active_presentation() {
        let plan = CompositingPlan::new(
            [0.0; 4],
            vec![SurfacePlacement {
                connection: 1,
                surface_id: 2,
                rectangle: Rect::new(0, 0, 1280, 720),
            }],
        );
        let messages =
            Presentation::new(Extent::new(1280, 720), vec![surface(0)]).resize_windows(&plan);
        assert_eq!(
            messages,
            [ResizeWindow {
                target_window: 3,
                width: 1280,
                height: 720,
            }]
        );
    }

    #[test]
    fn resize_windows_unrotates_guest_dimensions() {
        let plan = CompositingPlan::new(
            [0.0; 4],
            vec![SurfacePlacement {
                connection: 1,
                surface_id: 2,
                rectangle: Rect::new(0, 0, 360, 640),
            }],
        );
        let messages =
            Presentation::new(Extent::new(360, 640), vec![surface(1)]).resize_windows(&plan);
        assert_eq!((messages[0].width, messages[0].height), (640, 360));
    }

    #[test]
    fn dedicated_swapchains_accept_only_their_stream() {
        let mode = SwapchainMode::Dedicated(7);
        assert!(mode.accepts(7));
        assert!(!mode.accepts(8));
        assert!(SwapchainMode::Shared.accepts(8));
    }

    #[test]
    fn composition_plans_match_surfaces_by_identity() {
        let plan = CompositingPlan::new(
            [0.25, 0.5, 0.75, 1.0],
            vec![SurfacePlacement {
                connection: 7,
                surface_id: 9,
                rectangle: Rect::new(10, 20, 30, 40),
            }],
        );
        assert_eq!(plan.rectangle(7, 9), Some(Rect::new(10, 20, 30, 40)));
        assert_eq!(plan.rectangle(9, 7), None);
        assert_eq!(plan.clear_color(), [0.25, 0.5, 0.75, 1.0]);
    }
}
