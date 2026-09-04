//! Egui rendering commands appended to a Polymer Vulkan frame.

use std::error::Error;

use egui_ash_renderer::{Options, Renderer};
use polymer_renderer::renderer::vulkan::{VulkanCompositor, VulkanRenderPass};

/// Vulkan egui resources owned by a viewer rather than the game compositor.
pub struct VulkanEguiPainter {
    renderer: Renderer,
}

impl VulkanEguiPainter {
    pub fn new(compositor: &VulkanCompositor) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            renderer: Renderer::with_default_allocator(
                compositor.instance(),
                compositor.physical_device(),
                compositor.device().clone(),
                compositor.render_pass(),
                Options {
                    in_flight_frames: compositor.frames_in_flight(),
                    srgb_framebuffer: false,
                    ..Default::default()
                },
            )?,
        })
    }

    /// Applies atlas changes before the compositor begins its render pass.
    pub fn prepare(
        &mut self,
        compositor: &VulkanCompositor,
        output: &egui::FullOutput,
    ) -> Result<(), Box<dyn Error>> {
        self.renderer.free_textures(&output.textures_delta.free)?;
        self.renderer.set_textures(
            compositor.queue(),
            compositor.command_pool(),
            &output.textures_delta.set,
        )?;
        Ok(())
    }

    pub fn paint(
        &mut self,
        pass: &mut VulkanRenderPass<'_>,
        output: &egui::FullOutput,
        primitives: &[egui::ClippedPrimitive],
    ) -> Result<(), Box<dyn Error>> {
        self.renderer.cmd_draw(
            pass.command_buffer(),
            pass.extent(),
            output.pixels_per_point,
            primitives,
        )?;
        Ok(())
    }
}
