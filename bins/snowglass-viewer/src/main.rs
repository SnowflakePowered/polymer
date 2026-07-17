use ash::{Entry, vk};
use egui_ash_renderer::{Options as EguiRendererOptions, Renderer as EguiRenderer};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawWindowHandle};
use snowglass_consumer_vulkan::ImportedImageRing;
use snowglass_input::{DirectInputData, DirectInputDevice};
use snowglass_ipc::{LocalSocketListener, Message, StreamReady};
use snowglass_platform_windows::{
    KillOnCloseJob, NativeWindow, ProcessId, client_size, forward_character, forward_mouse_button,
    forward_mouse_move, forward_mouse_wheel, forward_raw_key, hide_top_level_windows,
    inject_libraries_before_resume, performance_counter, performance_frequency,
};
use snowglass_protocol::{
    AdapterLuid, ConsumerApi, ExternalImageKind, FrameReady, Handshake, PROTOCOL_VERSION,
};
use std::{
    collections::{HashSet, VecDeque},
    env,
    error::Error,
    path::Path,
    process::{Child, Command},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::{
    dpi::LogicalSize,
    event::{DeviceEvent, ElementState, Event, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::Window,
};

mod d3d12;

const FRAMES_IN_FLIGHT: usize = 2;

struct RawKeyboardContext {
    sender: mpsc::Sender<DirectInputData>,
    backing_window: Arc<AtomicIsize>,
    sequence: AtomicU32,
    started: Instant,
    pressed: [bool; 256],
}

struct RawKeyboardForwarder {
    window: isize,
    context: *mut RawKeyboardContext,
}

impl RawKeyboardForwarder {
    fn install(
        window: NativeWindow,
        sender: mpsc::Sender<DirectInputData>,
        backing_window: Arc<AtomicIsize>,
    ) -> std::io::Result<Self> {
        use windows_sys::Win32::UI::{
            Input::{RAWINPUTDEVICE, RIDEV_INPUTSINK, RegisterRawInputDevices},
            Shell::SetWindowSubclass,
        };
        let context = Box::into_raw(Box::new(RawKeyboardContext {
            sender,
            backing_window,
            sequence: AtomicU32::new(0),
            started: Instant::now(),
            pressed: [false; 256],
        }));
        let device = RAWINPUTDEVICE {
            usUsagePage: 1,
            usUsage: 6,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: window.0 as _,
        };
        if unsafe {
            RegisterRawInputDevices(&device, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32)
        } == 0
            || unsafe {
                SetWindowSubclass(
                    window.0 as _,
                    Some(raw_keyboard_subclass),
                    context as usize,
                    context as usize,
                )
            } == 0
        {
            unsafe { drop(Box::from_raw(context)) };
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            window: window.0,
            context,
        })
    }
}

impl Drop for RawKeyboardForwarder {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::Shell::RemoveWindowSubclass;
        unsafe {
            RemoveWindowSubclass(
                self.window as _,
                Some(raw_keyboard_subclass),
                self.context as usize,
            );
            drop(Box::from_raw(self.context));
        }
    }
}

