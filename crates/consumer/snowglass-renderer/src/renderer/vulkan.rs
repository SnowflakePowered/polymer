use super::RenderWindow;
use ash::{Entry, vk};
use egui_ash_renderer::{Options as EguiRendererOptions, Renderer as EguiRenderer};
use snowglass_consumer_vulkan::ImportedImageRing;
use snowglass_input::mouse::layout::Extent;
use snowglass_ipc::StreamReady;
use snowglass_protocol::{AdapterLuid, FrameReady, RELEASE_ALL_SYNC_VALUE};
use std::error::Error;

use crate::{
    fit_extent, fit_inside, layout, renderer::DisplayedSurface, surface_sort_key,
    vertical_layout_index, webrtc,
};

const FRAMES_IN_FLIGHT: usize = 2;
struct ImportedVulkanSurface {
    connection: u64,
    surface_id: u64,
    target_window: i64,
    imported: ImportedImageRing,
    flip_y: bool,
    quarter_turns: u8,
    rotation: Option<VulkanRotation>,
    latest: Option<FrameReady>,
    last_released_frame: u64,
    video_readbacks: Vec<VulkanVideoReadback>,
    video_cache: Option<VulkanVideoCache>,
    video_completion_value: u64,
}

struct VulkanRotation {
    stage_image: vk::Image,
    stage_memory: vk::DeviceMemory,
    stage_view: vk::ImageView,
    rotated_image: vk::Image,
    rotated_memory: vk::DeviceMemory,
    rotated_view: vk::ImageView,
    descriptor_set: vk::DescriptorSet,
}

struct RotationPipeline {
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    descriptor_pool: vk::DescriptorPool,
}

struct VulkanVideoCache {
    image: vk::Image,
    memory: vk::DeviceMemory,
    initialized: bool,
}

struct VulkanVideoReadback {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    pending: Option<FrameReady>,
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
    video_queue: Option<vk::Queue>,
    video_queue_family: Option<u32>,
    video_conversion_queue: Option<vk::Queue>,
    video_ready: Option<vk::Semaphore>,
    video_completion: Option<vk::Semaphore>,
    video_ready_value: u64,
    video_completion_value: u64,
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
    surfaces: Vec<ImportedVulkanSurface>,
    rotation_pipeline: Option<RotationPipeline>,
    keyed_mutex_enabled: bool,
    egui_renderer: Option<EguiRenderer>,
    publish_video: bool,
    completed_video: Vec<webrtc::VideoFrame>,
}

