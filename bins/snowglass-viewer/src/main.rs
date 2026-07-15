use ash::{Entry, vk};
use egui_ash_renderer::{Options as EguiRendererOptions, Renderer as EguiRenderer};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use snowglass_consumer_vulkan::ImportedImageRing;
use snowglass_ipc::{LocalSocketListener, Message, StreamReady};
use snowglass_platform_windows::{
    ProcessId, hide_top_level_windows, performance_counter, performance_frequency,
};
use snowglass_protocol::{FrameReady, Handshake, PROTOCOL_VERSION};
use std::{
    env,
    error::Error,
    path::Path,
    process::{Child, Command},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use winit::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::Window,
};

const FRAMES_IN_FLIGHT: usize = 2;

enum CaptureEvent {
    Stream(StreamReady),
    Frame(FrameReady),
    Disconnected,
}

#[derive(Default)]
struct Statistics {
    frame_number: u64,
    slot: u32,
    fps: f64,
    latency_ms: f64,
    received: u64,
    dropped: u64,
    frames_since_sample: u64,
    sample_started: Option<Instant>,
}

struct DirectCompositor {
    _entry: Entry,
    instance: ash::Instance,
    surface_loader: ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
    physical_device: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    swapchain_loader: ash::khr::swapchain::Device,
    swapchain: vk::SwapchainKHR,
    swapchain_images: Vec<vk::Image>,
    swapchain_views: Vec<vk::ImageView>,
    swapchain_initialized: Vec<bool>,
    extent: vk::Extent2D,
    render_pass: vk::RenderPass,
    framebuffers: Vec<vk::Framebuffer>,
    command_pool: vk::CommandPool,
    command_buffers: Vec<vk::CommandBuffer>,
    image_available: Vec<vk::Semaphore>,
    render_finished: Vec<vk::Semaphore>,
    fences: Vec<vk::Fence>,
    frame_index: u64,
    imported: Option<ImportedImageRing>,
    latest: Option<FrameReady>,
    egui_renderer: Option<EguiRenderer>,
}

