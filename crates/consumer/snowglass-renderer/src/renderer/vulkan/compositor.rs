use crate::renderer::{CompositingPlan, RenderWindow, SurfacePlacement};
use ash::{Entry, vk};
use snowglass_consumer_vulkan::ImportedImageRing;
use snowglass_input::mouse::layout::Extent;
use snowglass_ipc::StreamReady;
use snowglass_protocol::{AdapterLuid, FrameReady, RELEASE_ALL_SYNC_VALUE};
use std::error::Error;

use crate::renderer::{
    SwapchainMode,
    webrtc::{VulkanVideoCache, VulkanVideoReadback, VulkanWebRtcStream, WebRtcCompositor},
};
use crate::{fit_inside, renderer::ReceivedSurface, surface_sort_key, webrtc};

use super::VulkanReceiver;

const FRAMES_IN_FLIGHT: usize = 2;
const SURFACE_DESCRIPTOR_COUNT: u32 = 1024;

struct ImportedVulkanSurface {
    connection: u64,
    surface_id: u64,
    target_window: i64,
    imported: ImportedImageRing,
    sample_views: Vec<vk::ImageView>,
    sample_sets: Vec<vk::DescriptorSet>,
    flip_y: bool,
    quarter_turns: u8,
    latest: Option<FrameReady>,
    last_released_frame: u64,
}

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
    external_queue: bool,
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
    keyed_mutex_enabled: bool,
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
    pub fn new(
        window: &impl RenderWindow,
        preferred_adapter: Option<AdapterLuid>,
        enable_keyed_mutex: bool,
    ) -> Result<Self, Box<dyn Error>> {
        Self::with_mode(
            window,
            preferred_adapter,
            enable_keyed_mutex,
            false,
            SwapchainMode::Shared,
        )
        .map(|(compositor, _)| compositor)
    }

    pub fn new_with_webrtc(
        window: &impl RenderWindow,
        preferred_adapter: Option<AdapterLuid>,
        enable_keyed_mutex: bool,
    ) -> Result<(Self, WebRtcCompositor), Box<dyn Error>> {
        let (compositor, webrtc) = Self::with_mode(
            window,
            preferred_adapter,
            enable_keyed_mutex,
            true,
            SwapchainMode::Shared,
        )?;
        Ok((
            compositor,
            webrtc.expect("WebRTC was requested during Vulkan device creation"),
        ))
    }

    /// Creates a swapchain compositor dedicated to one received connection.
    pub fn new_for_stream(
        window: &impl RenderWindow,
        preferred_adapter: Option<AdapterLuid>,
        enable_keyed_mutex: bool,
        connection: u64,
    ) -> Result<Self, Box<dyn Error>> {
        Self::with_mode(
            window,
            preferred_adapter,
            enable_keyed_mutex,
            false,
            SwapchainMode::Dedicated(connection),
        )
        .map(|(compositor, _)| compositor)
    }

    pub fn new_for_stream_with_webrtc(
        window: &impl RenderWindow,
        preferred_adapter: Option<AdapterLuid>,
        enable_keyed_mutex: bool,
        connection: u64,
    ) -> Result<(Self, WebRtcCompositor), Box<dyn Error>> {
        let (compositor, webrtc) = Self::with_mode(
            window,
            preferred_adapter,
            enable_keyed_mutex,
            true,
            SwapchainMode::Dedicated(connection),
        )?;
        Ok((
            compositor,
            webrtc.expect("WebRTC was requested during Vulkan device creation"),
        ))
    }

    fn with_mode(
        window: &impl RenderWindow,
        preferred_adapter: Option<AdapterLuid>,
        enable_keyed_mutex: bool,
        publish_video: bool,
        mode: SwapchainMode,
    ) -> Result<(Self, Option<WebRtcCompositor>), Box<dyn Error>> {
        // SAFETY: all Vulkan children are owned by Self and destroyed in reverse
        // dependency order after the device becomes idle.
        unsafe {
            let entry = Entry::load()?;
            let display = window.display_handle()?.as_raw();
            let window_handle = window.window_handle()?.as_raw();
            let extensions = ash_window::enumerate_required_extensions(display)?;
            let application = vk::ApplicationInfo::default()
                .application_name(c"snowglass-viewer")
                .api_version(vk::API_VERSION_1_2);
            let instance = entry.create_instance(
                &vk::InstanceCreateInfo::default()
                    .application_info(&application)
                    .enabled_extension_names(extensions),
                None,
            )?;
            let surface =
                ash_window::create_surface(&entry, &instance, display, window_handle, None)?;
            let surface_loader = ash::khr::surface::Instance::new(&entry, &instance);
            let mut required = vec![
                ash::khr::swapchain::NAME,
                ash::khr::external_memory_win32::NAME,
                ash::khr::external_semaphore_win32::NAME,
                ash::khr::timeline_semaphore::NAME,
            ];
            if enable_keyed_mutex {
                required.push(ash::khr::win32_keyed_mutex::NAME);
            }
            let (physical_device, queue_family, video_queue_family) = instance
                .enumerate_physical_devices()?
                .into_iter()
                .find_map(|physical| {
                    if preferred_adapter.is_some_and(|luid| {
                        !physical_device_matches_luid(&instance, physical, luid)
                    }) {
                        return None;
                    }
                    let available = instance
                        .enumerate_device_extension_properties(physical)
                        .ok()?;
                    let supports_extensions = required.iter().all(|required| {
                        available.iter().any(|property| {
                            std::ffi::CStr::from_ptr(property.extension_name.as_ptr()) == *required
                        })
                    });
                    if !supports_extensions {
                        return None;
                    }
                    let video_extensions = [
                        ash::khr::video_queue::NAME,
                        ash::khr::video_encode_queue::NAME,
                        ash::khr::video_encode_h264::NAME,
                        ash::khr::synchronization2::NAME,
                    ];
                    let supports_video_extensions = publish_video
                        && video_extensions.iter().all(|required| {
                            available.iter().any(|property| {
                                std::ffi::CStr::from_ptr(property.extension_name.as_ptr())
                                    == *required
                            })
                        });
                    let families = instance.get_physical_device_queue_family_properties(physical);
                    let video_operations = queue_family_video_operations(&instance, physical);
                    let graphics_family =
                        families
                            .iter()
                            .enumerate()
                            .find_map(|(index, properties)| {
                                let graphics =
                                    properties.queue_flags.contains(vk::QueueFlags::GRAPHICS);
                                let present = surface_loader
                                    .get_physical_device_surface_support(
                                        physical,
                                        index as u32,
                                        surface,
                                    )
                                    .ok()?;
                                (graphics && present).then_some(index as u32)
                            })?;
                    let video_family = (supports_video_extensions
                        && families[graphics_family as usize].queue_count >= 2)
                        .then(|| {
                            families
                                .iter()
                                .enumerate()
                                .find(|(index, properties)| {
                                    *index as u32 != graphics_family
                                        && properties
                                            .queue_flags
                                            .contains(vk::QueueFlags::VIDEO_ENCODE_KHR)
                                        && video_operations[*index]
                                            .contains(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
                                })
                                .map(|(index, _)| index as u32)
                        })
                        .flatten();
                    Some((physical, graphics_family, video_family))
                })
                .ok_or("no Vulkan device supports capture import and presentation")?;

            let priorities = if video_queue_family.is_some() {
                vec![1.0, 1.0]
            } else {
                vec![1.0]
            };
            let video_priorities = [1.0];
            let queue_info = vk::DeviceQueueCreateInfo::default()
                .queue_family_index(queue_family)
                .queue_priorities(&priorities);
            let video_queue_info = video_queue_family.map(|family| {
                vk::DeviceQueueCreateInfo::default()
                    .queue_family_index(family)
                    .queue_priorities(&video_priorities)
            });
            let mut queue_infos = vec![queue_info];
            if let Some(info) = video_queue_info {
                queue_infos.push(info);
            }
            let mut extension_names = required
                .iter()
                .map(|name| name.as_ptr())
                .collect::<Vec<_>>();
            if video_queue_family.is_some() {
                extension_names.extend([
                    ash::khr::video_queue::NAME.as_ptr(),
                    ash::khr::video_encode_queue::NAME.as_ptr(),
                    ash::khr::video_encode_h264::NAME.as_ptr(),
                    ash::khr::synchronization2::NAME.as_ptr(),
                ]);
                let available = instance.enumerate_device_extension_properties(physical_device)?;
                if available.iter().any(|property| {
                    std::ffi::CStr::from_ptr(property.extension_name.as_ptr())
                        == ash::khr::video_encode_h265::NAME
                }) {
                    extension_names.push(ash::khr::video_encode_h265::NAME.as_ptr());
                }
            }
            let mut timeline_features =
                vk::PhysicalDeviceTimelineSemaphoreFeatures::default().timeline_semaphore(true);
            let device = instance.create_device(
                physical_device,
                &vk::DeviceCreateInfo::default()
                    .queue_create_infos(&queue_infos)
                    .enabled_extension_names(&extension_names)
                    .push_next(&mut timeline_features),
                None,
            )?;
            let queue = device.get_device_queue(queue_family, 0);
            let video_queue = video_queue_family.map(|family| device.get_device_queue(family, 0));
            let video_conversion_queue =
                video_queue_family.map(|_| device.get_device_queue(queue_family, 1));
            let (video_ready, video_completion) = if video_queue.is_some() {
                let mut timeline = vk::SemaphoreTypeCreateInfo::default()
                    .semaphore_type(vk::SemaphoreType::TIMELINE)
                    .initial_value(0);
                let create = vk::SemaphoreCreateInfo::default().push_next(&mut timeline);
                (
                    Some(device.create_semaphore(&create, None)?),
                    Some(device.create_semaphore(&create, None)?),
                )
            } else {
                (None, None)
            };
            let swapchain_loader = ash::khr::swapchain::Device::new(&instance, &device);
            let capabilities = surface_loader
                .get_physical_device_surface_capabilities(physical_device, surface)?;
            let surface_format = surface_loader
                .get_physical_device_surface_formats(physical_device, surface)?
                .into_iter()
                .find(|format| format.format == vk::Format::B8G8R8A8_UNORM)
                .unwrap_or(vk::SurfaceFormatKHR {
                    format: vk::Format::B8G8R8A8_UNORM,
                    color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
                });
            let requested = window.render_extent();
            let extent = if capabilities.current_extent.width != u32::MAX {
                capabilities.current_extent
            } else {
                vk::Extent2D {
                    width: requested.width.clamp(
                        capabilities.min_image_extent.width,
                        capabilities.max_image_extent.width,
                    ),
                    height: requested.height.clamp(
                        capabilities.min_image_extent.height,
                        capabilities.max_image_extent.height,
                    ),
                }
            };
            let image_count = capabilities.min_image_count;
            let swapchain = swapchain_loader.create_swapchain(
                &vk::SwapchainCreateInfoKHR::default()
                    .surface(surface)
                    .min_image_count(image_count)
                    .image_format(surface_format.format)
                    .image_color_space(surface_format.color_space)
                    .image_extent(extent)
                    .image_array_layers(1)
                    .image_usage(
                        vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT,
                    )
                    .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .pre_transform(capabilities.current_transform)
                    .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                    .present_mode(vk::PresentModeKHR::FIFO)
                    .clipped(true),
                None,
            )?;
            let swapchain_images = swapchain_loader.get_swapchain_images(swapchain)?;
            let swapchain_views = swapchain_images
                .iter()
                .map(|&image| {
                    device.create_image_view(
                        &vk::ImageViewCreateInfo::default()
                            .image(image)
                            .view_type(vk::ImageViewType::TYPE_2D)
                            .format(surface_format.format)
                            .subresource_range(color_range()),
                        None,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            let attachment = vk::AttachmentDescription::default()
                .format(surface_format.format)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(vk::AttachmentLoadOp::LOAD)
                .store_op(vk::AttachmentStoreOp::STORE)
                .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
            let color_reference = vk::AttachmentReference {
                attachment: 0,
                layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            };
            let subpass = vk::SubpassDescription::default()
                .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                .color_attachments(std::slice::from_ref(&color_reference));
            let render_pass = device.create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(std::slice::from_ref(&attachment))
                    .subpasses(std::slice::from_ref(&subpass)),
                None,
            )?;
            let composition_pipeline = create_composition_pipeline(&device, render_pass)?;
            let framebuffers = swapchain_views
                .iter()
                .map(|view| {
                    device.create_framebuffer(
                        &vk::FramebufferCreateInfo::default()
                            .render_pass(render_pass)
                            .attachments(std::slice::from_ref(view))
                            .width(extent.width)
                            .height(extent.height)
                            .layers(1),
                        None,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            let command_pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(queue_family),
                None,
            )?;
            let command_buffers = device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(FRAMES_IN_FLIGHT as u32),
            )?;
            let mut image_available = Vec::new();
            let mut render_finished = Vec::new();
            let mut fences = Vec::new();
            for _ in 0..FRAMES_IN_FLIGHT {
                image_available
                    .push(device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?);
                fences.push(device.create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )?);
            }
            for _ in &swapchain_images {
                render_finished
                    .push(device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?);
            }
            let initialized_count = swapchain_images.len();
            let receiver = VulkanReceiver::new(instance.clone(), device.clone(), physical_device);
            let webrtc = publish_video.then(|| {
                WebRtcCompositor::new_vulkan(
                    physical_device,
                    device.clone(),
                    video_queue,
                    video_queue_family,
                    video_conversion_queue,
                    queue_family,
                    video_ready,
                    video_completion,
                )
            });
            Ok((
                Self {
                    _entry: entry,
                    instance,
                    surface_loader,
                    surface,
                    physical_device,
                    device,
                    queue,
                    queue_family,
                    swapchain_loader,
                    swapchain,
                    swapchain_images,
                    swapchain_views,
                    swapchain_initialized: vec![false; initialized_count],
                    surface_format,
                    extent,
                    render_pass,
                    framebuffers,
                    command_pool,
                    command_buffers,
                    image_available,
                    render_finished,
                    fences,
                    frame_index: 0,
                    receiver,
                    mode,
                    surfaces: Vec::new(),
                    composition_pipeline,
                    keyed_mutex_enabled: enable_keyed_mutex,
                },
                webrtc,
            ))
        }
    }

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

    pub(crate) fn import_stream(
        &mut self,
        connection: u64,
        stream: &StreamReady,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        if !self.mode.accepts(connection) {
            return Err(
                format!("connection {connection} does not belong to {:?}", self.mode).into(),
            );
        }
        self.disconnect_stream(connection)?;
        let imported = self.receiver.import_stream(stream)?;
        let quarter_turns = quarter_turns % 4;
        let (sample_views, sample_sets) = unsafe {
            create_surface_samples(
                &self.device,
                &imported,
                stream.slot_count,
                &self.composition_pipeline,
            )?
        };
        self.surfaces.push(ImportedVulkanSurface {
            connection,
            surface_id: stream.surface_id.0,
            target_window: stream.source_window,
            imported,
            sample_views,
            sample_sets,
            flip_y: stream.bottom_up,
            quarter_turns,
            latest: None,
            last_released_frame: 0,
        });
        self.surfaces
            .sort_by_key(|surface| surface_sort_key(surface.surface_id));
        Ok(())
    }

    pub fn matches_adapter(&self, adapter: AdapterLuid) -> bool {
        physical_device_matches_luid(&self.instance, self.physical_device, adapter)
    }

    pub const fn swapchain_mode(&self) -> SwapchainMode {
        self.mode
    }

    pub fn keyed_mutex_enabled(&self) -> bool {
        self.keyed_mutex_enabled
    }

    pub(crate) fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        if let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.connection == connection)
        {
            surface.latest = Some(frame);
        }
    }

    pub(crate) fn received_surfaces(&self) -> Vec<ReceivedSurface> {
        self.surfaces
            .iter()
            .filter_map(|surface| {
                let frame = surface.latest?;
                let (width, height) = fit_inside(
                    frame.render_width.max(1),
                    frame.render_height.max(1),
                    surface.imported.width,
                    surface.imported.height,
                );
                let image_extent = if surface.quarter_turns % 2 == 1 {
                    Extent::new(height, width)
                } else {
                    Extent::new(width, height)
                };
                Some(ReceivedSurface {
                    connection: surface.connection,
                    surface_id: surface.surface_id,
                    target_window: surface.target_window,
                    image_extent,
                    quarter_turns: surface.quarter_turns,
                    flip_y: surface.flip_y,
                    frame,
                })
            })
            .collect()
    }

    pub(crate) fn has_pending_frame(&self) -> bool {
        self.surfaces.iter().any(|surface| surface.latest.is_some())
    }

    pub(crate) fn disconnect_stream(&mut self, connection: u64) -> Result<(), vk::Result> {
        let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.connection == connection)
        else {
            return Ok(());
        };
        unsafe {
            self.device.device_wait_idle()?;
            if let Some(semaphore) = self.surfaces[index].imported.consumer_timeline() {
                self.device.signal_semaphore(
                    &vk::SemaphoreSignalInfo::default()
                        .semaphore(semaphore)
                        .value(RELEASE_ALL_SYNC_VALUE),
                )?;
            }
        }
        let mut surface = self.surfaces.remove(index);
        unsafe {
            destroy_surface_samples(
                &self.device,
                self.composition_pipeline.descriptor_pool,
                &mut surface.sample_views,
                &mut surface.sample_sets,
            );
        };
        Ok(())
    }

    fn release_imported_streams(&mut self) -> Result<(), vk::Result> {
        unsafe {
            self.device.device_wait_idle()?;
            for surface in &self.surfaces {
                if let Some(semaphore) = surface.imported.consumer_timeline() {
                    self.device.signal_semaphore(
                        &vk::SemaphoreSignalInfo::default()
                            .semaphore(semaphore)
                            .value(RELEASE_ALL_SYNC_VALUE),
                    )?;
                }
            }
        }
        for mut surface in self.surfaces.drain(..) {
            unsafe {
                destroy_surface_samples(
                    &self.device,
                    self.composition_pipeline.descriptor_pool,
                    &mut surface.sample_views,
                    &mut surface.sample_sets,
                );
            };
        }
        Ok(())
    }

    pub(crate) fn recreate_swapchain(
        &mut self,
        window: &impl RenderWindow,
    ) -> Result<bool, Box<dyn Error>> {
        let requested = window.render_extent();
        if requested.width == 0 || requested.height == 0 {
            return Ok(false);
        }
        // SAFETY: the device is idle before replacing every swapchain child;
        // the old swapchain remains live until the replacement is created.
        unsafe {
            self.device.device_wait_idle()?;
            let capabilities = self
                .surface_loader
                .get_physical_device_surface_capabilities(self.physical_device, self.surface)?;
            let extent = if capabilities.current_extent.width != u32::MAX {
                capabilities.current_extent
            } else {
                vk::Extent2D {
                    width: requested.width.clamp(
                        capabilities.min_image_extent.width,
                        capabilities.max_image_extent.width,
                    ),
                    height: requested.height.clamp(
                        capabilities.min_image_extent.height,
                        capabilities.max_image_extent.height,
                    ),
                }
            };
            if extent.width == 0 || extent.height == 0 {
                return Ok(false);
            }
            let image_count = capabilities.min_image_count;
            let replacement = self.swapchain_loader.create_swapchain(
                &vk::SwapchainCreateInfoKHR::default()
                    .surface(self.surface)
                    .min_image_count(image_count)
                    .image_format(self.surface_format.format)
                    .image_color_space(self.surface_format.color_space)
                    .image_extent(extent)
                    .image_array_layers(1)
                    .image_usage(
                        vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT,
                    )
                    .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .pre_transform(capabilities.current_transform)
                    .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                    .present_mode(vk::PresentModeKHR::FIFO)
                    .clipped(true)
                    .old_swapchain(self.swapchain),
                None,
            )?;
            let images = self.swapchain_loader.get_swapchain_images(replacement)?;
            let views = create_swapchain_views(&self.device, &images, self.surface_format.format)?;
            let framebuffers = create_framebuffers(&self.device, self.render_pass, &views, extent)?;
            let render_finished = images
                .iter()
                .map(|_| {
                    self.device
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                })
                .collect::<Result<Vec<_>, _>>()?;

            for framebuffer in self.framebuffers.drain(..) {
                self.device.destroy_framebuffer(framebuffer, None);
            }
            for view in self.swapchain_views.drain(..) {
                self.device.destroy_image_view(view, None);
            }
            for semaphore in self.render_finished.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
            }
            self.swapchain_loader
                .destroy_swapchain(self.swapchain, None);
            self.swapchain = replacement;
            self.swapchain_images = images;
            self.swapchain_views = views;
            self.framebuffers = framebuffers;
            self.render_finished = render_finished;
            self.swapchain_initialized = vec![false; self.swapchain_images.len()];
            self.extent = extent;
            Ok(true)
        }
    }

    pub fn present_with(
        &mut self,
        plan: &CompositingPlan,
        webrtc: Option<&mut WebRtcCompositor>,
        record: impl FnOnce(&mut VulkanRenderPass<'_>) -> Result<(), Box<dyn Error>>,
    ) -> Result<bool, Box<dyn Error>> {
        let mut video_parts = webrtc.and_then(WebRtcCompositor::vulkan_parts_mut);
        if video_parts
            .as_ref()
            .is_some_and(|(_, video)| video.physical_device != self.physical_device)
        {
            return Err("WebRTC compositor belongs to a different Vulkan device".into());
        }
        // SAFETY: frame fences protect command and semaphore reuse; acquisition
        // owns the destination image; the external timeline gates source reads.
        unsafe {
            let flight = self.frame_index as usize % FRAMES_IN_FLIGHT;
            self.device
                .wait_for_fences(&[self.fences[flight]], true, u64::MAX)?;
            if let Some((video_swapchain, video)) = video_parts.as_mut() {
                for surface in &mut self.surfaces {
                    let Some(stream) = video.streams.get_mut(&surface.connection) else {
                        continue;
                    };
                    let Some(readback) = stream.readbacks.get_mut(flight) else {
                        continue;
                    };
                    let Some(_completed) = readback.pending.take() else {
                        continue;
                    };
                    let byte_len =
                        surface.imported.width as usize * surface.imported.height as usize * 4;
                    let mapped = self.device.map_memory(
                        readback.memory,
                        0,
                        byte_len as u64,
                        vk::MemoryMapFlags::empty(),
                    )?;
                    let packed = std::slice::from_raw_parts(mapped.cast::<u8>(), byte_len);
                    let frame = webrtc::VideoFrame {
                        connection: surface.connection,
                        width: surface.imported.width,
                        height: surface.imported.height,
                        data: webrtc::VideoFrameData::Rgba(vulkan_pixels_to_rgba(
                            surface.imported.format,
                            packed,
                        )),
                    };
                    let _ = video_swapchain.present(frame);
                    self.device.unmap_memory(readback.memory);
                }
            }
            for surface in self
                .surfaces
                .iter()
                .filter(|surface| surface.latest.is_some() && surface.imported.uses_keyed_mutex())
            {
                // A D3D11 shared texture is made visible by both its fence and
                // its keyed mutex. Waiting for the fence before submission
                // avoids asking the Vulkan driver to resolve both primitives
                // inside the same queue operation.
                surface
                    .imported
                    .wait_for(surface.latest.unwrap().sync_value, u64::MAX)?;
            }
            let (image_index, acquire_suboptimal) = match self.swapchain_loader.acquire_next_image(
                self.swapchain,
                u64::MAX,
                self.image_available[flight],
                vk::Fence::null(),
            ) {
                Ok(acquired) => acquired,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return Ok(false),
                Err(error) => return Err(error.into()),
            };
            self.device.reset_fences(&[self.fences[flight]])?;
            let command = self.command_buffers[flight];
            self.device
                .reset_command_buffer(command, vk::CommandBufferResetFlags::empty())?;
            self.device.begin_command_buffer(
                command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            let destination = self.swapchain_images[image_index as usize];
            let old_destination_layout = if self.swapchain_initialized[image_index as usize] {
                vk::ImageLayout::PRESENT_SRC_KHR
            } else {
                vk::ImageLayout::UNDEFINED
            };
            let to_destination = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(old_destination_layout)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .image(destination)
                .subresource_range(color_range());
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_destination],
            );
            if self.has_pending_frame() || !self.swapchain_initialized[image_index as usize] {
                self.device.cmd_clear_color_image(
                    command,
                    destination,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &vk::ClearColorValue {
                        float32: plan.clear_color(),
                    },
                    &[color_range()],
                );
            }
            let mut video_copies = Vec::new();
            // A timeline semaphore must be signaled in strictly increasing
            // order, but published frames resolve out of order: dropped frames
            // host-signal immediately while accepted frames signal from the
            // encoder later. Publishing only while no completion value is
            // outstanding keeps the timeline monotonic.
            let video_publish_ready = match video_parts.as_ref() {
                Some((_, video)) => match video.completion {
                    Some(semaphore) => {
                        self.device.get_semaphore_counter_value(semaphore)?
                            >= video.completion_value
                    }
                    None => true,
                },
                None => false,
            };
            let mut video_publish_budget = video_parts
                .as_ref()
                .map_or(0, |(swapchain, _)| swapchain.available());
            let mut surface_draws = Vec::new();
            for (surface_index, surface) in self.surfaces.iter_mut().enumerate() {
                let imported = &surface.imported;
                let Some(frame) = surface.latest else {
                    continue;
                };
                if let Some(source) = imported.image(frame.slot) {
                    let mut to_source = vk::ImageMemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                        .old_layout(vk::ImageLayout::GENERAL)
                        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .image(source)
                        .subresource_range(color_range());
                    if imported.uses_external_queue_family() {
                        to_source = to_source
                            .src_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
                            .dst_queue_family_index(self.queue_family);
                    }
                    self.device.cmd_pipeline_barrier(
                        command,
                        vk::PipelineStageFlags::ALL_COMMANDS,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &[to_source],
                    );
                    let (content_width, content_height) = fit_inside(
                        frame.render_width,
                        frame.render_height,
                        imported.width,
                        imported.height,
                    );
                    if video_publish_ready
                        && video_publish_budget > 0
                        && let Some(cache) = video_parts
                            .as_mut()
                            .and_then(|(_, video)| video.streams.get_mut(&surface.connection))
                            .and_then(|stream| stream.cache.as_mut())
                    {
                        video_publish_budget -= 1;
                        let cache_to_destination = vk::ImageMemoryBarrier::default()
                            .src_access_mask(if cache.initialized {
                                vk::AccessFlags::TRANSFER_READ
                            } else {
                                vk::AccessFlags::empty()
                            })
                            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                            .old_layout(if cache.initialized {
                                vk::ImageLayout::TRANSFER_SRC_OPTIMAL
                            } else {
                                vk::ImageLayout::UNDEFINED
                            })
                            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                            .image(cache.image)
                            .subresource_range(color_range());
                        self.device.cmd_pipeline_barrier(
                            command,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::DependencyFlags::empty(),
                            &[],
                            &[],
                            &[cache_to_destination],
                        );
                        self.device.cmd_copy_image(
                            command,
                            source,
                            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                            cache.image,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            &[vk::ImageCopy::default()
                                .src_subresource(color_layers())
                                .dst_subresource(color_layers())
                                .extent(vk::Extent3D {
                                    width: imported.width,
                                    height: imported.height,
                                    depth: 1,
                                })],
                        );
                        let cache_to_source = vk::ImageMemoryBarrier::default()
                            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                            .image(cache.image)
                            .subresource_range(color_range());
                        self.device.cmd_pipeline_barrier(
                            command,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::DependencyFlags::empty(),
                            &[],
                            &[],
                            &[cache_to_source],
                        );
                        cache.initialized = true;
                        video_copies.push((surface_index, frame));
                    }
                    if let Some(readback) = video_parts
                        .as_mut()
                        .and_then(|(_, video)| video.streams.get_mut(&surface.connection))
                        .and_then(|stream| stream.readbacks.get(flight))
                    {
                        self.device.cmd_copy_image_to_buffer(
                            command,
                            source,
                            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                            readback.buffer,
                            &[vk::BufferImageCopy::default()
                                .image_subresource(color_layers())
                                .image_extent(vk::Extent3D {
                                    width: imported.width,
                                    height: imported.height,
                                    depth: 1,
                                })],
                        );
                        let to_host = vk::BufferMemoryBarrier::default()
                            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                            .dst_access_mask(vk::AccessFlags::HOST_READ)
                            .buffer(readback.buffer)
                            .offset(0)
                            .size(vk::WHOLE_SIZE);
                        self.device.cmd_pipeline_barrier(
                            command,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::PipelineStageFlags::HOST,
                            vk::DependencyFlags::empty(),
                            &[],
                            &[to_host],
                            &[],
                        );
                        video_copies.push((surface_index, frame));
                    }
                    let placement = plan
                        .placements()
                        .find(|placement| {
                            placement.connection == surface.connection
                                && placement.surface_id == surface.surface_id
                        })
                        .copied();
                    let descriptor_set = surface.sample_sets.get(frame.slot as usize).copied();
                    let drawable = placement.zip(descriptor_set).filter(|(placement, _)| {
                        let content = placement.content;
                        content.opacity.is_finite()
                            && content.opacity > 0.0
                            && content.destination[2] > content.destination[0]
                            && content.destination[3] > content.destination[1]
                    });
                    if let Some((placement, descriptor_set)) = drawable {
                        let source_to_shader = vk::ImageMemoryBarrier::default()
                            .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                            .dst_access_mask(vk::AccessFlags::SHADER_READ)
                            .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                            .image(source)
                            .subresource_range(color_range());
                        self.device.cmd_pipeline_barrier(
                            command,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::PipelineStageFlags::FRAGMENT_SHADER,
                            vk::DependencyFlags::empty(),
                            &[],
                            &[],
                            &[source_to_shader],
                        );
                        surface_draws.push(SurfaceDraw {
                            image: source,
                            external_queue: imported.uses_external_queue_family(),
                            descriptor_set,
                            placement,
                            push: SurfacePush {
                                source: placement.content.source,
                                active_uv: [
                                    content_width as f32 / imported.width as f32,
                                    content_height as f32 / imported.height as f32,
                                ],
                                opacity: placement.content.opacity.clamp(0.0, 1.0),
                                transform: u32::from(surface.quarter_turns)
                                    | if surface.flip_y { 0x100 } else { 0 },
                            },
                        });
                    } else {
                        let mut source_to_general = vk::ImageMemoryBarrier::default()
                            .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                            .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                            .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                            .new_layout(vk::ImageLayout::GENERAL)
                            .image(source)
                            .subresource_range(color_range());
                        if imported.uses_external_queue_family() {
                            source_to_general = source_to_general
                                .src_queue_family_index(self.queue_family)
                                .dst_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL);
                        }
                        self.device.cmd_pipeline_barrier(
                            command,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::PipelineStageFlags::ALL_COMMANDS,
                            vk::DependencyFlags::empty(),
                            &[],
                            &[],
                            &[source_to_general],
                        );
                    }
                }
            }
            let to_color = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .image(destination)
                .subresource_range(color_range());
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_color],
            );
            self.device.cmd_begin_render_pass(
                command,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(self.render_pass)
                    .framebuffer(self.framebuffers[image_index as usize])
                    .render_area(vk::Rect2D {
                        offset: vk::Offset2D::default(),
                        extent: self.extent,
                    }),
                vk::SubpassContents::INLINE,
            );
            draw_surface_content(
                &self.device,
                command,
                self.extent,
                &self.composition_pipeline,
                plan,
                &surface_draws,
            );
            record(&mut VulkanRenderPass {
                device: &self.device,
                command_buffer: command,
                extent: self.extent,
                frame_index: flight,
            })?;
            self.device.cmd_end_render_pass(command);
            for draw in &surface_draws {
                let mut to_general = vk::ImageMemoryBarrier::default()
                    .src_access_mask(vk::AccessFlags::SHADER_READ)
                    .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                    .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .image(draw.image)
                    .subresource_range(color_range());
                if draw.external_queue {
                    to_general = to_general
                        .src_queue_family_index(self.queue_family)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL);
                }
                self.device.cmd_pipeline_barrier(
                    command,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[to_general],
                );
            }
            let to_present = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .dst_access_mask(vk::AccessFlags::empty())
                .old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .image(destination)
                .subresource_range(color_range());
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_present],
            );
            self.device.end_command_buffer(command)?;

            let mut waits = vec![self.image_available[flight]];
            let mut wait_values = vec![0];
            let mut wait_stages = vec![vk::PipelineStageFlags::TRANSFER];
            for surface in &self.surfaces {
                if let Some(frame) = surface.latest
                    && !surface.imported.uses_keyed_mutex()
                {
                    waits.push(surface.imported.timeline());
                    wait_values.push(frame.sync_value);
                    wait_stages.push(
                        vk::PipelineStageFlags::TRANSFER | vk::PipelineStageFlags::FRAGMENT_SHADER,
                    );
                }
            }
            let video_wait_value = video_parts.as_ref().map_or(0, |(_, video)| {
                self.surfaces
                    .iter()
                    .filter(|surface| surface.latest.is_some())
                    .filter_map(|surface| video.streams.get(&surface.connection))
                    .filter(|stream| stream.cache.is_some())
                    .map(|stream| stream.completion_value)
                    .max()
                    .unwrap_or(0)
            });
            if video_wait_value != 0 {
                waits.push(
                    video_parts
                        .as_ref()
                        .and_then(|(_, video)| video.completion)
                        .expect("video completion semaphore"),
                );
                wait_values.push(video_wait_value);
                wait_stages.push(vk::PipelineStageFlags::TRANSFER);
            }
            let commands = [command];
            // Presentation may continue using its wait semaphore after the
            // render fence signals. Indexing by acquired image guarantees that
            // the image has returned from presentation before reusing it.
            let render_complete = self.render_finished[image_index as usize];
            let mut signals = vec![render_complete];
            let mut signal_values = vec![0];
            let hardware_video = video_parts.as_ref().is_some_and(|(_, video)| {
                video_copies.iter().any(|(index, _)| {
                    video
                        .streams
                        .get(&self.surfaces[*index].connection)
                        .is_some_and(|stream| stream.cache.is_some())
                })
            });
            if hardware_video {
                let video = &mut video_parts
                    .as_mut()
                    .expect("hardware video requires a WebRTC compositor")
                    .1;
                video.ready_value += 1;
                signals.push(video.ready.expect("video ready semaphore"));
                signal_values.push(video.ready_value);
            }
            let mut releases = Vec::new();
            for (index, surface) in self.surfaces.iter().enumerate() {
                if let Some(frame) = surface.latest
                    && frame.frame_number > surface.last_released_frame
                    && let Some(semaphore) = surface.imported.consumer_timeline()
                {
                    signals.push(semaphore);
                    signal_values.push(frame.sync_value);
                    releases.push((index, frame.frame_number));
                }
            }
            let mut timeline_submit = vk::TimelineSemaphoreSubmitInfo::default()
                .wait_semaphore_values(&wait_values)
                .signal_semaphore_values(&signal_values);
            let acquire_syncs: Vec<_> = self
                .surfaces
                .iter()
                .filter_map(|surface| {
                    let frame = surface.latest?;
                    surface
                        .imported
                        .uses_keyed_mutex()
                        .then(|| surface.imported.memory(frame.slot))
                        .flatten()
                })
                .collect();
            let acquire_keys = vec![1; acquire_syncs.len()];
            let acquire_timeouts = vec![u32::MAX; acquire_syncs.len()];
            let release_syncs = acquire_syncs.clone();
            let release_keys = vec![0; release_syncs.len()];
            let mut keyed_mutex = vk::Win32KeyedMutexAcquireReleaseInfoKHR::default()
                .acquire_syncs(&acquire_syncs)
                .acquire_keys(&acquire_keys)
                .acquire_timeouts(&acquire_timeouts)
                .release_syncs(&release_syncs)
                .release_keys(&release_keys);
            let mut submit = vk::SubmitInfo::default()
                .push_next(&mut timeline_submit)
                .wait_semaphores(&waits)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&commands)
                .signal_semaphores(&signals);
            if !acquire_syncs.is_empty() {
                submit = submit.push_next(&mut keyed_mutex);
            }
            self.device
                .queue_submit(self.queue, &[submit], self.fences[flight])?;
            for (index, frame) in video_copies {
                let Some((video_swapchain, video)) = video_parts.as_mut() else {
                    continue;
                };
                let connection = self.surfaces[index].connection;
                let cached_image = video
                    .streams
                    .get(&connection)
                    .and_then(|stream| stream.cache.as_ref())
                    .map(|cache| cache.image);
                if let Some(image) = cached_image {
                    video.completion_value += 1;
                    let completion_value = video.completion_value;
                    if let Some(stream) = video.streams.get_mut(&connection) {
                        stream.completion_value = completion_value;
                    }
                    let frame = webrtc::VideoFrame {
                        connection,
                        width: self.surfaces[index].imported.width,
                        height: self.surfaces[index].imported.height,
                        data: webrtc::VideoFrameData::Vulkan(webrtc::VulkanFrame {
                            instance: self.instance.clone(),
                            physical_device: self.physical_device,
                            device: self.device.clone(),
                            queue: video.queue.expect("video queue"),
                            queue_family: video.queue_family.expect("video queue family"),
                            conversion_queue: video
                                .conversion_queue
                                .expect("video conversion queue"),
                            conversion_queue_family: video.conversion_queue_family,
                            image,
                            format: self.surfaces[index].imported.format,
                            layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                            ready: video.ready.expect("video ready semaphore"),
                            ready_value: video.ready_value,
                            completion: video.completion.expect("video completion semaphore"),
                            completion_value,
                            completion_submitted: false,
                        }),
                    };
                    let _ = video_swapchain.present(frame);
                } else if let Some(readback) = video
                    .streams
                    .get_mut(&connection)
                    .and_then(|stream| stream.readbacks.get_mut(flight))
                {
                    readback.pending = Some(frame);
                }
            }
            for (index, frame_number) in releases {
                self.surfaces[index].last_released_frame = frame_number;
            }
            let swapchains = [self.swapchain];
            let indices = [image_index];
            let present_suboptimal = match self.swapchain_loader.queue_present(
                self.queue,
                &vk::PresentInfoKHR::default()
                    .wait_semaphores(&signals[..1])
                    .swapchains(&swapchains)
                    .image_indices(&indices),
            ) {
                Ok(suboptimal) => suboptimal,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => true,
                Err(error) => return Err(error.into()),
            };
            self.swapchain_initialized[image_index as usize] = true;
            for surface in &mut self.surfaces {
                if surface.imported.uses_keyed_mutex() {
                    surface.latest = None;
                }
            }
            self.frame_index += 1;
            Ok(!(acquire_suboptimal || present_suboptimal))
        }
    }

    pub fn present_with_webrtc(
        &mut self,
        plan: &CompositingPlan,
        webrtc: Option<&mut WebRtcCompositor>,
    ) -> Result<bool, Box<dyn Error>> {
        self.present_with(plan, webrtc, |_| Ok(()))
    }

    pub fn present(&mut self, plan: &CompositingPlan) -> Result<bool, Box<dyn Error>> {
        self.present_with_webrtc(plan, None)
    }
}