impl VulkanCompositor {
    pub fn new(
        window: &impl RenderWindow,
        preferred_adapter: Option<AdapterLuid>,
        enable_keyed_mutex: bool,
        publish_video: bool,
    ) -> Result<Self, Box<dyn Error>> {
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
            let image_count =
                (capabilities.min_image_count + 1).min(if capabilities.max_image_count == 0 {
                    u32::MAX
                } else {
                    capabilities.max_image_count
                });
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
            let egui_renderer = EguiRenderer::with_default_allocator(
                &instance,
                physical_device,
                device.clone(),
                render_pass,
                EguiRendererOptions {
                    in_flight_frames: FRAMES_IN_FLIGHT,
                    srgb_framebuffer: false,
                    ..Default::default()
                },
            )?;
            let initialized_count = swapchain_images.len();
            Ok(Self {
                _entry: entry,
                instance,
                surface_loader,
                surface,
                physical_device,
                device,
                queue,
                queue_family,
                video_queue,
                video_queue_family,
                video_conversion_queue,
                video_ready,
                video_completion,
                video_ready_value: 0,
                video_completion_value: 0,
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
                surfaces: Vec::new(),
                rotation_pipeline: None,
                keyed_mutex_enabled: enable_keyed_mutex,
                egui_renderer: Some(egui_renderer),
                publish_video,
                completed_video: Vec::new(),
            })
        }
    }

    pub(crate) fn import_stream(
        &mut self,
        connection: u64,
        stream: &StreamReady,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        self.disconnect_stream(connection)?;
        let imported = ImportedImageRing::import_into(
            &self.instance,
            self.device.clone(),
            self.physical_device,
            stream,
        )?;
        let quarter_turns = quarter_turns % 4;
        let rotation = if quarter_turns % 2 == 1 {
            self.ensure_rotation_pipeline()?;
            let pipeline = self
                .rotation_pipeline
                .as_ref()
                .expect("rotation pipeline was just created");
            Some(unsafe {
                create_vulkan_rotation(
                    &self.instance,
                    &self.device,
                    self.physical_device,
                    imported.width,
                    imported.height,
                    pipeline,
                )?
            })
        } else {
            None
        };
        let video_readbacks = if self.publish_video && self.video_queue.is_none() {
            unsafe {
                create_vulkan_video_readbacks(
                    &self.instance,
                    &self.device,
                    self.physical_device,
                    imported.width,
                    imported.height,
                    imported.format,
                )?
            }
        } else {
            Vec::new()
        };
        let video_cache = if self.publish_video && self.video_queue.is_some() {
            Some(unsafe {
                create_vulkan_video_cache(
                    &self.instance,
                    &self.device,
                    self.physical_device,
                    imported.width,
                    imported.height,
                    imported.format,
                    self.queue_family,
                    self.video_queue_family.expect("video queue is present"),
                )?
            })
        } else {
            None
        };
        self.surfaces.push(ImportedVulkanSurface {
            connection,
            surface_id: stream.surface_id.0,
            target_window: stream.source_window,
            imported,
            flip_y: stream.bottom_up,
            quarter_turns,
            rotation,
            latest: None,
            last_released_frame: 0,
            video_readbacks,
            video_cache,
            video_completion_value: 0,
        });
        self.surfaces
            .sort_by_key(|surface| surface_sort_key(surface.surface_id));
        Ok(())
    }

    fn ensure_rotation_pipeline(&mut self) -> Result<(), Box<dyn Error>> {
        if self.rotation_pipeline.is_some() {
            return Ok(());
        }
        // SAFETY: every created object is owned by the returned pipeline and
        // destroyed together after the device idles; the shader module is
        // consumed by pipeline creation.
        unsafe {
            let bindings = [
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(1)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
            ];
            let descriptor_set_layout = self.device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                None,
            )?;
            let push_range = vk::PushConstantRange::default()
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .size(3 * size_of::<u32>() as u32);
            let pipeline_layout = self.device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(std::slice::from_ref(&descriptor_set_layout))
                    .push_constant_ranges(std::slice::from_ref(&push_range)),
                None,
            )?;
            let words = ash::util::read_spv(&mut std::io::Cursor::new(include_bytes!(
                "../rotate_quarter.comp.spv"
            )))?;
            let module = self
                .device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)?;
            let pipelines = self.device.create_compute_pipelines(
                vk::PipelineCache::null(),
                &[vk::ComputePipelineCreateInfo::default()
                    .stage(
                        vk::PipelineShaderStageCreateInfo::default()
                            .stage(vk::ShaderStageFlags::COMPUTE)
                            .module(module)
                            .name(c"main"),
                    )
                    .layout(pipeline_layout)],
                None,
            );
            self.device.destroy_shader_module(module, None);
            let pipeline = pipelines.map_err(|(_, error)| error)?[0];
            // One set per surface; the surface limit is 16.
            let pool_size = vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(32);
            let descriptor_pool = self.device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                    .max_sets(16)
                    .pool_sizes(std::slice::from_ref(&pool_size)),
                None,
            )?;
            self.rotation_pipeline = Some(RotationPipeline {
                descriptor_set_layout,
                pipeline_layout,
                pipeline,
                descriptor_pool,
            });
        }
        Ok(())
    }

    pub fn matches_adapter(&self, adapter: AdapterLuid) -> bool {
        physical_device_matches_luid(&self.instance, self.physical_device, adapter)
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

    pub(crate) fn displayed_surfaces(&self) -> Vec<DisplayedSurface> {
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
                Some(DisplayedSurface {
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
        let surface = self.surfaces.remove(index);
        unsafe {
            destroy_vulkan_video_readbacks(&self.device, surface.video_readbacks);
            if let Some(cache) = surface.video_cache {
                destroy_vulkan_video_cache(&self.device, cache);
            }
            if let Some(rotation) = surface.rotation {
                let pool = self
                    .rotation_pipeline
                    .as_ref()
                    .expect("rotation images require the rotation pipeline")
                    .descriptor_pool;
                destroy_vulkan_rotation(&self.device, pool, rotation);
            }
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
                destroy_vulkan_video_readbacks(
                    &self.device,
                    std::mem::take(&mut surface.video_readbacks),
                );
                if let Some(cache) = surface.video_cache.take() {
                    destroy_vulkan_video_cache(&self.device, cache);
                }
                if let Some(rotation) = surface.rotation.take() {
                    let pool = self
                        .rotation_pipeline
                        .as_ref()
                        .expect("rotation images require the rotation pipeline")
                        .descriptor_pool;
                    destroy_vulkan_rotation(&self.device, pool, rotation);
                }
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
            let image_count =
                (capabilities.min_image_count + 1).min(if capabilities.max_image_count == 0 {
                    u32::MAX
                } else {
                    capabilities.max_image_count
                });
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

    pub(crate) fn draw(
        &mut self,
        output: &egui::FullOutput,
        primitives: &[egui::ClippedPrimitive],
    ) -> Result<bool, Box<dyn Error>> {
        // SAFETY: frame fences protect command and semaphore reuse; acquisition
        // owns the destination image; the external timeline gates source reads.
        unsafe {
            let flight = self.frame_index as usize % FRAMES_IN_FLIGHT;
            self.device
                .wait_for_fences(&[self.fences[flight]], true, u64::MAX)?;
            for surface in &mut self.surfaces {
                let Some(readback) = surface.video_readbacks.get_mut(flight) else {
                    continue;
                };
                let Some(frame) = readback.pending.take() else {
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
                self.completed_video.push(webrtc::VideoFrame {
                    connection: surface.connection,
                    width: surface.imported.width,
                    height: surface.imported.height,
                    data: webrtc::VideoFrameData::Rgba(vulkan_pixels_to_rgba(
                        surface.imported.format,
                        packed,
                    )),
                });
                self.device.unmap_memory(readback.memory);
                let _ = frame;
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
            self.egui_renderer
                .as_mut()
                .expect("egui renderer is live")
                .free_textures(&output.textures_delta.free)?;
            self.egui_renderer
                .as_mut()
                .expect("egui renderer is live")
                .set_textures(self.queue, self.command_pool, &output.textures_delta.set)?;
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
                        float32: layout::clear_color(),
                    },
                    &[color_range()],
                );
            }
            let visible_count = self
                .surfaces
                .iter()
                .filter(|surface| surface.latest.is_some())
                .count() as u32;
            let reverse_surface_rows = visible_count > 1
                && self
                    .surfaces
                    .iter()
                    .filter(|surface| surface.latest.is_some())
                    .all(|surface| surface.flip_y);
            let cell_height = self.extent.height / visible_count.max(1);
            let layout_images = self
                .surfaces
                .iter()
                .filter_map(|surface| {
                    let frame = surface.latest?;
                    let (width, height) = fit_inside(
                        frame.render_width,
                        frame.render_height,
                        surface.imported.width,
                        surface.imported.height,
                    );
                    Some(if surface.quarter_turns % 2 == 1 {
                        Extent::new(height, width)
                    } else {
                        Extent::new(width, height)
                    })
                })
                .collect::<Vec<_>>();
            let custom_rectangles = layout::active_rectangles(
                Extent::new(self.extent.width, self.extent.height),
                &layout_images,
                reverse_surface_rows,
            );
            let mut visible_index = 0;
            let mut video_copies = Vec::new();
            // A timeline semaphore must be signaled in strictly increasing
            // order, but published frames resolve out of order: dropped frames
            // host-signal immediately while accepted frames signal from the
            // encoder later. Publishing only while no completion value is
            // outstanding keeps the timeline monotonic.
            let video_publish_ready = match self.video_completion {
                Some(semaphore) => {
                    self.device.get_semaphore_counter_value(semaphore)?
                        >= self.video_completion_value
                }
                None => false,
            };
            let mut video_publish_budget = webrtc::FRAME_CHANNEL_CAPACITY;
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
                    let turned = surface.quarter_turns % 2 == 1;
                    let (display_width, display_height) = if turned {
                        (content_height, content_width)
                    } else {
                        (content_width, content_height)
                    };
                    let (offset_x, offset_y, width, height) = if let Some(rect) = custom_rectangles
                        .as_ref()
                        .and_then(|rectangles| rectangles.get(visible_index as usize))
                    {
                        (rect.x, rect.y, rect.width as i32, rect.height as i32)
                    } else {
                        let (x, y, width, height) = fit_extent(
                            display_width,
                            display_height,
                            self.extent.width,
                            cell_height,
                        );
                        let row = vertical_layout_index(
                            visible_index,
                            visible_count,
                            reverse_surface_rows,
                        );
                        (x, y + (row * cell_height) as i32, width, height)
                    };
                    let destination_offsets = [
                        vk::Offset3D {
                            x: offset_x,
                            y: offset_y,
                            z: 0,
                        },
                        vk::Offset3D {
                            x: offset_x + width,
                            y: offset_y + height,
                            z: 1,
                        },
                    ];
                    if let Some(rotation) = &surface.rotation {
                        // Do rotation via compute shader
                        let pipeline = self
                            .rotation_pipeline
                            .as_ref()
                            .expect("rotation images require the rotation pipeline");
                        let stage_to_transfer = vk::ImageMemoryBarrier::default()
                            .src_access_mask(vk::AccessFlags::MEMORY_READ)
                            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                            .old_layout(vk::ImageLayout::UNDEFINED)
                            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                            .image(rotation.stage_image)
                            .subresource_range(color_range());
                        self.device.cmd_pipeline_barrier(
                            command,
                            vk::PipelineStageFlags::ALL_COMMANDS,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::DependencyFlags::empty(),
                            &[],
                            &[],
                            &[stage_to_transfer],
                        );
                        self.device.cmd_blit_image(
                            command,
                            source,
                            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                            rotation.stage_image,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            &[vk::ImageBlit::default()
                                .src_subresource(color_layers())
                                .src_offsets(blit_source_offsets(
                                    content_width,
                                    content_height,
                                    surface.flip_y,
                                    0,
                                ))
                                .dst_subresource(color_layers())
                                .dst_offsets([
                                    vk::Offset3D::default(),
                                    vk::Offset3D {
                                        x: content_width as i32,
                                        y: content_height as i32,
                                        z: 1,
                                    },
                                ])],
                            vk::Filter::NEAREST,
                        );
                        let stage_to_compute = vk::ImageMemoryBarrier::default()
                            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                            .dst_access_mask(vk::AccessFlags::SHADER_READ)
                            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                            .new_layout(vk::ImageLayout::GENERAL)
                            .image(rotation.stage_image)
                            .subresource_range(color_range());
                        let rotated_to_compute = vk::ImageMemoryBarrier::default()
                            .src_access_mask(vk::AccessFlags::MEMORY_READ)
                            .dst_access_mask(vk::AccessFlags::SHADER_WRITE)
                            .old_layout(vk::ImageLayout::UNDEFINED)
                            .new_layout(vk::ImageLayout::GENERAL)
                            .image(rotation.rotated_image)
                            .subresource_range(color_range());
                        self.device.cmd_pipeline_barrier(
                            command,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::PipelineStageFlags::COMPUTE_SHADER,
                            vk::DependencyFlags::empty(),
                            &[],
                            &[],
                            &[stage_to_compute, rotated_to_compute],
                        );
                        self.device.cmd_bind_pipeline(
                            command,
                            vk::PipelineBindPoint::COMPUTE,
                            pipeline.pipeline,
                        );
                        self.device.cmd_bind_descriptor_sets(
                            command,
                            vk::PipelineBindPoint::COMPUTE,
                            pipeline.pipeline_layout,
                            0,
                            &[rotation.descriptor_set],
                            &[],
                        );
                        let push = [
                            content_width,
                            content_height,
                            u32::from(surface.quarter_turns),
                        ];
                        self.device.cmd_push_constants(
                            command,
                            pipeline.pipeline_layout,
                            vk::ShaderStageFlags::COMPUTE,
                            0,
                            std::slice::from_raw_parts(
                                push.as_ptr().cast::<u8>(),
                                size_of_val(&push),
                            ),
                        );
                        self.device.cmd_dispatch(
                            command,
                            content_height.div_ceil(16),
                            content_width.div_ceil(16),
                            1,
                        );
                        let rotated_to_transfer = vk::ImageMemoryBarrier::default()
                            .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                            .old_layout(vk::ImageLayout::GENERAL)
                            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                            .image(rotation.rotated_image)
                            .subresource_range(color_range());
                        self.device.cmd_pipeline_barrier(
                            command,
                            vk::PipelineStageFlags::COMPUTE_SHADER,
                            vk::PipelineStageFlags::TRANSFER,
                            vk::DependencyFlags::empty(),
                            &[],
                            &[],
                            &[rotated_to_transfer],
                        );
                        self.device.cmd_blit_image(
                            command,
                            rotation.rotated_image,
                            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                            destination,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            &[vk::ImageBlit::default()
                                .src_subresource(color_layers())
                                .src_offsets([
                                    vk::Offset3D::default(),
                                    vk::Offset3D {
                                        x: content_height as i32,
                                        y: content_width as i32,
                                        z: 1,
                                    },
                                ])
                                .dst_subresource(color_layers())
                                .dst_offsets(destination_offsets)],
                            vk::Filter::LINEAR,
                        );
                    } else {
                        self.device.cmd_blit_image(
                            command,
                            source,
                            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                            destination,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            &[vk::ImageBlit::default()
                                .src_subresource(color_layers())
                                .src_offsets(blit_source_offsets(
                                    content_width,
                                    content_height,
                                    surface.flip_y,
                                    surface.quarter_turns,
                                ))
                                .dst_subresource(color_layers())
                                .dst_offsets(destination_offsets)],
                            vk::Filter::LINEAR,
                        );
                    }
                    if video_publish_ready
                        && video_publish_budget > 0
                        && let Some(cache) = surface.video_cache.as_mut()
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
                    if let Some(readback) = surface.video_readbacks.get(flight) {
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
                visible_index += 1;
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
            self.egui_renderer
                .as_mut()
                .expect("egui renderer is live")
                .cmd_draw(command, self.extent, output.pixels_per_point, primitives)?;
            self.device.cmd_end_render_pass(command);
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
                    wait_stages.push(vk::PipelineStageFlags::TRANSFER);
                }
            }
            let video_wait_value = self
                .surfaces
                .iter()
                .filter(|surface| surface.video_cache.is_some() && surface.latest.is_some())
                .map(|surface| surface.video_completion_value)
                .max()
                .unwrap_or(0);
            if video_wait_value != 0 {
                waits.push(self.video_completion.expect("video completion semaphore"));
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
            let hardware_video = video_copies
                .iter()
                .any(|(index, _)| self.surfaces[*index].video_cache.is_some());
            if hardware_video {
                self.video_ready_value += 1;
                signals.push(self.video_ready.expect("video ready semaphore"));
                signal_values.push(self.video_ready_value);
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
                if let Some(image) = self.surfaces[index]
                    .video_cache
                    .as_ref()
                    .map(|cache| cache.image)
                {
                    self.video_completion_value += 1;
                    let completion_value = self.video_completion_value;
                    self.surfaces[index].video_completion_value = completion_value;
                    self.completed_video.push(webrtc::VideoFrame {
                        connection: self.surfaces[index].connection,
                        width: self.surfaces[index].imported.width,
                        height: self.surfaces[index].imported.height,
                        data: webrtc::VideoFrameData::Vulkan(webrtc::VulkanFrame {
                            instance: self.instance.clone(),
                            physical_device: self.physical_device,
                            device: self.device.clone(),
                            queue: self.video_queue.expect("video queue"),
                            queue_family: self.video_queue_family.expect("video queue family"),
                            conversion_queue: self
                                .video_conversion_queue
                                .expect("video conversion queue"),
                            conversion_queue_family: self.queue_family,
                            image,
                            format: self.surfaces[index].imported.format,
                            layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                            ready: self.video_ready.expect("video ready semaphore"),
                            ready_value: self.video_ready_value,
                            completion: self.video_completion.expect("video completion semaphore"),
                            completion_value,
                            completion_submitted: false,
                        }),
                    });
                } else {
                    self.surfaces[index].video_readbacks[flight].pending = Some(frame);
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

    pub(crate) fn take_video_frames(&mut self) -> Vec<webrtc::VideoFrame> {
        std::mem::take(&mut self.completed_video)
    }
}

impl Drop for VulkanCompositor {
    fn drop(&mut self) {
        let _ = self.release_imported_streams();
        unsafe {
            let _ = self.device.wait_for_fences(&self.fences, true, u64::MAX);
            let _ = self.device.device_wait_idle();
            self.egui_renderer = None;
            for surface in &mut self.surfaces {
                destroy_vulkan_video_readbacks(
                    &self.device,
                    std::mem::take(&mut surface.video_readbacks),
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
            if let Some(semaphore) = self.video_ready.take() {
                self.device.destroy_semaphore(semaphore, None);
            }
            if let Some(semaphore) = self.video_completion.take() {
                self.device.destroy_semaphore(semaphore, None);
            }
            if let Some(pipeline) = self.rotation_pipeline.take() {
                self.device.destroy_pipeline(pipeline.pipeline, None);
                self.device
                    .destroy_pipeline_layout(pipeline.pipeline_layout, None);
                self.device
                    .destroy_descriptor_set_layout(pipeline.descriptor_set_layout, None);
                self.device
                    .destroy_descriptor_pool(pipeline.descriptor_pool, None);
            }
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

unsafe fn destroy_vulkan_video_cache(device: &ash::Device, cache: VulkanVideoCache) {
    unsafe {
        device.destroy_image(cache.image, None);
        device.free_memory(cache.memory, None);
    }
}

/// Rotation intermediates always use RGBA8: blits convert the imported format
/// on the way in and the swapchain format on the way out, and RGBA8 storage
/// images are universally supported.
const ROTATION_FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;

unsafe fn create_vulkan_rotation(
    instance: &ash::Instance,
    device: &ash::Device,
    physical_device: vk::PhysicalDevice,
    width: u32,
    height: u32,
    pipeline: &RotationPipeline,
) -> Result<VulkanRotation, vk::Result> {
    unsafe {
        let (stage_image, stage_memory) = create_vulkan_rotation_image(
            instance,
            device,
            physical_device,
            width,
            height,
            vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::STORAGE,
        )?;
        let (rotated_image, rotated_memory) = create_vulkan_rotation_image(
            instance,
            device,
            physical_device,
            height,
            width,
            vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;
        let view = |image| {
            device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(ROTATION_FORMAT)
                    .subresource_range(color_range()),
                None,
            )
        };
        let stage_view = view(stage_image)?;
        let rotated_view = view(rotated_image)?;
        let descriptor_set = device.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(pipeline.descriptor_pool)
                .set_layouts(std::slice::from_ref(&pipeline.descriptor_set_layout)),
        )?[0];
        let stage_info = vk::DescriptorImageInfo::default()
            .image_view(stage_view)
            .image_layout(vk::ImageLayout::GENERAL);
        let rotated_info = vk::DescriptorImageInfo::default()
            .image_view(rotated_view)
            .image_layout(vk::ImageLayout::GENERAL);
        device.update_descriptor_sets(
            &[
                vk::WriteDescriptorSet::default()
                    .dst_set(descriptor_set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .image_info(std::slice::from_ref(&stage_info)),
                vk::WriteDescriptorSet::default()
                    .dst_set(descriptor_set)
                    .dst_binding(1)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .image_info(std::slice::from_ref(&rotated_info)),
            ],
            &[],
        );
        Ok(VulkanRotation {
            stage_image,
            stage_memory,
            stage_view,
            rotated_image,
            rotated_memory,
            rotated_view,
            descriptor_set,
        })
    }
}

unsafe fn create_vulkan_rotation_image(
    instance: &ash::Instance,
    device: &ash::Device,
    physical_device: vk::PhysicalDevice,
    width: u32,
    height: u32,
    usage: vk::ImageUsageFlags,
) -> Result<(vk::Image, vk::DeviceMemory), vk::Result> {
    unsafe {
        let image = device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(ROTATION_FORMAT)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(usage)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        )?;
        let requirements = device.get_image_memory_requirements(image);
        let properties = instance.get_physical_device_memory_properties(physical_device);
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
        let memory = device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type),
            None,
        )?;
        device.bind_image_memory(image, memory, 0)?;
        Ok((image, memory))
    }
}

unsafe fn destroy_vulkan_rotation(
    device: &ash::Device,
    descriptor_pool: vk::DescriptorPool,
    rotation: VulkanRotation,
) {
    unsafe {
        let _ = device.free_descriptor_sets(descriptor_pool, &[rotation.descriptor_set]);
        device.destroy_image_view(rotation.stage_view, None);
        device.destroy_image_view(rotation.rotated_view, None);
        device.destroy_image(rotation.stage_image, None);
        device.destroy_image(rotation.rotated_image, None);
        device.free_memory(rotation.stage_memory, None);
        device.free_memory(rotation.rotated_memory, None);
    }
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
        eprintln!(
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

unsafe fn destroy_vulkan_video_readbacks(
    device: &ash::Device,
    readbacks: Vec<VulkanVideoReadback>,
) {
    for readback in readbacks {
        unsafe {
            device.destroy_buffer(readback.buffer, None);
            device.free_memory(readback.memory, None);
        }
    }
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
