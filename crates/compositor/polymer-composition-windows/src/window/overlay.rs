use polymer_platform_windows::{performance_counter, performance_frequency};
use polymer_renderer::egui::EguiPainter;
use polymer_renderer::{CompositingPlan, Renderer};
use polymer_renderer_d3d12::D3d12Compositor;
use polymer_renderer_d3d12::D3d12EguiPainter;
use polymer_statistics::RecorderHandle;
use std::{
    error::Error,
    ops::{Deref, DerefMut},
};

use super::SurfaceWindow;
use polymer_composition::overlay::{OverlayFrame, OverlayState};
use winit::window::Window;
pub(crate) struct EguiOverlay {
    painter: D3d12EguiPainter,
    compositor: D3d12Compositor,
    state: OverlayState,
    pending: Option<OverlayFrame>,
}
impl Deref for EguiOverlay {
    type Target = OverlayState;
    fn deref(&self) -> &OverlayState {
        &self.state
    }
}
impl DerefMut for EguiOverlay {
    fn deref_mut(&mut self) -> &mut OverlayState {
        &mut self.state
    }
}
impl EguiOverlay {
    pub(crate) fn new(window: &Window, recorder: RecorderHandle) -> Result<Self, Box<dyn Error>> {
        let size = window.inner_size();
        let render_window = SurfaceWindow::new(window, size.width.max(1), size.height.max(1));
        let compositor = D3d12Compositor::new_default_for_transparent_composition(&render_window)?;
        let painter = D3d12EguiPainter::new(
            compositor.d3d12_device().clone(),
            compositor.target_format(),
            compositor.frames_in_flight(),
        )?;
        Ok(Self {
            painter,
            compositor,
            pending: None,
            state: OverlayState::new(
                window,
                recorder,
                "D3D12/DXGI",
                "d3d12",
                performance_frequency().unwrap_or(1),
                performance_counter,
            ),
        })
    }
    pub(crate) fn content(&self) -> Result<windows::core::IUnknown, windows::core::Error> {
        self.compositor.composition_content()
    }
    pub(crate) fn resize(&mut self, window: &Window) -> Result<(), Box<dyn Error>> {
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let render_window = SurfaceWindow::new(window, size.width, size.height);
        Renderer::recreate_swapchain(&mut self.compositor, &render_window)?;
        Ok(())
    }
    pub(crate) fn draw(&mut self, window: &Window) -> Result<bool, Box<dyn Error>> {
        if self.pending.is_none() {
            self.pending = self.state.prepare(window);
        }
        let Some(frame) = &self.pending else {
            return Ok(false);
        };
        if !self.compositor.try_begin_frame() {
            self.state.request_immediate_repaint();
            return Ok(false);
        }

        self.painter.prepare(&self.compositor, &frame.output)?;
        let painter = &mut self.painter;
        let presented =
            self.compositor
                .present_with(&CompositingPlan::new([0.; 4], vec![]), None, |pass| {
                    painter.paint(pass, &frame.output, &frame.primitives)
                })?;
        if presented {
            self.pending = None;
        } else {
            self.state.request_immediate_repaint();
        }
        Ok(presented)
    }
}