impl WebRtcCompositor {
    /// Adds one of a Vulkan receiver's imported streams to the video
    /// swapchain. The window and WebRTC compositors remain sibling consumers
    /// of the same imported surface.
    pub fn import_vulkan_stream(
        &mut self,
        compositor: &VulkanCompositor,
        connection: u64,
    ) -> Result<(), Box<dyn Error>> {
        let surface = compositor
            .surfaces
            .iter()
            .find(|surface| surface.connection == connection)
            .ok_or_else(|| format!("Vulkan stream {connection} is not imported"))?;
        let Some((_, video)) = self.vulkan_parts_mut() else {
            return Err("WebRTC compositor is not attached to Vulkan".into());
        };
        if video.physical_device != compositor.physical_device {
            return Err("WebRTC compositor belongs to a different Vulkan device".into());
        }
        if video.streams.contains_key(&connection) {
            return Ok(());
        }
        let readbacks = if video.queue.is_none() {
            unsafe {
                create_vulkan_video_readbacks(
                    &compositor.instance,
                    &compositor.device,
                    compositor.physical_device,
                    surface.imported.width,
                    surface.imported.height,
                    surface.imported.format,
                )?
            }
        } else {
            Vec::new()
        };
        let cache = if video.queue.is_some() {
            Some(unsafe {
                create_vulkan_video_cache(
                    &compositor.instance,
                    &compositor.device,
                    compositor.physical_device,
                    surface.imported.width,
                    surface.imported.height,
                    surface.imported.format,
                    compositor.queue_family,
                    video.queue_family.expect("video queue is present"),
                )?
            })
        } else {
            None
        };
        video.streams.insert(
            connection,
            VulkanWebRtcStream {
                readbacks,
                cache,
                completion_value: 0,
            },
        );
        Ok(())
    }
}