unsafe extern "system" fn raw_keyboard_subclass(
    window: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
    _id: usize,
    context: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::{
        Input::{GetRawInputData, RAWINPUT, RID_INPUT, RIM_TYPEKEYBOARD},
        Shell::DefSubclassProc,
        WindowsAndMessaging::{RI_KEY_BREAK, RI_KEY_E0, RI_KEY_E1, WM_INPUT},
    };
    if message == WM_INPUT {
        let mut input = RAWINPUT::default();
        let mut size = std::mem::size_of::<RAWINPUT>() as u32;
        if unsafe {
            GetRawInputData(
                lparam as _,
                RID_INPUT,
                (&mut input as *mut RAWINPUT).cast(),
                &mut size,
                std::mem::size_of_val(&input.header) as u32,
            )
        } != u32::MAX
            && input.header.dwType == RIM_TYPEKEYBOARD
        {
            let keyboard = unsafe { input.data.keyboard };
            if keyboard.MakeCode != 0 {
                let extended = keyboard.Flags & (RI_KEY_E0 | RI_KEY_E1) as u16 != 0;
                let offset = u32::from(keyboard.MakeCode) | u32::from(extended) * 0x80;
                let context = unsafe { &mut *(context as *mut RawKeyboardContext) };
                let pressed = keyboard.Flags & RI_KEY_BREAK as u16 == 0;
                let repeat = pressed && context.pressed[offset as usize];
                context.pressed[offset as usize] = pressed;
                let sequence = context.sequence.fetch_add(1, Ordering::Relaxed) + 1;
                let _ = context.sender.send(DirectInputData {
                    device: DirectInputDevice::Keyboard,
                    offset,
                    data: if pressed { 0x80 } else { 0 },
                    timestamp: context.started.elapsed().as_millis() as u32,
                    sequence,
                });
                let backing = context.backing_window.load(Ordering::Acquire);
                if backing != 0 {
                    forward_raw_key(
                        NativeWindow(backing),
                        u32::from(keyboard.VKey),
                        keyboard.MakeCode,
                        extended,
                        pressed,
                        repeat,
                    );
                }
            }
        }
    }
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

enum CaptureEvent {
    Stream(u64, StreamReady),
    Frame(u64, FrameReady),
    Disconnected(u64),
}

#[derive(Default)]
struct Statistics {
    frame_number: u64,
    slot: u32,
    render_width: u32,
    render_height: u32,
    capture_x: u32,
    capture_y: u32,
    capture_width: u32,
    capture_height: u32,
    fps: f64,
    latency_ms: f64,
    frame_interval_ms: f64,
    previous_timestamp_qpc: Option<u64>,
    received: u64,
    dropped: u64,
    frames_since_sample: u64,
    sample_started: Option<Instant>,
    history: VecDeque<StatisticsSample>,
}

#[derive(Clone, Copy)]
struct StatisticsSample {
    fps: f32,
    latency_ms: f32,
    frame_interval_ms: f32,
}

struct DirectCompositor {
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
    imported: Option<ImportedImageRing>,
    latest: Option<FrameReady>,
    last_released_frame: u64,
    keyed_mutex_enabled: bool,
    egui_renderer: Option<EguiRenderer>,
}

enum ViewerCompositor {
    Vulkan(Box<DirectCompositor>),
    D3d12(Box<d3d12::D3d12Compositor>),
    Empty,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ViewerApi {
    #[default]
    D3d12,
    Vulkan,
}

impl ViewerApi {
    fn parse(value: &str) -> Result<Self, Box<dyn Error>> {
        match value.to_ascii_lowercase().as_str() {
            "d3d12" => Ok(Self::D3d12),
            "vulkan" => Ok(Self::Vulkan),
            _ => Err(format!("unsupported viewer API '{value}'; expected d3d12 or vulkan").into()),
        }
    }
}

impl ViewerCompositor {
    fn new(window: &Window, api: ViewerApi) -> Result<Self, Box<dyn Error>> {
        match api {
            // D3D12 device selection requires the source adapter LUID, which arrives
            // with StreamReady. Avoid creating and immediately replacing a Vulkan surface.
            ViewerApi::D3d12 => Ok(Self::Empty),
            ViewerApi::Vulkan => Ok(Self::Vulkan(Box::new(DirectCompositor::new(
                window, None, true,
            )?))),
        }
    }

    fn import_stream(
        &mut self,
        window: &Window,
        stream: &StreamReady,
        api: ViewerApi,
    ) -> Result<(), Box<dyn Error>> {
        if api == ViewerApi::D3d12
            && matches!(
                stream.image_kind,
                ExternalImageKind::D3d12Resource
                    | ExternalImageKind::OpenGlD3d12Resource
                    | ExternalImageKind::VulkanD3d12Resource
            )
        {
            let matches =
                matches!(self, Self::D3d12(value) if value.adapter_luid() == stream.adapter_luid);
            if !matches {
                drop(std::mem::replace(self, Self::Empty));
                *self = Self::D3d12(Box::new(d3d12::D3d12Compositor::new(
                    window,
                    stream.adapter_luid,
                )?));
            }
            let Self::D3d12(value) = self else {
                unreachable!()
            };
            return value.import_stream(stream);
        }

        let needs_keyed_mutex = stream.image_kind == ExternalImageKind::D3d11SharedResourceNt;
        let adapter = stream.adapter_luid;
        let matches = matches!(self, Self::Vulkan(value)
            if (!needs_keyed_mutex || value.keyed_mutex_enabled)
                && ((adapter.low_part == 0 && adapter.high_part == 0) || value.matches_adapter(adapter)));
        if !matches {
            drop(std::mem::replace(self, Self::Empty));
            let preferred = (adapter.low_part != 0 || adapter.high_part != 0).then_some(adapter);
            *self = Self::Vulkan(Box::new(DirectCompositor::new(
                window,
                preferred,
                needs_keyed_mutex,
            )?));
        }
        let Self::Vulkan(value) = self else {
            unreachable!()
        };
        value.import_stream(stream)
    }

    fn recreate_swapchain(&mut self, window: &Window) -> Result<bool, Box<dyn Error>> {
        match self {
            Self::Vulkan(value) => value.recreate_swapchain(window),
            Self::D3d12(value) => value.resize(window),
            Self::Empty => Ok(false),
        }
    }

    fn set_latest(&mut self, frame: FrameReady) {
        match self {
            Self::Vulkan(value) => value.set_latest(frame),
            Self::D3d12(value) => value.set_latest(frame),
            Self::Empty => {}
        }
    }

    fn has_pending_frame(&self) -> bool {
        match self {
            Self::Vulkan(value) => value.has_pending_frame(),
            Self::D3d12(value) => value.has_pending_frame(),
            Self::Empty => false,
        }
    }

    fn disconnect_stream(&mut self) -> Result<(), Box<dyn Error>> {
        match self {
            Self::Vulkan(value) => value.disconnect_stream().map_err(Into::into),
            Self::D3d12(value) => value.disconnect_stream(),
            Self::Empty => Ok(()),
        }
    }

    fn draw(
        &mut self,
        output: &egui::FullOutput,
        primitives: &[egui::ClippedPrimitive],
    ) -> Result<bool, Box<dyn Error>> {
        match self {
            Self::Vulkan(value) => value.draw(output, primitives),
            Self::D3d12(value) => value.draw(output, primitives),
            Self::Empty => Ok(false),
        }
    }
}

impl DirectCompositor {
    fn new(
        window: &Window,
        preferred_adapter: Option<AdapterLuid>,
        enable_keyed_mutex: bool,
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
            let (physical_device, queue_family) = instance
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
            let extension_names = required
                .iter()
                .map(|name| name.as_ptr())
                .collect::<Vec<_>>();
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
                imported: None,
                latest: None,
                last_released_frame: 0,
                keyed_mutex_enabled: enable_keyed_mutex,
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
        self.last_released_frame = 0;
        Ok(())
    }

    fn matches_adapter(&self, adapter: AdapterLuid) -> bool {
        physical_device_matches_luid(&self.instance, self.physical_device, adapter)
    }

    fn set_latest(&mut self, frame: FrameReady) {
        self.latest = Some(frame);
    }

    fn has_pending_frame(&self) -> bool {
        self.latest.is_some()
    }

    fn disconnect_stream(&mut self) -> Result<(), vk::Result> {
        unsafe { self.device.device_wait_idle()? };
        self.latest = None;
        self.last_released_frame = 0;
        self.imported = None;
        Ok(())
    }

    fn recreate_swapchain(&mut self, window: &Window) -> Result<bool, Box<dyn Error>> {
        let requested = window.inner_size();
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

            for framebuffer in self.framebuffers.drain(..) {
                self.device.destroy_framebuffer(framebuffer, None);
            }
            for view in self.swapchain_views.drain(..) {
                self.device.destroy_image_view(view, None);
            }
            self.swapchain_loader
                .destroy_swapchain(self.swapchain, None);
            self.swapchain = replacement;
            self.swapchain_images = images;
            self.swapchain_views = views;
            self.framebuffers = framebuffers;
            self.swapchain_initialized = vec![false; self.swapchain_images.len()];
            self.extent = extent;
            Ok(true)
        }
    }

    fn draw(
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
            if let (Some(imported), Some(frame)) = (&self.imported, self.latest)
                && imported.uses_keyed_mutex()
            {
                // A D3D11 shared texture is made visible by both its fence and
                // its keyed mutex. Waiting for the fence before submission
                // avoids asking the Vulkan driver to resolve both primitives
                // inside the same queue operation.
                imported.wait_for(frame.sync_value, u64::MAX)?;
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
            if self.latest.is_some() || !self.swapchain_initialized[image_index as usize] {
                self.device.cmd_clear_color_image(
                    command,
                    destination,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &vk::ClearColorValue {
                        float32: [1.0, 0.0, 1.0, 1.0],
                    },
                    &[color_range()],
                );
            }
            if let (Some(imported), Some(frame)) = (&self.imported, self.latest) {
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
                    let (offset_x, offset_y, width, height) = fit_extent(
                        content_width,
                        content_height,
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
                                    x: content_width as i32,
                                    y: content_height as i32,
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
            if let (Some(imported), Some(frame)) = (&self.imported, self.latest)
                && !imported.uses_keyed_mutex()
            {
                waits.push(imported.timeline());
                wait_values.push(frame.sync_value);
                wait_stages.push(vk::PipelineStageFlags::TRANSFER);
            }
            let commands = [command];
            let mut signals = vec![self.render_finished[flight]];
            let mut signal_values = vec![0];
            let release = self
                .imported
                .as_ref()
                .zip(self.latest)
                .and_then(|(imported, frame)| {
                    (frame.frame_number > self.last_released_frame)
                        .then(|| {
                            imported
                                .consumer_timeline()
                                .map(|semaphore| (semaphore, frame))
                        })
                        .flatten()
                });
            if let Some((semaphore, frame)) = release {
                signals.push(semaphore);
                signal_values.push(frame.sync_value);
            }
            let mut timeline_submit = vk::TimelineSemaphoreSubmitInfo::default()
                .wait_semaphore_values(&wait_values)
                .signal_semaphore_values(&signal_values);
            let keyed_memory =
                self.imported
                    .as_ref()
                    .zip(self.latest)
                    .and_then(|(imported, frame)| {
                        imported
                            .uses_keyed_mutex()
                            .then(|| imported.memory(frame.slot))
                            .flatten()
                            .map(|memory| memory)
                    });
            let acquire_syncs = keyed_memory.map_or([vk::DeviceMemory::null()], |memory| [memory]);
            let acquire_keys = keyed_memory.map_or([0], |_| [1]);
            let acquire_timeouts = [u32::MAX];
            let release_syncs = acquire_syncs;
            let release_keys = [0];
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
            if keyed_memory.is_some() {
                submit = submit.push_next(&mut keyed_mutex);
            }
            self.device
                .queue_submit(self.queue, &[submit], self.fences[flight])?;
            if let Some((_, frame)) = release {
                self.last_released_frame = frame.frame_number;
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
            if keyed_memory.is_some() {
                self.latest = None;
            }
            self.frame_index += 1;
            Ok(!(acquire_suboptimal || present_suboptimal))
        }
    }
}

impl Drop for DirectCompositor {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.wait_for_fences(&self.fences, true, u64::MAX);
            self.imported = None;
            let _ = self.device.device_wait_idle();
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
    let options = parse_target()?;
    let target = options.target;
    let track_render_target = options.track_render_target;
    let viewer_api = options.api;
    let executable_dir = env::current_exe()?
        .parent()
        .ok_or("viewer executable has no parent directory")?
        .to_path_buf();
    // Include a monotonic nonce so a stale AF_UNIX path left by a forcibly
    // terminated viewer can never collide with a subsequently reused PID.
    let session = format!(
        "snowglass-{}-{:x}",
        std::process::id(),
        performance_counter().unwrap_or_else(|_| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64
        })
    );
    let listener = LocalSocketListener::bind(&session)?;
    let input_listener = LocalSocketListener::bind(&format!("{session}-input"))?;
    let (input_tx, input_rx) = mpsc::channel();
    spawn_input_ipc(input_listener, input_rx);
    let target_process = launch_target(&target, &executable_dir, &session, track_render_target)?;
    let process_id = ProcessId(target_process.child.id());
    let stop = Arc::new(AtomicBool::new(false));
    let backing_window = Arc::new(AtomicIsize::new(0));
    let (target_done_tx, target_done_rx) = mpsc::channel();
    spawn_target_monitor(target_process, process_id, stop.clone(), target_done_tx);
    let (capture_tx, capture_rx) = mpsc::channel();
    spawn_ipc(listener, capture_tx, viewer_api);

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let window = event_loop.create_window(
        Window::default_attributes()
            .with_title("Snowglass")
            .with_inner_size(LogicalSize::new(1280, 720))
            .with_resizable(true),
    )?;
    let viewer_window = native_window(&window)?;
    let _raw_keyboard =
        RawKeyboardForwarder::install(viewer_window, input_tx.clone(), backing_window.clone())?;
    let mut egui_context = egui::Context::default();
    let mut egui_state = egui_winit::State::new(
        egui_context.clone(),
        egui::ViewportId::ROOT,
        &window,
        Some(window.scale_factor() as f32),
        window.theme(),
        None,
    );
    let mut compositor = ViewerCompositor::new(&window, viewer_api)?;
    let frequency = performance_frequency().unwrap_or(1) as f64;
    let mut stats = Statistics {
        sample_started: Some(Instant::now()),
        ..Default::default()
    };
    let mut cursor_position = (0, 0);
    let mut active_capture_connection = 0u64;
    let mut active_backend = "Waiting";
    let mut active_consumer = "Waiting";
    let input_started = Instant::now();
    let mut input_sequence = 0u32;
    let mut pressed_keys = HashSet::<KeyCode>::new();

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
                WindowEvent::Resized(size) => {
                    if size.width != 0
                        && size.height != 0
                        && let Err(error) = compositor.recreate_swapchain(&window)
                    {
                        eprintln!("snowglass: swapchain recreation failed: {error}");
                        target.exit();
                    }
                }
                WindowEvent::Focused(focused) => {
                    if !focused {
                        pressed_keys.clear();
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        match event.state {
                            ElementState::Pressed => {
                                pressed_keys.insert(code);
                            }
                            ElementState::Released => {
                                pressed_keys.remove(&code);
                            }
                        }
                    }
                    if let Some(backing) = load_window(&backing_window) {
                        if event.state == ElementState::Pressed
                            && let Some(text) = event.text
                        {
                            for character in text.chars() {
                                if !character.is_control() {
                                    forward_character(backing, character);
                                }
                            }
                        }
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    if let Some(backing) = load_window(&backing_window) {
                        let viewer_size = window.inner_size();
                        let backing_size =
                            client_size(backing).unwrap_or((viewer_size.width, viewer_size.height));
                        cursor_position = (
                            (position.x * backing_size.0 as f64 / viewer_size.width.max(1) as f64)
                                .round() as i32,
                            (position.y * backing_size.1 as f64 / viewer_size.height.max(1) as f64)
                                .round() as i32,
                        );
                        forward_mouse_move(backing, cursor_position.0, cursor_position.1);
                    }
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    if let Some(button) = mouse_button(button) {
                        if let Some(backing) = load_window(&backing_window) {
                            forward_mouse_button(
                                backing,
                                button,
                                state == ElementState::Pressed,
                                cursor_position.0,
                                cursor_position.1,
                            );
                        }
                        send_direct_input(
                            &input_tx,
                            &mut input_sequence,
                            input_started,
                            DirectInputDevice::Mouse,
                            12 + u32::from(button.saturating_sub(1)),
                            if state == ElementState::Pressed {
                                0x80
                            } else {
                                0
                            },
                        );
                    }
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    let vertical = match delta {
                        MouseScrollDelta::LineDelta(_, vertical) => (vertical * 120.0) as i32,
                        MouseScrollDelta::PixelDelta(delta) => delta.y as i32,
                    };
                    if vertical != 0 {
                        send_direct_input(
                            &input_tx,
                            &mut input_sequence,
                            input_started,
                            DirectInputDevice::Mouse,
                            8,
                            vertical as u32,
                        );
                    }
                    if let Some(backing) = load_window(&backing_window) {
                        match delta {
                            MouseScrollDelta::LineDelta(horizontal, vertical) => {
                                if vertical != 0.0 {
                                    forward_mouse_wheel(backing, (vertical * 120.0) as i16, false);
                                }
                                if horizontal != 0.0 {
                                    forward_mouse_wheel(backing, (horizontal * 120.0) as i16, true);
                                }
                            }
                            MouseScrollDelta::PixelDelta(delta) => {
                                if delta.y != 0.0 {
                                    forward_mouse_wheel(backing, delta.y as i16, false);
                                }
                                if delta.x != 0.0 {
                                    forward_mouse_wheel(backing, delta.x as i16, true);
                                }
                            }
                        }
                    }
                }
                WindowEvent::RedrawRequested => {
                    let size = window.inner_size();
                    if size.width == 0 || size.height == 0 {
                        return;
                    }
                    if target_done_rx.try_recv().is_ok() {
                        target.exit();
                        return;
                    }
                    while let Ok(capture) = capture_rx.try_recv() {
                        match capture {
                            CaptureEvent::Stream(connection, stream) => {
                                active_capture_connection = connection;
                                active_backend = match stream.image_kind {
                                    ExternalImageKind::D3d11SharedResourceNt => "D3D11",
                                    ExternalImageKind::VulkanOpaqueWin32 => "Vulkan",
                                    ExternalImageKind::D3d12Resource
                                    | ExternalImageKind::D3d12Heap => "D3D12",
                                    ExternalImageKind::OpenGlD3d12Resource => "OpenGL",
                                    ExternalImageKind::VulkanD3d12Resource => "Vulkan",
                                };
                                if stream.source_window != 0 {
                                    let source = NativeWindow(stream.source_window as isize);
                                    backing_window.store(source.0, Ordering::Release);
                                }
                                match compositor.import_stream(&window, &stream, viewer_api) {
                                    Ok(()) => {
                                        // A deferred or replaced graphics backend has no copy
                                        // of textures emitted to its predecessor. A fresh egui
                                        // context guarantees that Managed(0) is emitted as a full
                                        // font atlas before this backend's first draw.
                                        egui_context = egui::Context::default();
                                        egui_state = egui_winit::State::new(
                                            egui_context.clone(),
                                            egui::ViewportId::ROOT,
                                            &window,
                                            Some(window.scale_factor() as f32),
                                            window.theme(),
                                            None,
                                        );
                                        active_consumer = match viewer_api {
                                            ViewerApi::D3d12
                                                if matches!(
                                                    stream.image_kind,
                                                    ExternalImageKind::D3d12Resource
                                                        | ExternalImageKind::OpenGlD3d12Resource
                                                        | ExternalImageKind::VulkanD3d12Resource
                                                ) =>
                                            {
                                                "D3D12/DXGI"
                                            }
                                            _ => "Vulkan",
                                        };
                                        eprintln!(
                                            "snowglass: imported {active_backend} stream {}x{} with {} slots using {}",
                                            stream.width,
                                            stream.height,
                                            stream.slot_count,
                                            active_consumer,
                                        );
                                    }
                                    Err(error) => {
                                        eprintln!("snowglass: direct import failed: {error}")
                                    }
                                }
                            }
                            CaptureEvent::Frame(connection, frame) => {
                                if connection != active_capture_connection {
                                    continue;
                                }
                                if frame.frame_number == 1 {
                                    eprintln!(
                                        "snowglass: received first {active_backend} frame on connection {connection}"
                                    );
                                }
                                if let Some(previous) = stats.previous_timestamp_qpc {
                                    let frame_count = frame
                                        .frame_number
                                        .saturating_sub(stats.frame_number)
                                        .max(1);
                                    stats.frame_interval_ms =
                                        frame.timestamp_qpc.saturating_sub(previous) as f64
                                            * 1000.0
                                            / frequency
                                            / frame_count as f64;
                                }
                                stats.previous_timestamp_qpc = Some(frame.timestamp_qpc);
                                if stats.received != 0
                                    && frame.frame_number > stats.frame_number + 1
                                {
                                    stats.dropped += frame.frame_number - stats.frame_number - 1;
                                }
                                stats.received += 1;
                                stats.frames_since_sample += 1;
                                stats.frame_number = frame.frame_number;
                                stats.slot = frame.slot;
                                stats.render_width = frame.render_width;
                                stats.render_height = frame.render_height;
                                stats.capture_x = frame.capture_x;
                                stats.capture_y = frame.capture_y;
                                stats.capture_width = frame.capture_width;
                                stats.capture_height = frame.capture_height;
                                stats.latency_ms = performance_counter()
                                    .unwrap_or(frame.timestamp_qpc)
                                    .saturating_sub(frame.timestamp_qpc)
                                    as f64
                                    * 1000.0
                                    / frequency;
                                compositor.set_latest(frame);
                                if active_backend == "D3D11" {
                                    // Keyed-mutex frames must be consumed and released in order;
                                    // do not overwrite one with a newer socket message.
                                    break;
                                }
                            }
                            CaptureEvent::Disconnected(connection) => {
                                if connection != active_capture_connection {
                                    continue;
                                }
                                if let Err(error) = compositor.disconnect_stream() {
                                    eprintln!("snowglass: stream disconnect failed: {error}");
                                }
                            }
                        }
                    }
                    if active_backend == "D3D11" && !compositor.has_pending_frame() {
                        // Each viewer swapchain image contains a different historical capture.
                        // Without a new keyed-mutex frame, presenting another image would rotate
                        // an older capture back onto the screen and look like temporal ghosting.
                        return;
                    }
                    if let Some(started) = stats.sample_started {
                        if started.elapsed() >= Duration::from_millis(500) {
                            stats.fps =
                                stats.frames_since_sample as f64 / started.elapsed().as_secs_f64();
                            stats.frames_since_sample = 0;
                            stats.sample_started = Some(Instant::now());
                            stats.history.push_back(StatisticsSample {
                                fps: stats.fps as f32,
                                latency_ms: stats.latency_ms as f32,
                                frame_interval_ms: stats.frame_interval_ms as f32,
                            });
                            if stats.history.len() > 240 {
                                stats.history.pop_front();
                            }
                        }
                    }
                    let input = egui_state.take_egui_input(&window);
                    let output = egui_context.run(input, |context| {
                        egui::TopBottomPanel::top("statistics").show(context, |ui| {
                            ui.horizontal(|ui| {
                                ui.strong(format!(
                                    "Snowglass {active_backend} via {active_consumer}"
                                ));
                                ui.label(format!("Frame {}", stats.frame_number));
                                ui.label(format!("Slot {}", stats.slot));
                                ui.label(format!(
                                    "Render target {}x{}",
                                    stats.render_width, stats.render_height
                                ));
                                ui.label(format!(
                                    "Scissor ({}, {}) {}x{}",
                                    stats.capture_x,
                                    stats.capture_y,
                                    stats.capture_width,
                                    stats.capture_height
                                ));
                                ui.label(format!("{:.1} FPS", stats.fps));
                                ui.label(format!("Latency {:.2} ms", stats.latency_ms));
                                ui.label(format!(
                                    "Frame-to-frame {:.2} ms",
                                    stats.frame_interval_ms
                                ));
                                ui.label(format!("Dropped {}", stats.dropped));
                                let mut keys = pressed_keys
                                    .iter()
                                    .map(|key| format!("{key:?}"))
                                    .collect::<Vec<_>>();
                                keys.sort_unstable();
                                ui.label(if keys.is_empty() {
                                    "Keys: none".to_owned()
                                } else {
                                    format!("Keys: {}", keys.join(" + "))
                                });
                            });
                        });
                        let statistics_frame = egui::Frame::window(&context.style())
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(egui::Stroke::new(
                                1.0_f32,
                                egui::Color32::from_white_alpha(48),
                            ))
                            .shadow(egui::epaint::Shadow::NONE);
                        egui::Window::new("Frame Graph")
                            .default_pos(egui::pos2(12.0, 48.0))
                            .resizable(false)
                            .collapsible(true)
                            .frame(statistics_frame)
                            .show(context, |ui| {
                                draw_statistics_graph(ui, &stats.history);
                            });
                    });
                    egui_state.handle_platform_output(&window, output.platform_output.clone());
                    let primitives =
                        egui_context.tessellate(output.shapes.clone(), output.pixels_per_point);
                    match compositor.draw(&output, &primitives) {
                        Ok(true) => {}
                        Ok(false) => {
                            if let Err(error) = compositor.recreate_swapchain(&window) {
                                eprintln!("snowglass: swapchain recreation failed: {error}");
                                target.exit();
                            }
                        }
                        Err(error) => {
                            eprintln!("snowglass: direct composition failed: {error}");
                            target.exit();
                        }
                    }
                }
                _ => {}
            }
        }
        Event::DeviceEvent {
            event: DeviceEvent::MouseMotion { delta },
            ..
        } => {
            for (offset, value) in [(0, delta.0), (4, delta.1)] {
                if value != 0.0 {
                    send_direct_input(
                        &input_tx,
                        &mut input_sequence,
                        input_started,
                        DirectInputDevice::Mouse,
                        offset,
                        (value as i32) as u32,
                    );
                }
            }
        }
        _ => {}
    })?;
    Ok(())
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

fn spawn_ipc(
    listener: LocalSocketListener,
    events: mpsc::Sender<CaptureEvent>,
    viewer_api: ViewerApi,
) {
    thread::spawn(move || {
        let newest_connection = Arc::new(AtomicU64::new(0));
        loop {
            let mut socket = match listener.accept() {
                Ok(socket) => socket,
                Err(error) => {
                    eprintln!("snowglass: IPC accept ended: {error}");
                    return;
                }
            };
            let connection = newest_connection.fetch_add(1, Ordering::AcqRel) + 1;
            if let Err(error) = socket.send(Message::Hello(Handshake {
                protocol_version: PROTOCOL_VERSION,
                process_id: std::process::id(),
                preferred_consumer: match viewer_api {
                    ViewerApi::D3d12 => ConsumerApi::D3d12,
                    ViewerApi::Vulkan => ConsumerApi::Vulkan,
                },
            })) {
                eprintln!("snowglass: IPC handshake failed: {error}");
                continue;
            }
            let events = events.clone();
            let newest_connection = newest_connection.clone();
            thread::spawn(move || {
                let result = (|| -> std::io::Result<()> {
                    loop {
                        match socket.receive()? {
                            Message::StreamReady(stream) => {
                                events
                                    .send(CaptureEvent::Stream(connection, stream))
                                    .map_err(|_| {
                                        std::io::Error::new(
                                            std::io::ErrorKind::BrokenPipe,
                                            "viewer closed",
                                        )
                                    })?;
                            }
                            Message::FrameReady(frame) => {
                                if newest_connection.load(Ordering::Acquire) == connection {
                                    let _ = events.send(CaptureEvent::Frame(connection, frame));
                                }
                            }
                            Message::Shutdown => return Ok(()),
                            Message::DirectInputData(_) => {}
                            Message::Hello(_) => {}
                        }
                    }
                })();
                if let Err(error) = result
                    && error.kind() != std::io::ErrorKind::UnexpectedEof
                    && error.kind() != std::io::ErrorKind::ConnectionReset
                {
                    eprintln!("snowglass: IPC connection ended: {error}");
                }
                let _ = events.send(CaptureEvent::Disconnected(connection));
            });
        }
    });
}

fn spawn_input_ipc(listener: LocalSocketListener, input: mpsc::Receiver<DirectInputData>) {
    thread::spawn(move || {
        let mut pending = None;
        loop {
            let mut socket = match listener.accept() {
                Ok(socket) => socket,
                Err(error) => {
                    eprintln!("snowglass: DirectInput IPC accept ended: {error}");
                    return;
                }
            };
            loop {
                let event = match pending.take() {
                    Some(event) => event,
                    None => match input.recv() {
                        Ok(event) => event,
                        Err(_) => return,
                    },
                };
                if let Err(error) = socket.send(Message::DirectInputData(event)) {
                    pending = Some(event);
                    if error.kind() != std::io::ErrorKind::BrokenPipe
                        && error.kind() != std::io::ErrorKind::ConnectionReset
                    {
                        eprintln!("snowglass: DirectInput IPC send failed: {error}");
                    }
                    break;
                }
            }
        }
    });
}

fn spawn_target_monitor(
    target_process: TargetProcess,
    process_id: ProcessId,
    stop: Arc<AtomicBool>,
    done: mpsc::Sender<()>,
) {
    thread::spawn(move || {
        let TargetProcess { mut child, job } = target_process;
        loop {
            if stop.load(Ordering::Acquire) {
                break;
            }
            let _ = hide_top_level_windows(process_id);
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => thread::sleep(Duration::from_millis(8)),
            }
        }
        // Closing the job terminates the root and every descendant before the
        // viewer is notified that the target has finished.
        drop(job);
        let _ = child.wait();
        let _ = done.send(());
    });
}

