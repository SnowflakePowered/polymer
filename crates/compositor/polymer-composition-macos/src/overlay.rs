use polymer_platform_macos::{performance_counter, performance_frequency};
use polymer_renderer::CompositingPlan;
use polymer_renderer::egui::EguiPainter;
use polymer_renderer_metal::MetalCompositor;
use polymer_renderer_metal::MetalEguiPainter;
use polymer_statistics::RecorderHandle;
use std::{
    error::Error,
    ops::{Deref, DerefMut},
};

use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::MTLDevice;
use polymer_composition::overlay::{OverlayFrame, OverlayState};
use winit::window::Window;
pub(crate) struct EguiOverlay {
    painter: MetalEguiPainter,
    compositor: MetalCompositor,
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
    pub(crate) fn new(
        window: &Window,
        recorder: RecorderHandle,
        device: Retained<ProtocolObject<dyn MTLDevice>>,
        wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, Box<dyn Error>> {
        let size = window.inner_size();
        let mut compositor = MetalCompositor::for_layer(
            device,
            polymer_renderer::Extent::new(size.width.max(1), size.height.max(1)),
        )?;
        compositor.set_wake(wake);
        let painter = MetalEguiPainter::default();
        Ok(Self {
            painter,
            compositor,
            pending: None,
            state: OverlayState::new(
                window,
                recorder,
                "Metal/Core Animation",
                "metal",
                performance_frequency().unwrap_or(1),
                performance_counter,
            ),
        })
    }
    pub(crate) fn layer(&self) -> &objc2_quartz_core::CAMetalLayer {
        self.compositor.layer()
    }
    pub(crate) fn resize(&mut self, window: &Window) -> Result<(), Box<dyn Error>> {
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        self.compositor
            .resize_layer(polymer_renderer::Extent::new(size.width, size.height));
        Ok(())
    }
    pub(crate) fn draw(&mut self, window: &Window) -> Result<bool, Box<dyn Error>> {
        if self.pending.is_none() {
            self.pending = self.state.prepare(window);
        }
        let Some(frame) = &self.pending else {
            return Ok(false);
        };

        self.painter.prepare(&self.compositor, &frame.output)?;
        let painter = &mut self.painter;
        let presented = self
            .compositor
            .present_with(&CompositingPlan::new([0.; 4], vec![]), |pass| {
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