impl Drop for VulkanCompositor {
    fn drop(&mut self) {
        let _ = self.release_imported_streams();
        unsafe {
            let _ = self.device.wait_for_fences(&self.fences, true, u64::MAX);
            let _ = self.device.device_wait_idle();
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

fn physical_device_matches_luid(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    luid: AdapterLuid,
) -> bool {
    let mut id = vk::PhysicalDeviceIDProperties::default();
    let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
    unsafe { instance.get_physical_device_properties2(physical_device, &mut properties) };
    let expected = [luid.low_part.to_le_bytes(), luid.high_part.to_le_bytes()].concat();
    id.device_luid_valid == vk::TRUE && id.device_luid[..8] == expected
}

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

pub(crate) unsafe fn queue_family_video_operations(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
) -> Vec<vk::VideoCodecOperationFlagsKHR> {
    unsafe {
        let count = instance.get_physical_device_queue_family_properties2_len(physical_device);
        let mut video = vec![vk::QueueFamilyVideoPropertiesKHR::default(); count];
        let mut properties = vec![vk::QueueFamilyProperties2::default(); count];
        for (properties, video) in properties.iter_mut().zip(&mut video) {
            properties.p_next = (video as *mut vk::QueueFamilyVideoPropertiesKHR<'_>).cast();
        }
        instance.get_physical_device_queue_family_properties2(physical_device, &mut properties);
        video
            .into_iter()
            .map(|properties| properties.video_codec_operations)
            .collect()
    }
}

unsafe fn create_vulkan_video_cache(
    instance: &ash::Instance,
    device: &ash::Device,
    physical_device: vk::PhysicalDevice,
    width: u32,
    height: u32,
    format: vk::Format,
    graphics_family: u32,
    video_family: u32,
) -> Result<VulkanVideoCache, vk::Result> {
    let families = [graphics_family, video_family];
    let image = unsafe {
        device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(
                    vk::ImageUsageFlags::TRANSFER_DST
                        | vk::ImageUsageFlags::TRANSFER_SRC
                        | vk::ImageUsageFlags::SAMPLED,
                )
                .sharing_mode(vk::SharingMode::CONCURRENT)
                .queue_family_indices(&families)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        )?
    };
    let requirements = unsafe { device.get_image_memory_requirements(image) };
    let properties = unsafe { instance.get_physical_device_memory_properties(physical_device) };
    let memory_type = properties.memory_types[..properties.memory_type_count as usize]
        .iter()
        .enumerate()
        .find(|(index, memory)| {
            requirements.memory_type_bits & (1 << index) != 0
                && memory
                    .property_flags
                    .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        })
        .map(|(index, _)| index as u32)
        .ok_or(vk::Result::ERROR_FEATURE_NOT_PRESENT)?;
    let memory = unsafe {
        device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type),
            None,
        )?
    };
    unsafe { device.bind_image_memory(image, memory, 0)? };
    Ok(VulkanVideoCache {
        image,
        memory,
        initialized: false,
    })
}

unsafe fn create_vulkan_video_readbacks(
    instance: &ash::Instance,
    device: &ash::Device,
    physical_device: vk::PhysicalDevice,
    width: u32,
    height: u32,
    format: vk::Format,
) -> Result<Vec<VulkanVideoReadback>, vk::Result> {
    if !matches!(
        format,
        vk::Format::R8G8B8A8_UNORM
            | vk::Format::R8G8B8A8_SRGB
            | vk::Format::B8G8R8A8_UNORM
            | vk::Format::B8G8R8A8_SRGB
            | vk::Format::A2B10G10R10_UNORM_PACK32
    ) {
        tracing::error!(
            "snowglass: WebRTC does not support Vulkan format {}",
            format.as_raw()
        );
        return Ok(Vec::new());
    }
    let size = width as u64 * height as u64 * 4;
    let properties = unsafe { instance.get_physical_device_memory_properties(physical_device) };
    let mut readbacks = Vec::with_capacity(FRAMES_IN_FLIGHT);
    for _ in 0..FRAMES_IN_FLIGHT {
        let buffer = unsafe {
            device.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size)
                    .usage(vk::BufferUsageFlags::TRANSFER_DST)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )?
        };
        let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
        let memory_type = properties.memory_types[..properties.memory_type_count as usize]
            .iter()
            .enumerate()
            .find(|(index, memory)| {
                requirements.memory_type_bits & (1 << index) != 0
                    && memory.property_flags.contains(
                        vk::MemoryPropertyFlags::HOST_VISIBLE
                            | vk::MemoryPropertyFlags::HOST_COHERENT,
                    )
            })
            .map(|(index, _)| index as u32)
            .ok_or(vk::Result::ERROR_FEATURE_NOT_PRESENT)?;
        let memory = unsafe {
            device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(memory_type),
                None,
            )?
        };
        unsafe { device.bind_buffer_memory(buffer, memory, 0)? };
        readbacks.push(VulkanVideoReadback {
            buffer,
            memory,
            pending: None,
        });
    }
    Ok(readbacks)
}

