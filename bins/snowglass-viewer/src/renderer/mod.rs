use std::error::Error;

use snowglass_input::mouse::layout::{Extent, Point};
use snowglass_ipc::StreamReady;
use snowglass_protocol::FrameReady;
use winit::window::Window;

use crate::{input::pointer::DisplayedSurface, webrtc};

pub(crate) mod d3d12;
pub(crate) mod vulkan;

pub(crate) use d3d12::D3d12Compositor;
pub(crate) use vulkan::VulkanCompositor;

#[derive(Clone, Debug)]
pub(crate) struct Presentation {
    viewer_extent: Extent,
    surfaces: Vec<DisplayedSurface>,
}

impl Presentation {
    pub(crate) fn new(viewer_extent: Extent, surfaces: Vec<DisplayedSurface>) -> Self {
        Self {
            viewer_extent,
            surfaces,
        }
    }

    pub(crate) const fn viewer_extent(&self) -> Extent {
        self.viewer_extent
    }

    pub(crate) fn surfaces(&self) -> &[DisplayedSurface] {
        &self.surfaces
    }

    pub(crate) fn map_pointer(
        &self,
        point: Point,
        captured_connection: Option<u64>,
    ) -> Option<crate::input::pointer::MappedPointer> {
        crate::input::pointer::map_pointer(self, point, captured_connection)
    }
}

pub(crate) trait Renderer {
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
    fn recreate_swapchain(&mut self, window: &Window) -> Result<bool, Box<dyn Error>>;
    fn draw(
        &mut self,
        output: &egui::FullOutput,
        primitives: &[egui::ClippedPrimitive],
    ) -> Result<bool, Box<dyn Error>>;
    fn take_video_frames(&mut self) -> Vec<webrtc::VideoFrame>;

    fn presentation(&self, viewer_extent: Extent) -> Presentation {
        Presentation::new(viewer_extent, self.displayed_surfaces())
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

    fn recreate_swapchain(&mut self, window: &Window) -> Result<bool, Box<dyn Error>> {
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

    fn recreate_swapchain(&mut self, window: &Window) -> Result<bool, Box<dyn Error>> {
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