impl DirectCompositor {
    fn new(window: &Window) -> Result<Self, Box<dyn Error>> {
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
            let required = [
                ash::khr::swapchain::NAME,
                ash::khr::external_memory_win32::NAME,
                ash::khr::external_semaphore_win32::NAME,
                ash::khr::timeline_semaphore::NAME,
            ];
            let (physical_device, queue_family) = instance
                .enumerate_physical_devices()?
                .into_iter()
                .find_map(|physical| {
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
                    instance
                        .get_physical_device_queue_family_properties(physical)
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
                            (graphics && present).then_some((physical, index as u32))
                        })
                })
                .ok_or("no Vulkan device supports capture import and presentation")?;

            let priorities = [1.0];
            let queue_info = vk::DeviceQueueCreateInfo::default()
                .queue_family_index(queue_family)
                .queue_priorities(&priorities);
            let extension_names = required.map(|name| name.as_ptr());
            let mut timeline_features =
                vk::PhysicalDeviceTimelineSemaphoreFeatures::default().timeline_semaphore(true);
            let device = instance.create_device(
                physical_device,
                &vk::DeviceCreateInfo::default()
                    .queue_create_infos(std::slice::from_ref(&queue_info))
                    .enabled_extension_names(&extension_names)
                    .push_next(&mut timeline_features),
                None,
            )?;
            let queue = device.get_device_queue(queue_family, 0);
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
            let requested = window.inner_size();
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
                render_finished
                    .push(device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?);
                fences.push(device.create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )?);
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
                swapchain_loader,
                swapchain,
                swapchain_images,
                swapchain_views,
                swapchain_initialized: vec![false; initialized_count],
                extent,
                render_pass,
                framebuffers,
                command_pool,
                command_buffers,
                image_available,
                render_finished,
                fences,
                frame_index: 0,
                imported: None,
                latest: None,
                egui_renderer: Some(egui_renderer),
            })
        }
    }

    fn import_stream(&mut self, stream: &StreamReady) -> Result<(), Box<dyn Error>> {
        unsafe { self.device.device_wait_idle()? };
        self.imported = Some(ImportedImageRing::import_into(
            &self.instance,
            self.device.clone(),
            self.physical_device,
            stream,
        )?);
        self.latest = None;
        Ok(())
    }

    fn set_latest(&mut self, frame: FrameReady) {
        self.latest = Some(frame);
    }

    fn draw(
        &mut self,
        output: &egui::FullOutput,
        primitives: &[egui::ClippedPrimitive],
    ) -> Result<(), Box<dyn Error>> {
        // SAFETY: frame fences protect command and semaphore reuse; acquisition
        // owns the destination image; the external timeline gates source reads.
        unsafe {
            let flight = self.frame_index as usize % FRAMES_IN_FLIGHT;
            self.device
                .wait_for_fences(&[self.fences[flight]], true, u64::MAX)?;
            self.egui_renderer
                .as_mut()
                .expect("egui renderer is live")
                .free_textures(&output.textures_delta.free)?;
            let (image_index, _) = self.swapchain_loader.acquire_next_image(
                self.swapchain,
                u64::MAX,
                self.image_available[flight],
                vk::Fence::null(),
            )?;
            self.device.reset_fences(&[self.fences[flight]])?;
            self.egui_renderer
                .as_mut()
                .expect("egui renderer is live")
                .set_textures(self.queue, self.command_pool, &output.textures_delta.set)?;
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
            self.device.cmd_clear_color_image(
                command,
                destination,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &vk::ClearColorValue {
                    float32: [0.0, 0.0, 0.0, 1.0],
                },
                &[color_range()],
            );
            if let (Some(imported), Some(frame)) = (&self.imported, self.latest) {
                if let Some(source) = imported.image(frame.slot) {
                    let to_source = vk::ImageMemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                        .old_layout(vk::ImageLayout::GENERAL)
                        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .image(source)
                        .subresource_range(color_range());
                    self.device.cmd_pipeline_barrier(
                        command,
                        vk::PipelineStageFlags::ALL_COMMANDS,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &[to_source],
                    );
                    let (offset_x, offset_y, width, height) = fit_extent(
                        imported.width,
                        imported.height,
                        self.extent.width,
                        self.extent.height,
                    );
                    self.device.cmd_blit_image(
                        command,
                        source,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        destination,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &[vk::ImageBlit::default()
                            .src_subresource(color_layers())
                            .src_offsets([
                                vk::Offset3D::default(),
                                vk::Offset3D {
                                    x: imported.width as i32,
                                    y: imported.height as i32,
                                    z: 1,
                                },
                            ])
                            .dst_subresource(color_layers())
                            .dst_offsets([
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
                            ])],
                        vk::Filter::LINEAR,
                    );
                    let source_to_general = vk::ImageMemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                        .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                        .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .new_layout(vk::ImageLayout::GENERAL)
                        .image(source)
                        .subresource_range(color_range());
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
            if let (Some(imported), Some(frame)) = (&self.imported, self.latest) {
                waits.push(imported.timeline());
                wait_values.push(frame.sync_value);
                wait_stages.push(vk::PipelineStageFlags::TRANSFER);
            }
            let commands = [command];
            let signals = [self.render_finished[flight]];
            let signal_values = [0];
            let mut timeline_submit = vk::TimelineSemaphoreSubmitInfo::default()
                .wait_semaphore_values(&wait_values)
                .signal_semaphore_values(&signal_values);
            let submit = vk::SubmitInfo::default()
                .push_next(&mut timeline_submit)
                .wait_semaphores(&waits)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&commands)
                .signal_semaphores(&signals);
            self.device
                .queue_submit(self.queue, &[submit], self.fences[flight])?;
            let swapchains = [self.swapchain];
            let indices = [image_index];
            self.swapchain_loader.queue_present(
                self.queue,
                &vk::PresentInfoKHR::default()
                    .wait_semaphores(&signals)
                    .swapchains(&swapchains)
                    .image_indices(&indices),
            )?;
            self.swapchain_initialized[image_index as usize] = true;
            self.frame_index += 1;
            Ok(())
        }
    }
}