fn vulkan_pixels_to_rgba(format: vk::Format, packed: &[u8]) -> Vec<u8> {
    if format == vk::Format::A2B10G10R10_UNORM_PACK32 {
        let mut output = Vec::with_capacity(packed.len());
        for bytes in packed.chunks_exact(4) {
            let pixel = u32::from_le_bytes(bytes.try_into().expect("four-byte pixel"));
            output.extend_from_slice(&[
                ((pixel & 0x3ff) * 255 / 1023) as u8,
                (((pixel >> 10) & 0x3ff) * 255 / 1023) as u8,
                (((pixel >> 20) & 0x3ff) * 255 / 1023) as u8,
                255,
            ]);
        }
        output
    } else if matches!(
        format,
        vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB
    ) {
        packed
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], pixel[3]])
            .collect()
    } else {
        packed.to_vec()
    }
}

unsafe fn create_swapchain_views(
    device: &ash::Device,
    images: &[vk::Image],
    format: vk::Format,
) -> Result<Vec<vk::ImageView>, vk::Result> {
    images
        .iter()
        .map(|&image| unsafe {
            device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(color_range()),
                None,
            )
        })
        .collect()
}

unsafe fn create_framebuffers(
    device: &ash::Device,
    render_pass: vk::RenderPass,
    views: &[vk::ImageView],
    extent: vk::Extent2D,
) -> Result<Vec<vk::Framebuffer>, vk::Result> {
    views
        .iter()
        .map(|view| unsafe {
            device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(render_pass)
                    .attachments(std::slice::from_ref(view))
                    .width(extent.width)
                    .height(extent.height)
                    .layers(1),
                None,
            )
        })
        .collect()
}

