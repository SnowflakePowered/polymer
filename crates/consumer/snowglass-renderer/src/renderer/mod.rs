use std::error::Error;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use snowglass_input::mouse::layout::{Extent, Point};
use snowglass_ipc::StreamReady;
use snowglass_protocol::{FrameReady, ResizeWindow};

use crate::webrtc;

pub mod d3d12;
pub mod vulkan;

pub use d3d12::D3d12Compositor;
pub use vulkan::VulkanCompositor;

pub trait RenderWindow: HasDisplayHandle + HasWindowHandle {
    fn render_extent(&self) -> Extent;

    /// Computes guest-window resize requests against this host window's
    /// current drawable extent.
    fn resize_windows(&self, surfaces: &[DisplayedSurface]) -> Vec<ResizeWindow> {
        Presentation::new(self.render_extent(), surfaces.to_vec()).resize_windows()
    }
}

impl RenderWindow for winit::window::Window {
    fn render_extent(&self) -> Extent {
        let size = self.inner_size();
        Extent::new(size.width, size.height)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DisplayedSurface {
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

#[derive(Clone, Debug)]
pub struct Presentation {
    viewer_extent: Extent,
    surfaces: Vec<DisplayedSurface>,
}

impl Presentation {
    pub fn new(viewer_extent: Extent, surfaces: Vec<DisplayedSurface>) -> Self {
        Self {
            viewer_extent,
            surfaces,
        }
    }

    pub const fn viewer_extent(&self) -> Extent {
        self.viewer_extent
    }

    pub fn surfaces(&self) -> &[DisplayedSurface] {
        &self.surfaces
    }

    /// Returns the producer client extents which fill the current presentation.
    pub fn resize_windows(&self) -> Vec<ResizeWindow> {
        if self.viewer_extent.is_empty() || self.surfaces.is_empty() {
            return Vec::new();
        }
        let images = self
            .surfaces
            .iter()
            .map(|surface| surface.image_extent)
            .collect::<Vec<_>>();
        let rectangles = crate::layout::active_rectangles(self.viewer_extent, &images, false)
            .unwrap_or_else(|| {
                let cell_height =
                    self.viewer_extent.height / u32::try_from(images.len()).unwrap_or(u32::MAX);
                images
                    .iter()
                    .enumerate()
                    .map(|(index, image)| {
                        let (x, y, width, height) = crate::fit_extent(
                            image.width,
                            image.height,
                            self.viewer_extent.width,
                            cell_height,
                        );
                        snowglass_input::mouse::layout::Rect::new(
                            x,
                            y + i32::try_from(index as u32 * cell_height).unwrap_or(i32::MAX),
                            width.max(0) as u32,
                            height.max(0) as u32,
                        )
                    })
                    .collect()
            });
        self.surfaces
            .iter()
            .zip(rectangles)
            .filter(|(surface, rectangle)| surface.target_window != 0 && !rectangle.is_empty())
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
    fn displayed_surfaces(&self) -> Vec<DisplayedSurface>;
    fn has_pending_frame(&self) -> bool;
    fn recreate_swapchain(&mut self, window: &impl RenderWindow) -> Result<bool, Box<dyn Error>>;
    fn draw(
        &mut self,
        output: &egui::FullOutput,
        primitives: &[egui::ClippedPrimitive],
    ) -> Result<bool, Box<dyn Error>>;
    fn take_video_frames(&mut self) -> Vec<webrtc::VideoFrame>;

    fn presentation(&self, viewer_extent: Extent) -> Presentation {
        Presentation::new(viewer_extent, self.displayed_surfaces())
    }

    fn resize_windows(&self, window: &impl RenderWindow) -> Vec<ResizeWindow> {
        window.resize_windows(&self.displayed_surfaces())
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

    fn displayed_surfaces(&self) -> Vec<DisplayedSurface> {
        D3d12Compositor::displayed_surfaces(self)
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
    ) -> Result<bool, Box<dyn Error>> {
        D3d12Compositor::draw(self, output, primitives)
    }

    fn take_video_frames(&mut self) -> Vec<webrtc::VideoFrame> {
        D3d12Compositor::take_video_frames(self)
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

    fn displayed_surfaces(&self) -> Vec<DisplayedSurface> {
        VulkanCompositor::displayed_surfaces(self)
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
    ) -> Result<bool, Box<dyn Error>> {
        VulkanCompositor::draw(self, output, primitives)
    }

    fn take_video_frames(&mut self) -> Vec<webrtc::VideoFrame> {
        VulkanCompositor::take_video_frames(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snowglass_protocol::{PresentationTransform, StreamId};

    fn surface(quarter_turns: u8) -> DisplayedSurface {
        DisplayedSurface {
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
        crate::layout::configure(None).unwrap();
        let messages = Presentation::new(Extent::new(1280, 720), vec![surface(0)]).resize_windows();
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
        crate::layout::configure(None).unwrap();
        let messages = Presentation::new(Extent::new(360, 640), vec![surface(1)]).resize_windows();
        assert_eq!((messages[0].width, messages[0].height), (640, 360));
    }
}