impl Drop for DirectCompositor {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.imported = None;
            self.egui_renderer = None;
            for fence in self.fences.drain(..) {
                self.device.destroy_fence(fence, None);
            }
            for semaphore in self.render_finished.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
            }
            for semaphore in self.image_available.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
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

fn main() -> Result<(), Box<dyn Error>> {
    let target = parse_target()?;
    let executable_dir = env::current_exe()?
        .parent()
        .ok_or("viewer executable has no parent directory")?
        .to_path_buf();
    let session = format!("snowglass-{}", std::process::id());
    let listener = LocalSocketListener::bind(&session)?;
    let child = launch_target(&target, &executable_dir, &session)?;
    let process_id = ProcessId(child.id());
    let stop = Arc::new(AtomicBool::new(false));
    let (target_done_tx, target_done_rx) = mpsc::channel();
    spawn_target_monitor(child, process_id, stop.clone(), target_done_tx);
    let (capture_tx, capture_rx) = mpsc::sync_channel(1);
    spawn_ipc(listener, capture_tx);

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let window = event_loop.create_window(
        Window::default_attributes()
            .with_title("Snowglass")
            .with_inner_size(LogicalSize::new(1280, 720))
            .with_resizable(false),
    )?;
    let egui_context = egui::Context::default();
    let mut egui_state = egui_winit::State::new(
        egui_context.clone(),
        egui::ViewportId::ROOT,
        &window,
        Some(window.scale_factor() as f32),
        window.theme(),
        None,
    );
    let mut compositor = DirectCompositor::new(&window)?;
    let frequency = performance_frequency().unwrap_or(1) as f64;
    let mut stats = Statistics {
        sample_started: Some(Instant::now()),
        ..Default::default()
    };

    event_loop.run(move |event, target| match event {
        Event::AboutToWait => window.request_redraw(),
        Event::WindowEvent { event, .. } => {
            let response = egui_state.on_window_event(&window, &event);
            if response.repaint {
                window.request_redraw();
            }
            match event {
                WindowEvent::CloseRequested => {
                    stop.store(true, Ordering::Release);
                    target.exit();
                }
                WindowEvent::RedrawRequested => {
                    if target_done_rx.try_recv().is_ok() {
                        target.exit();
                        return;
                    }
                    while let Ok(capture) = capture_rx.try_recv() {
                        match capture {
                            CaptureEvent::Stream(stream) => {
                                if let Err(error) = compositor.import_stream(&stream) {
                                    eprintln!("snowglass: direct import failed: {error}");
                                }
                            }
                            CaptureEvent::Frame(frame) => {
                                if stats.received != 0
                                    && frame.frame_number > stats.frame_number + 1
                                {
                                    stats.dropped += frame.frame_number - stats.frame_number - 1;
                                }
                                stats.received += 1;
                                stats.frames_since_sample += 1;
                                stats.frame_number = frame.frame_number;
                                stats.slot = frame.slot;
                                stats.latency_ms = performance_counter()
                                    .unwrap_or(frame.timestamp_qpc)
                                    .saturating_sub(frame.timestamp_qpc)
                                    as f64
                                    * 1000.0
                                    / frequency;
                                compositor.set_latest(frame);
                            }
                            CaptureEvent::Disconnected => {}
                        }
                    }
                    if let Some(started) = stats.sample_started {
                        if started.elapsed() >= Duration::from_millis(500) {
                            stats.fps =
                                stats.frames_since_sample as f64 / started.elapsed().as_secs_f64();
                            stats.frames_since_sample = 0;
                            stats.sample_started = Some(Instant::now());
                        }
                    }
                    let input = egui_state.take_egui_input(&window);
                    let output = egui_context.run(input, |context| {
                        egui::TopBottomPanel::top("statistics").show(context, |ui| {
                            ui.horizontal(|ui| {
                                ui.strong("Snowglass Vulkan Direct");
                                ui.label(format!("Frame {}", stats.frame_number));
                                ui.label(format!("Slot {}", stats.slot));
                                ui.label(format!("{:.1} FPS", stats.fps));
                                ui.label(format!("Latency {:.2} ms", stats.latency_ms));
                                ui.label(format!("Dropped {}", stats.dropped));
                            });
                        });
                    });
                    egui_state.handle_platform_output(&window, output.platform_output.clone());
                    let primitives =
                        egui_context.tessellate(output.shapes.clone(), output.pixels_per_point);
                    if let Err(error) = compositor.draw(&output, &primitives) {
                        eprintln!("snowglass: direct composition failed: {error}");
                        target.exit();
                    }
                }
                _ => {}
            }
        }
        _ => {}
    })?;
    Ok(())
}