unsafe fn create_composition_pipeline(
    device: &ash::Device,
    render_pass: vk::RenderPass,
) -> Result<CompositionPipeline, Box<dyn Error>> {
    unsafe {
        let binding = vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT);
        let descriptor_set_layout = device.create_descriptor_set_layout(
            &vk::DescriptorSetLayoutCreateInfo::default().bindings(std::slice::from_ref(&binding)),
            None,
        )?;
        let push_range = vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .size(size_of::<SurfacePush>() as u32);
        let pipeline_layout = device.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default()
                .set_layouts(std::slice::from_ref(&descriptor_set_layout))
                .push_constant_ranges(std::slice::from_ref(&push_range)),
            None,
        )?;
        let sampler = device.create_sampler(
            &vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .max_lod(0.0),
            None,
        )?;
        let pool_size = vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(SURFACE_DESCRIPTOR_COUNT);
        let descriptor_pool = device.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default()
                .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                .max_sets(SURFACE_DESCRIPTOR_COUNT)
                .pool_sizes(std::slice::from_ref(&pool_size)),
            None,
        )?;

        let vertex_words = ash::util::read_spv(&mut std::io::Cursor::new(include_bytes!(
            "../../surface.vert.spv"
        )))?;
        let fragment_words = ash::util::read_spv(&mut std::io::Cursor::new(include_bytes!(
            "../../surface.frag.spv"
        )))?;
        let vertex_module = device.create_shader_module(
            &vk::ShaderModuleCreateInfo::default().code(&vertex_words),
            None,
        )?;
        let fragment_module = device.create_shader_module(
            &vk::ShaderModuleCreateInfo::default().code(&fragment_words),
            None,
        )?;
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vertex_module)
                .name(c"main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fragment_module)
                .name(c"main"),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let make_pipeline = |blending| {
            let attachment = vk::PipelineColorBlendAttachmentState::default()
                .blend_enable(blending)
                .src_color_blend_factor(vk::BlendFactor::ONE)
                .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .color_blend_op(vk::BlendOp::ADD)
                .src_alpha_blend_factor(vk::BlendFactor::ONE)
                .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
                .alpha_blend_op(vk::BlendOp::ADD)
                .color_write_mask(vk::ColorComponentFlags::RGBA);
            let color_blend = vk::PipelineColorBlendStateCreateInfo::default()
                .attachments(std::slice::from_ref(&attachment));
            device
                .create_graphics_pipelines(
                    vk::PipelineCache::null(),
                    &[vk::GraphicsPipelineCreateInfo::default()
                        .stages(&stages)
                        .vertex_input_state(&vertex_input)
                        .input_assembly_state(&assembly)
                        .viewport_state(&viewport)
                        .rasterization_state(&raster)
                        .multisample_state(&multisample)
                        .color_blend_state(&color_blend)
                        .dynamic_state(&dynamic)
                        .layout(pipeline_layout)
                        .render_pass(render_pass)
                        .subpass(0)],
                    None,
                )
                .map(|pipelines| pipelines[0])
                .map_err(|(_, error)| error)
        };
        let opaque_pipeline = make_pipeline(false)?;
        let blend_pipeline = make_pipeline(true)?;
        device.destroy_shader_module(fragment_module, None);
        device.destroy_shader_module(vertex_module, None);
        Ok(CompositionPipeline {
            descriptor_set_layout,
            pipeline_layout,
            opaque_pipeline,
            blend_pipeline,
            descriptor_pool,
            sampler,
        })
    }
}