struct ViewerOptions {
    target: Vec<String>,
    track_render_target: bool,
    api: ViewerApi,
}

fn parse_target() -> Result<ViewerOptions, Box<dyn Error>> {
    parse_arguments(env::args().skip(1))
}

fn parse_arguments(
    arguments: impl IntoIterator<Item = String>,
) -> Result<ViewerOptions, Box<dyn Error>> {
    let mut arguments = arguments.into_iter();
    let mut track_render_target = false;
    let mut api = ViewerApi::default();
    let mut found_separator = false;
    while let Some(argument) = arguments.next() {
        if argument == "--" {
            found_separator = true;
            break;
        }
        if argument == "--track-render-target" {
            track_render_target = true;
        } else if argument == "--api" {
            api = ViewerApi::parse(&arguments.next().ok_or("--api requires d3d12 or vulkan")?)?;
        } else if let Some(value) = argument.strip_prefix("--api=") {
            api = ViewerApi::parse(value)?;
        } else {
            return Err(format!(
                "unknown viewer option '{argument}'; usage: snowglass-viewer [--api d3d12|vulkan] [--track-render-target] -- <program> [arguments...]"
            )
            .into());
        }
    }
    if !found_separator {
        return Err(
            "usage: snowglass-viewer [--api d3d12|vulkan] [--track-render-target] -- <program> [arguments...]".into(),
        );
    }
    let target = arguments.collect::<Vec<_>>();
    if target.is_empty() {
        return Err("target program is required".into());
    }
    Ok(ViewerOptions {
        target,
        track_render_target,
        api,
    })
}