fn spawn_ipc(listener: LocalSocketListener, events: mpsc::SyncSender<CaptureEvent>) {
    thread::spawn(move || {
        let result = (|| -> std::io::Result<()> {
            let mut socket = listener.accept()?;
            socket.send(Message::Hello(Handshake {
                protocol_version: PROTOCOL_VERSION,
                process_id: std::process::id(),
            }))?;
            loop {
                match socket.receive()? {
                    Message::StreamReady(stream) => {
                        events.send(CaptureEvent::Stream(stream)).map_err(|_| {
                            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "viewer closed")
                        })?;
                    }
                    Message::FrameReady(frame) => {
                        let _ = events.try_send(CaptureEvent::Frame(frame));
                    }
                    Message::Shutdown => return Ok(()),
                    Message::Hello(_) => {}
                }
            }
        })();
        if let Err(error) = result {
            eprintln!("snowglass: IPC ended: {error}");
        }
        let _ = events.try_send(CaptureEvent::Disconnected);
    });
}

fn spawn_target_monitor(
    mut child: Child,
    process_id: ProcessId,
    stop: Arc<AtomicBool>,
    done: mpsc::Sender<()>,
) {
    thread::spawn(move || {
        loop {
            if stop.load(Ordering::Acquire) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = done.send(());
                return;
            }
            let _ = hide_top_level_windows(process_id);
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => {
                    let _ = done.send(());
                    return;
                }
                Ok(None) => thread::sleep(Duration::from_millis(8)),
            }
        }
    });
}

fn parse_target() -> Result<Vec<String>, Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    if arguments.next().as_deref() != Some("--") {
        return Err("usage: snowglass-viewer -- <program> [arguments...]".into());
    }
    let target = arguments.collect::<Vec<_>>();
    if target.is_empty() {
        return Err("target program is required".into());
    }
    Ok(target)
}

fn launch_target(target: &[String], layer_dir: &Path, session: &str) -> std::io::Result<Child> {
    Command::new(&target[0])
        .args(&target[1..])
        .env("VK_LAYER_PATH", layer_dir)
        .env("VK_INSTANCE_LAYERS", "VK_LAYER_SNOWGLASS_capture")
        .env("SNOWGLASS_SESSION", session)
        .env("SNOWGLASS_LOG_FILE", layer_dir.join("snowglass-layer.log"))
        .spawn()
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

fn color_layers() -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: 0,
        base_array_layer: 0,
        layer_count: 1,
    }
}

fn fit_extent(
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> (i32, i32, i32, i32) {
    let scale = (target_width as f64 / source_width as f64)
        .min(target_height as f64 / source_height as f64);
    let width = (source_width as f64 * scale).round() as i32;
    let height = (source_height as f64 * scale).round() as i32;
    (
        ((target_width as i32 - width) / 2),
        ((target_height as i32 - height) / 2),
        width,
        height,
    )
}