unsafe fn create_surface_samples(
    device: &ash::Device,
    imported: &ImportedImageRing,
    slot_count: u32,
    pipeline: &CompositionPipeline,
) -> Result<(Vec<vk::ImageView>, Vec<vk::DescriptorSet>), vk::Result> {
    unsafe {
        let mut views = Vec::with_capacity(slot_count as usize);
        for slot in 0..slot_count {
            let Some(image) = imported.image(slot) else {
                for view in views {
                    device.destroy_image_view(view, None);
                }
                return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
            };
            match device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(imported.format)
                    .subresource_range(color_range()),
                None,
            ) {
                Ok(view) => views.push(view),
                Err(error) => {
                    for view in views {
                        device.destroy_image_view(view, None);
                    }
                    return Err(error);
                }
            }
        }
        let layouts = vec![pipeline.descriptor_set_layout; views.len()];
        let sets = match device.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(pipeline.descriptor_pool)
                .set_layouts(&layouts),
        ) {
            Ok(sets) => sets,
            Err(error) => {
                for view in views {
                    device.destroy_image_view(view, None);
                }
                return Err(error);
            }
        };
        let images = views
            .iter()
            .map(|&view| {
                vk::DescriptorImageInfo::default()
                    .sampler(pipeline.sampler)
                    .image_view(view)
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            })
            .collect::<Vec<_>>();
        let writes = sets
            .iter()
            .zip(&images)
            .map(|(&set, image)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(std::slice::from_ref(image))
            })
            .collect::<Vec<_>>();
        device.update_descriptor_sets(&writes, &[]);
        Ok((views, sets))
    }
}