#[cfg(test)]
mod argument_tests {
    use super::*;

    fn arguments(values: &[&str]) -> Result<ViewerOptions, Box<dyn Error>> {
        parse_arguments(values.iter().map(|value| (*value).to_owned()))
    }

    #[test]
    fn defaults_to_d3d12() -> Result<(), Box<dyn Error>> {
        let options = arguments(&["--", "game.exe"])?;
        assert_eq!(options.api, ViewerApi::D3d12);
        assert!(!options.track_render_target);
        assert_eq!(options.target, ["game.exe"]);
        Ok(())
    }

    #[test]
    fn accepts_vulkan_override_and_options_in_either_order() -> Result<(), Box<dyn Error>> {
        let options = arguments(&[
            "--track-render-target",
            "--api=vulkan",
            "--",
            "game.exe",
            "--api",
            "target-argument",
        ])?;
        assert_eq!(options.api, ViewerApi::Vulkan);
        assert!(options.track_render_target);
        assert_eq!(options.target, ["game.exe", "--api", "target-argument"]);
        Ok(())
    }
}

struct TargetProcess {
    child: Child,
    job: KillOnCloseJob,
}

fn launch_target(
    target: &[String],
    layer_dir: &Path,
    session: &str,
    track_render_target: bool,
) -> std::io::Result<TargetProcess> {
    use std::os::windows::{io::AsRawHandle, process::CommandExt};
    use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

    let d3d11_hook = layer_dir.join("snowglass_hook_d3d11.dll");
    let d3d12_hook = layer_dir.join("snowglass_hook_d3d12.dll");
    let opengl_hook = layer_dir.join("snowglass_hook_opengl32.dll");
    let dinput_hook = layer_dir.join("snowglass_hook_dinput.dll");
    for (name, hook) in [
        ("D3D11", &d3d11_hook),
        ("D3D12", &d3d12_hook),
        ("OpenGL", &opengl_hook),
        ("DirectInput", &dinput_hook),
    ] {
        if hook.is_file() {
            continue;
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{name} hook is missing at {}", hook.display()),
        ));
    }
    let mut command = Command::new(&target[0]);
    command
        .args(&target[1..])
        .env("VK_LAYER_PATH", layer_dir)
        .env("VK_INSTANCE_LAYERS", "VK_LAYER_SNOWGLASS_capture")
        .env("SNOWGLASS_SESSION", session)
        .env("SNOWGLASS_LOG_FILE", layer_dir.join("snowglass-layer.log"));
    command.env(
        "SNOWGLASS_DINPUT_LOG_FILE",
        layer_dir.join("snowglass-dinput.log"),
    );
    command.env(
        "SNOWGLASS_D3D11_LOG_FILE",
        layer_dir.join("snowglass-d3d11.log"),
    );
    command.env(
        "SNOWGLASS_D3D12_LOG_FILE",
        layer_dir.join("snowglass-d3d12.log"),
    );
    command.env(
        "SNOWGLASS_OPENGL_LOG_FILE",
        layer_dir.join("snowglass-opengl.log"),
    );
    if track_render_target {
        command.env("SNOWGLASS_TRACK_RENDER_TARGET", "1");
    }
    command.creation_flags(CREATE_SUSPENDED);
    let job = KillOnCloseJob::new()?;
    let mut child = command.spawn()?;
    let process_id = ProcessId(child.id());
    let process = child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    if let Err(error) = job.assign_process(process) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let libraries = [
        (
            d3d11_hook.as_path(),
            c"SnowglassInstallD3d11Hook" as &std::ffi::CStr,
        ),
        (
            d3d12_hook.as_path(),
            c"SnowglassInstallD3d12Hook" as &std::ffi::CStr,
        ),
        (
            opengl_hook.as_path(),
            c"SnowglassInstallOpenGlHook" as &std::ffi::CStr,
        ),
        (
            dinput_hook.as_path(),
            c"SnowglassInstallDinputHook" as &std::ffi::CStr,
        ),
    ];
    if let Err(error) = inject_libraries_before_resume(process, process_id, &libraries) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(TargetProcess { child, job })
}

