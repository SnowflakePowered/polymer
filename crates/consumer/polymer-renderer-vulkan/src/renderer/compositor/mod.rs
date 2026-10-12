use ash::{Entry, vk};
use polymer_consumer_vulkan::physical_device_matches_luid;
use polymer_ipc::StreamReady;
use polymer_protocol::{AdapterLuid, FrameReady};
use polymer_renderer::renderer::{CompositingPlan, RenderWindow, SurfacePlacement};
use std::error::Error;

use polymer_renderer::renderer::SwapchainMode;
use polymer_renderer::{ReceivedSurface, surface_sort_key};

use super::VulkanReceiver;
use super::surface::{ImportedVulkanSurface, SurfaceCache};

mod present;
mod setup;
mod streams;
mod swapchain;
use streams::*;

const FRAMES_IN_FLIGHT: usize = 2;
const SURFACE_DESCRIPTOR_COUNT: u32 = 1024;

struct CompositionPipeline {
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    opaque_pipeline: vk::Pipeline,
    blend_pipeline: vk::Pipeline,
    descriptor_pool: vk::DescriptorPool,
    sampler: vk::Sampler,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SurfacePush {
    source: [f32; 4],
    active_uv: [f32; 2],
    opacity: f32,
    transform: u32,
}

#[derive(Clone, Copy)]
struct SurfaceDraw {
    image: vk::Image,
    descriptor_set: vk::DescriptorSet,
    placement: SurfacePlacement,
    push: SurfacePush,
}

pub struct VulkanCompositor {
    _entry: Entry,
    instance: ash::Instance,
    surface_loader: ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
    physical_device: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    queue_family: u32,
    swapchain_loader: ash::khr::swapchain::Device,
    swapchain: vk::SwapchainKHR,
    swapchain_images: Vec<vk::Image>,
    swapchain_views: Vec<vk::ImageView>,
    swapchain_initialized: Vec<bool>,
    surface_format: vk::SurfaceFormatKHR,
    extent: vk::Extent2D,
    render_pass: vk::RenderPass,
    framebuffers: Vec<vk::Framebuffer>,
    command_pool: vk::CommandPool,
    command_buffers: Vec<vk::CommandBuffer>,
    image_available: Vec<vk::Semaphore>,
    render_finished: Vec<vk::Semaphore>,
    fences: Vec<vk::Fence>,
    frame_index: u64,
    receiver: VulkanReceiver,
    mode: SwapchainMode,
    surfaces: Vec<ImportedVulkanSurface>,
    composition_pipeline: CompositionPipeline,
}

/// Open Vulkan render pass after game composition and before presentation.
pub struct VulkanRenderPass<'a> {
    device: &'a ash::Device,
    command_buffer: vk::CommandBuffer,
    extent: vk::Extent2D,
    frame_index: usize,
}

impl VulkanRenderPass<'_> {
    pub fn device(&self) -> &ash::Device {
        self.device
    }

    pub const fn command_buffer(&self) -> vk::CommandBuffer {
        self.command_buffer
    }

    pub const fn extent(&self) -> vk::Extent2D {
        self.extent
    }

    pub const fn frame_index(&self) -> usize {
        self.frame_index
    }
}

impl VulkanCompositor {
    pub fn instance(&self) -> &ash::Instance {
        &self.instance
    }

    pub const fn physical_device(&self) -> vk::PhysicalDevice {
        self.physical_device
    }

    pub fn device(&self) -> &ash::Device {
        &self.device
    }

    pub const fn queue(&self) -> vk::Queue {
        self.queue
    }

    pub const fn command_pool(&self) -> vk::CommandPool {
        self.command_pool
    }

    pub const fn render_pass(&self) -> vk::RenderPass {
        self.render_pass
    }

    pub const fn frames_in_flight(&self) -> usize {
        FRAMES_IN_FLIGHT
    }

    pub fn matches_adapter(&self, adapter: AdapterLuid) -> bool {
        physical_device_matches_luid(&self.instance, self.physical_device, adapter)
    }

    pub const fn swapchain_mode(&self) -> SwapchainMode {
        self.mode
    }
}

impl Drop for VulkanCompositor {
    fn drop(&mut self) {
        let _ = self.release_imported_streams();
        unsafe {
            // A failed submit can leave a reset frame fence unsignaled forever.
            // Device idle covers submitted work without waiting on such fences.
            let _ = self.device.device_wait_idle();
            // Retirement can fail on device loss. Destroy imported children
            // while their device is still alive even in that case.
            for mut surface in self.surfaces.drain(..) {
                destroy_surface_samples(
                    &self.device,
                    self.composition_pipeline.descriptor_pool,
                    &mut surface.sample_views,
                    &mut surface.sample_sets,
                );
            }
            for fence in self.fences.drain(..) {
                self.device.destroy_fence(fence, None);
            }
            for semaphore in self.render_finished.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
            }
            for semaphore in self.image_available.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
            }
            self.device
                .destroy_pipeline(self.composition_pipeline.opaque_pipeline, None);
            self.device
                .destroy_pipeline(self.composition_pipeline.blend_pipeline, None);
            self.device
                .destroy_pipeline_layout(self.composition_pipeline.pipeline_layout, None);
            self.device.destroy_descriptor_set_layout(
                self.composition_pipeline.descriptor_set_layout,
                None,
            );
            self.device
                .destroy_descriptor_pool(self.composition_pipeline.descriptor_pool, None);
            self.device
                .destroy_sampler(self.composition_pipeline.sampler, None);
            self.device.destroy_command_pool(self.command_pool, None);
            for framebuffer in self.framebuffers.drain(..) {
                self.device.destroy_framebuffer(framebuffer, None);
            }
            self.device.destroy_render_pass(self.render_pass, None);
            for view in self.swapchain_views.drain(..) {
                self.device.destroy_image_view(view, None);
            }
            self.swapchain_loader
                .destroy_swapchain(self.swapchain, None);
            self.device.destroy_device(None);
            self.surface_loader.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}

pub(super) fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

pub fn blit_source_offsets(
    width: u32,
    height: u32,
    flip_y: bool,
    quarter_turns: u8,
) -> [vk::Offset3D; 2] {
    let mut x = [0, width as i32];
    let mut y = if flip_y {
        [height as i32, 0]
    } else {
        [0, height as i32]
    };
    if quarter_turns == 2 {
        x.reverse();
        y.reverse();
    }
    [
        vk::Offset3D {
            x: x[0],
            y: y[0],
            z: 0,
        },
        vk::Offset3D {
            x: x[1],
            y: y[1],
            z: 1,
        },
    ]
}