unsafe fn destroy_surface_samples(
    device: &ash::Device,
    pool: vk::DescriptorPool,
    views: &mut Vec<vk::ImageView>,
    sets: &mut Vec<vk::DescriptorSet>,
) {
    unsafe {
        if !sets.is_empty() {
            let _ = device.free_descriptor_sets(pool, sets);
            sets.clear();
        }
        for view in views.drain(..) {
            device.destroy_image_view(view, None);
        }
    }
}

unsafe fn draw_surface_content(
    device: &ash::Device,
    command: vk::CommandBuffer,
    extent: vk::Extent2D,
    pipeline: &CompositionPipeline,
    plan: &CompositingPlan,
    draws: &[SurfaceDraw],
) {
    unsafe {
        device.cmd_set_scissor(
            command,
            0,
            &[vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent,
            }],
        );
        let mut blending = None;
        for placement in plan.placements() {
            let Some(draw) = draws.iter().find(|draw| {
                draw.placement.connection == placement.connection
                    && draw.placement.surface_id == placement.surface_id
            }) else {
                continue;
            };
            let content = draw.placement.content;
            let blend = content.opacity < 1.0;
            if blending != Some(blend) {
                blending = Some(blend);
                device.cmd_bind_pipeline(
                    command,
                    vk::PipelineBindPoint::GRAPHICS,
                    if blend {
                        pipeline.blend_pipeline
                    } else {
                        pipeline.opaque_pipeline
                    },
                );
            }
            device.cmd_set_viewport(
                command,
                0,
                &[vk::Viewport {
                    x: content.destination[0],
                    y: content.destination[1],
                    width: content.destination[2] - content.destination[0],
                    height: content.destination[3] - content.destination[1],
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            device.cmd_bind_descriptor_sets(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                pipeline.pipeline_layout,
                0,
                &[draw.descriptor_set],
                &[],
            );
            device.cmd_push_constants(
                command,
                pipeline.pipeline_layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                std::slice::from_raw_parts(
                    (&draw.push as *const SurfacePush).cast::<u8>(),
                    size_of::<SurfacePush>(),
                ),
            );
            device.cmd_draw(command, 6, 1, 0, 0);
        }
    }
}

fn color_layers() -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: 0,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn texture_coordinate(
        active_uv: [f32; 2],
        source: [f32; 4],
        flip_y: bool,
        quarter_turns: u8,
        corner: [f32; 2],
    ) -> [f32; 2] {
        let top = if flip_y { active_uv[1] } else { 0.0 };
        let bottom = if flip_y { 0.0 } else { active_uv[1] };
        let raw = [
            [0.0, top],
            [active_uv[0], top],
            [active_uv[0], bottom],
            [0.0, bottom],
        ];
        let turns = quarter_turns as usize % 4;
        let full = [
            raw[turns],
            raw[(turns + 1) % 4],
            raw[(turns + 2) % 4],
            raw[(turns + 3) % 4],
        ];
        let oriented = [
            source[0] + (source[2] - source[0]) * corner[0],
            source[1] + (source[3] - source[1]) * corner[1],
        ];
        let top = [
            full[0][0] + (full[1][0] - full[0][0]) * oriented[0],
            full[0][1] + (full[1][1] - full[0][1]) * oriented[0],
        ];
        let bottom = [
            full[3][0] + (full[2][0] - full[3][0]) * oriented[0],
            full[3][1] + (full[2][1] - full[3][1]) * oriented[0],
        ];
        [
            top[0] + (bottom[0] - top[0]) * oriented[1],
            top[1] + (bottom[1] - top[1]) * oriented[1],
        ]
    }

    fn assert_close(actual: [f32; 2], expected: [f32; 2]) {
        assert!((actual[0] - expected[0]).abs() < f32::EPSILON);
        assert!((actual[1] - expected[1]).abs() < f32::EPSILON);
    }

    #[test]
    fn surface_push_layout_matches_glsl_push_block() {
        assert_eq!(size_of::<SurfacePush>(), 32);
        assert_eq!(std::mem::offset_of!(SurfacePush, source), 0);
        assert_eq!(std::mem::offset_of!(SurfacePush, active_uv), 16);
        assert_eq!(std::mem::offset_of!(SurfacePush, opacity), 24);
        assert_eq!(std::mem::offset_of!(SurfacePush, transform), 28);
    }

    #[test]
    fn embedded_surface_shaders_are_spirv() {
        ash::util::read_spv(&mut std::io::Cursor::new(include_bytes!(
            "../../surface.vert.spv"
        )))
        .unwrap();
        ash::util::read_spv(&mut std::io::Cursor::new(include_bytes!(
            "../../surface.frag.spv"
        )))
        .unwrap();
    }

    #[test]
    fn source_crop_is_applied_after_presentation_rotation() {
        let active_uv = [0.8, 0.6];
        let source = [0.25, 0.0, 0.75, 1.0];

        assert_close(
            texture_coordinate(active_uv, source, false, 1, [0.0, 0.0]),
            [0.8, 0.15],
        );
        assert_close(
            texture_coordinate(active_uv, source, false, 1, [1.0, 0.0]),
            [0.8, 0.45],
        );
        assert_close(
            texture_coordinate(active_uv, source, false, 1, [1.0, 1.0]),
            [0.0, 0.45],
        );
    }

    #[test]
    fn vertical_flip_composes_with_half_turn() {
        assert_close(
            texture_coordinate([0.75, 0.5], [0.0, 0.0, 1.0, 1.0], true, 2, [0.0, 0.0]),
            [0.75, 0.0],
        );
        assert_close(
            texture_coordinate([0.75, 0.5], [0.0, 0.0, 1.0, 1.0], true, 2, [1.0, 1.0]),
            [0.0, 0.5],
        );
    }
}