fn load_window(window: &AtomicIsize) -> Option<NativeWindow> {
    let value = window.load(Ordering::Acquire);
    (value != 0).then_some(NativeWindow(value))
}

fn native_window(window: &Window) -> Result<NativeWindow, Box<dyn Error>> {
    match window.window_handle()?.as_raw() {
        RawWindowHandle::Win32(handle) => Ok(NativeWindow(handle.hwnd.get() as isize)),
        _ => Err("Snowglass viewer requires a Win32 window".into()),
    }
}

fn mouse_button(button: MouseButton) -> Option<u16> {
    match button {
        MouseButton::Left => Some(1),
        MouseButton::Right => Some(2),
        MouseButton::Middle => Some(3),
        MouseButton::Back => Some(4),
        MouseButton::Forward => Some(5),
        MouseButton::Other(button) => Some(button.saturating_add(4)),
    }
}

fn send_direct_input(
    sender: &mpsc::Sender<DirectInputData>,
    sequence: &mut u32,
    started: Instant,
    device: DirectInputDevice,
    offset: u32,
    data: u32,
) {
    *sequence = sequence.wrapping_add(1);
    let _ = sender.send(DirectInputData {
        device,
        offset,
        data,
        timestamp: started.elapsed().as_millis() as u32,
        sequence: *sequence,
    });
}

fn virtual_key(code: KeyCode) -> Option<u32> {
    Some(match code {
        KeyCode::Backspace => 0x08,
        KeyCode::Tab => 0x09,
        KeyCode::Enter | KeyCode::NumpadEnter => 0x0d,
        KeyCode::ShiftLeft | KeyCode::ShiftRight => 0x10,
        KeyCode::ControlLeft | KeyCode::ControlRight => 0x11,
        KeyCode::AltLeft | KeyCode::AltRight => 0x12,
        KeyCode::Pause => 0x13,
        KeyCode::CapsLock => 0x14,
        KeyCode::Escape => 0x1b,
        KeyCode::Space => 0x20,
        KeyCode::PageUp => 0x21,
        KeyCode::PageDown => 0x22,
        KeyCode::End => 0x23,
        KeyCode::Home => 0x24,
        KeyCode::ArrowLeft => 0x25,
        KeyCode::ArrowUp => 0x26,
        KeyCode::ArrowRight => 0x27,
        KeyCode::ArrowDown => 0x28,
        KeyCode::Insert => 0x2d,
        KeyCode::Delete => 0x2e,
        KeyCode::Digit0 => 0x30,
        KeyCode::Digit1 => 0x31,
        KeyCode::Digit2 => 0x32,
        KeyCode::Digit3 => 0x33,
        KeyCode::Digit4 => 0x34,
        KeyCode::Digit5 => 0x35,
        KeyCode::Digit6 => 0x36,
        KeyCode::Digit7 => 0x37,
        KeyCode::Digit8 => 0x38,
        KeyCode::Digit9 => 0x39,
        KeyCode::KeyA => 0x41,
        KeyCode::KeyB => 0x42,
        KeyCode::KeyC => 0x43,
        KeyCode::KeyD => 0x44,
        KeyCode::KeyE => 0x45,
        KeyCode::KeyF => 0x46,
        KeyCode::KeyG => 0x47,
        KeyCode::KeyH => 0x48,
        KeyCode::KeyI => 0x49,
        KeyCode::KeyJ => 0x4a,
        KeyCode::KeyK => 0x4b,
        KeyCode::KeyL => 0x4c,
        KeyCode::KeyM => 0x4d,
        KeyCode::KeyN => 0x4e,
        KeyCode::KeyO => 0x4f,
        KeyCode::KeyP => 0x50,
        KeyCode::KeyQ => 0x51,
        KeyCode::KeyR => 0x52,
        KeyCode::KeyS => 0x53,
        KeyCode::KeyT => 0x54,
        KeyCode::KeyU => 0x55,
        KeyCode::KeyV => 0x56,
        KeyCode::KeyW => 0x57,
        KeyCode::KeyX => 0x58,
        KeyCode::KeyY => 0x59,
        KeyCode::KeyZ => 0x5a,
        KeyCode::SuperLeft => 0x5b,
        KeyCode::SuperRight => 0x5c,
        KeyCode::Numpad0 => 0x60,
        KeyCode::Numpad1 => 0x61,
        KeyCode::Numpad2 => 0x62,
        KeyCode::Numpad3 => 0x63,
        KeyCode::Numpad4 => 0x64,
        KeyCode::Numpad5 => 0x65,
        KeyCode::Numpad6 => 0x66,
        KeyCode::Numpad7 => 0x67,
        KeyCode::Numpad8 => 0x68,
        KeyCode::Numpad9 => 0x69,
        KeyCode::NumpadMultiply => 0x6a,
        KeyCode::NumpadAdd => 0x6b,
        KeyCode::NumpadSubtract => 0x6d,
        KeyCode::NumpadDecimal => 0x6e,
        KeyCode::NumpadDivide => 0x6f,
        KeyCode::F1 => 0x70,
        KeyCode::F2 => 0x71,
        KeyCode::F3 => 0x72,
        KeyCode::F4 => 0x73,
        KeyCode::F5 => 0x74,
        KeyCode::F6 => 0x75,
        KeyCode::F7 => 0x76,
        KeyCode::F8 => 0x77,
        KeyCode::F9 => 0x78,
        KeyCode::F10 => 0x79,
        KeyCode::F11 => 0x7a,
        KeyCode::F12 => 0x7b,
        KeyCode::NumLock => 0x90,
        KeyCode::ScrollLock => 0x91,
        KeyCode::Semicolon => 0xba,
        KeyCode::Equal => 0xbb,
        KeyCode::Comma => 0xbc,
        KeyCode::Minus => 0xbd,
        KeyCode::Period => 0xbe,
        KeyCode::Slash => 0xbf,
        KeyCode::Backquote => 0xc0,
        KeyCode::BracketLeft => 0xdb,
        KeyCode::Backslash => 0xdc,
        KeyCode::BracketRight => 0xdd,
        KeyCode::Quote => 0xde,
        _ => return None,
    })
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

fn draw_statistics_graph(ui: &mut egui::Ui, history: &VecDeque<StatisticsSample>) {
    const GRAPH_WIDTH: f32 = 420.0;
    const GRAPH_HEIGHT: f32 = 90.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(GRAPH_WIDTH, GRAPH_HEIGHT), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, egui::Color32::from_black_alpha(48));
    for division in 1..4 {
        let y = egui::lerp(rect.top()..=rect.bottom(), division as f32 / 4.0);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(55)),
        );
    }
    if history.len() >= 2 {
        let fps_max = history
            .iter()
            .map(|sample| sample.fps)
            .fold(60.0_f32, f32::max)
            .max(1.0);
        let latency_max = history
            .iter()
            .map(|sample| sample.latency_ms)
            .fold(16.67_f32, f32::max)
            .max(1.0);
        let frame_interval_max = history
            .iter()
            .map(|sample| sample.frame_interval_ms)
            .fold(16.67_f32, f32::max)
            .max(1.0);
        let points = |value: fn(&StatisticsSample) -> f32, maximum: f32| {
            history
                .iter()
                .enumerate()
                .map(|(index, sample)| {
                    let x = egui::lerp(
                        rect.left()..=rect.right(),
                        index as f32 / (history.len() - 1) as f32,
                    );
                    let normalized = (value(sample) / maximum).clamp(0.0, 1.0);
                    egui::pos2(x, egui::lerp(rect.bottom()..=rect.top(), normalized))
                })
                .collect::<Vec<_>>()
        };
        painter.add(egui::Shape::line(
            points(|sample| sample.fps, fps_max),
            egui::Stroke::new(1.5_f32, egui::Color32::LIGHT_GREEN),
        ));
        painter.add(egui::Shape::line(
            points(|sample| sample.latency_ms, latency_max),
            egui::Stroke::new(1.5_f32, egui::Color32::LIGHT_RED),
        ));
        painter.add(egui::Shape::line(
            points(|sample| sample.frame_interval_ms, frame_interval_max),
            egui::Stroke::new(1.5_f32, egui::Color32::LIGHT_BLUE),
        ));
        painter.text(
            rect.left_top() + egui::vec2(5.0, 4.0),
            egui::Align2::LEFT_TOP,
            format!(
                "FPS green {fps_max:.0}   latency red {latency_max:.1} ms   frame Δ blue {frame_interval_max:.1} ms"
            ),
            egui::FontId::monospace(10.0),
            egui::Color32::WHITE,
        );
    } else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Collecting frame statistics…",
            egui::FontId::monospace(10.0),
            egui::Color32::GRAY,
        );
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

fn fit_inside(
    source_width: u32,
    source_height: u32,
    bounds_width: u32,
    bounds_height: u32,
) -> (u32, u32) {
    if source_width <= bounds_width && source_height <= bounds_height {
        return (source_width.max(1), source_height.max(1));
    }
    let scale = (bounds_width as f64 / source_width.max(1) as f64)
        .min(bounds_height as f64 / source_height.max(1) as f64);
    (
        (source_width as f64 * scale).floor().max(1.0) as u32,
        (source_height as f64 * scale).floor().max(1.0) as u32,
    )
}
