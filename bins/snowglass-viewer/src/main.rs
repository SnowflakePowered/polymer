use ash::{Entry, vk};
use egui_ash_renderer::{Options as EguiRendererOptions, Renderer as EguiRenderer};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawWindowHandle};
use snowglass_consumer_vulkan::ImportedImageRing;
use snowglass_input::{DirectInputData, DirectInputDevice};
use snowglass_ipc::{LocalSocketListener, Message, StreamReady};
use snowglass_platform_windows::{
    KillOnCloseJob, NativeWindow, PrivateDesktop, ProcessId, client_size, forward_character,
    forward_mouse_button, forward_mouse_move, forward_mouse_wheel, forward_raw_key,
    inject_libraries_before_resume, performance_counter, performance_frequency,
};
use snowglass_protocol::{
    AdapterLuid, ConsumerApi, ExternalImageKind, ExternalSyncKind, FrameReady, Handshake,
    PROTOCOL_VERSION, RELEASE_ALL_SYNC_VALUE,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    error::Error,
    ffi::{OsStr, OsString},
    path::Path,
    process::{Child, Command},
    sync::{
        Arc, Mutex,
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
    Stream(u64, StreamReady, mpsc::SyncSender<bool>),
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

struct ImportedVulkanSurface {
    connection: u64,
    imported: ImportedImageRing,
    latest: Option<FrameReady>,
    last_released_frame: u64,
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
    surfaces: Vec<ImportedVulkanSurface>,
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

/// A capture backend the viewer can install into the target process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HookBackend {
    Vulkan,
    OpenGl,
    D3d12,
    D3d11,
    Gdi,
}

/// The `--hook auto` selection: every backend except GDI, ranked by how
/// likely each is to be the real renderer rather than a probe or GUI shell.
const AUTO_HOOKS: [HookBackend; 4] = [
    HookBackend::Vulkan,
    HookBackend::OpenGl,
    HookBackend::D3d12,
    HookBackend::D3d11,
];

impl HookBackend {
    fn parse(value: &str) -> Result<Self, Box<dyn Error>> {
        match value.to_ascii_lowercase().as_str() {
            "vulkan" => Ok(Self::Vulkan),
            "opengl" => Ok(Self::OpenGl),
            "d3d12" => Ok(Self::D3d12),
            "d3d11" => Ok(Self::D3d11),
            "gdi" => Ok(Self::Gdi),
            _ => Err(format!(
                "unsupported hook '{value}'; expected auto, vulkan, d3d11, d3d12, opengl, or gdi"
            )
            .into()),
        }
    }

    fn class(self) -> u8 {
        match self {
            Self::Gdi => 0,
            Self::D3d11 => 1,
            Self::D3d12 => 2,
            Self::OpenGl => 3,
            Self::Vulkan => 4,
        }
    }
}

/// Appends comma-separated `--hook` tokens in submission order; `auto`
/// expands to [`AUTO_HOOKS`], skipping backends that are already listed.
fn parse_hook_list(hooks: &mut Vec<HookBackend>, value: &str) -> Result<(), Box<dyn Error>> {
    for token in value.split(',') {
        if token.eq_ignore_ascii_case("auto") {
            for &hook in &AUTO_HOOKS {
                if !hooks.contains(&hook) {
                    hooks.push(hook);
                }
            }
            continue;
        }
        let hook = HookBackend::parse(token)?;
        if hooks.contains(&hook) {
            return Err(format!("hook '{token}' is listed more than once").into());
        }
        hooks.push(hook);
    }
    Ok(())
}

/// Returns the preemption rank of a backend class under the configured hook
/// order: lower is higher priority, `None` means the backend is not enabled.
fn class_priority(hooks: &[HookBackend], class: u8) -> Option<usize> {
    hooks.iter().position(|hook| hook.class() == class)
}

/// Whether the native D3D12 consumer can import this stream: either a
/// fence-synchronized D3D12-shareable resource, or a keyed-mutex D3D11
/// resource acquired through D3D11On12 (which needs the adapter LUID).
fn d3d12_consumable(stream: &StreamReady) -> bool {
    matches!(
        stream.image_kind,
        ExternalImageKind::D3d12Resource
            | ExternalImageKind::OpenGlD3d12Resource
            | ExternalImageKind::VulkanD3d12Resource
            | ExternalImageKind::D3d11D3d12Resource
            | ExternalImageKind::GdiD3d12Resource
    ) || (stream.image_kind == ExternalImageKind::D3d11SharedResourceNt
        && stream.sync_kind == ExternalSyncKind::D3d11KeyedMutex
        && (stream.adapter_luid.low_part != 0 || stream.adapter_luid.high_part != 0))
}

fn stream_backend_class(kind: ExternalImageKind) -> u8 {
    match kind {
        ExternalImageKind::GdiD3d12Resource => HookBackend::Gdi.class(),
        ExternalImageKind::D3d11SharedResourceNt | ExternalImageKind::D3d11D3d12Resource => {
            HookBackend::D3d11.class()
        }
        ExternalImageKind::D3d12Resource | ExternalImageKind::D3d12Heap => {
            HookBackend::D3d12.class()
        }
        ExternalImageKind::OpenGlD3d12Resource => HookBackend::OpenGl.class(),
        ExternalImageKind::VulkanOpaqueWin32 | ExternalImageKind::VulkanD3d12Resource => {
            HookBackend::Vulkan.class()
        }
    }
}

/// A stream replaces an earlier stream for the same window when its backend
/// ranks at least as high in the configured hook order; a recreated surface
/// from the same backend always replaces its predecessor.
fn should_replace_same_window_backend(hooks: &[HookBackend], existing: u8, incoming: u8) -> bool {
    match (
        class_priority(hooks, incoming),
        class_priority(hooks, existing),
    ) {
        (Some(incoming), Some(existing)) => incoming <= existing,
        (Some(_), None) => true,
        (None, _) => false,
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
        connection: u64,
        stream: &StreamReady,
        api: ViewerApi,
    ) -> Result<(), Box<dyn Error>> {
        if api == ViewerApi::D3d12 && d3d12_consumable(stream) {
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
            return value.import_stream(connection, stream);
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
        value.import_stream(connection, stream)
    }

    fn recreate_swapchain(&mut self, window: &Window) -> Result<bool, Box<dyn Error>> {
        match self {
            Self::Vulkan(value) => value.recreate_swapchain(window),
            Self::D3d12(value) => value.resize(window),
            Self::Empty => Ok(false),
        }
    }

    fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        match self {
            Self::Vulkan(value) => value.set_latest(connection, frame),
            Self::D3d12(value) => value.set_latest(connection, frame),
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

    fn disconnect_stream(&mut self, connection: u64) -> Result<(), Box<dyn Error>> {
        match self {
            Self::Vulkan(value) => value.disconnect_stream(connection).map_err(Into::into),
            Self::D3d12(value) => value.disconnect_stream(connection),
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
                surfaces: Vec::new(),
                keyed_mutex_enabled: enable_keyed_mutex,
                egui_renderer: Some(egui_renderer),
            })
        }
    }

    fn import_stream(
        &mut self,
        connection: u64,
        stream: &StreamReady,
    ) -> Result<(), Box<dyn Error>> {
        self.disconnect_stream(connection)?;
        let imported = ImportedImageRing::import_into(
            &self.instance,
            self.device.clone(),
            self.physical_device,
            stream,
        )?;
        self.surfaces.push(ImportedVulkanSurface {
            connection,
            imported,
            latest: None,
            last_released_frame: 0,
        });
        Ok(())
    }

    fn matches_adapter(&self, adapter: AdapterLuid) -> bool {
        physical_device_matches_luid(&self.instance, self.physical_device, adapter)
    }

    fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        if let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.connection == connection)
        {
            surface.latest = Some(frame);
        }
    }

    fn has_pending_frame(&self) -> bool {
        self.surfaces.iter().any(|surface| surface.latest.is_some())
    }

    fn disconnect_stream(&mut self, connection: u64) -> Result<(), vk::Result> {
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
        self.surfaces.remove(index);
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
        self.surfaces.clear();
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
                        float32: [1.0, 0.0, 1.0, 1.0],
                    },
                    &[color_range()],
                );
            }
            let visible_count = self
                .surfaces
                .iter()
                .filter(|surface| surface.latest.is_some())
                .count() as u32;
            let cell_height = self.extent.height / visible_count.max(1);
            let mut visible_index = 0;
            for surface in &self.surfaces {
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
                    let (offset_x, offset_y, width, height) = fit_extent(
                        content_width,
                        content_height,
                        self.extent.width,
                        cell_height,
                    );
                    let offset_y = offset_y + (visible_index * cell_height) as i32;
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
            let commands = [command];
            let mut signals = vec![self.render_finished[flight]];
            let mut signal_values = vec![0];
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
}

impl Drop for DirectCompositor {
    fn drop(&mut self) {
        let _ = self.release_imported_streams();
        unsafe {
            let _ = self.device.wait_for_fences(&self.fences, true, u64::MAX);
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
    let track_render_targets = options.track_render_targets;
    let surface_limit = options.surfaces;
    let mut remaining_surface_skips = options.skip_surfaces;
    let isolate = options.isolate;
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
    let hooks = options.hooks;
    let target_process = launch_target(
        &target,
        &executable_dir,
        &session,
        track_render_targets,
        options.force_keyed_mutex,
        &hooks,
        isolate,
    )?;
    let stop = Arc::new(AtomicBool::new(false));
    let backing_window = Arc::new(AtomicIsize::new(0));
    let (target_done_tx, target_done_rx) = mpsc::channel();
    spawn_target_monitor(target_process, stop.clone(), target_done_tx);
    let (capture_tx, capture_rx) = mpsc::channel();
    // Connections the event loop has evicted (preempted or replaced). The IPC
    // threads close these sockets so the producer stops capturing.
    let revoked_connections = Arc::new(Mutex::new(HashSet::<u64>::new()));
    spawn_ipc(
        listener,
        capture_tx,
        viewer_api,
        revoked_connections.clone(),
    );

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
    let mut active_capture_connections = HashMap::<u64, (i64, u8)>::new();
    let mut skipped_surface_windows = HashSet::<(i64, u8)>::new();
    let mut skipped_surface_backends = HashSet::<u8>::new();
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
                            CaptureEvent::Stream(connection, stream, accepted) => {
                                let surface_identity = (
                                    stream.source_window,
                                    stream_backend_class(stream.image_kind),
                                );
                                let Some(stream_rank) =
                                    class_priority(&hooks, surface_identity.1)
                                else {
                                    eprintln!(
                                        "snowglass: ignored {:?} surface window {:#x}; its backend is not hooked",
                                        stream.image_kind, stream.source_window
                                    );
                                    let _ = accepted.send(false);
                                    continue;
                                };
                                let previously_skipped = stream.source_window != 0
                                    && skipped_surface_windows.contains(&surface_identity);
                                if previously_skipped
                                    || (!active_capture_connections.contains_key(&connection)
                                        && remaining_surface_skips != 0)
                                {
                                    if !previously_skipped {
                                        remaining_surface_skips -= 1;
                                        if stream.source_window != 0 {
                                            skipped_surface_windows.insert(surface_identity);
                                        }
                                        skipped_surface_backends.insert(surface_identity.1);
                                        eprintln!(
                                            "snowglass: skipped {:?} surface window {:#x}; {} initial surface(s) still to skip",
                                            stream.image_kind,
                                            stream.source_window,
                                            remaining_surface_skips
                                        );
                                    }
                                    let _ = accepted.send(false);
                                    continue;
                                }
                                if !active_capture_connections.contains_key(&connection) {
                                    let replaced = (stream.source_window != 0)
                                        .then(|| {
                                            active_capture_connections.iter().find_map(
                                                |(&existing, &(window, backend))| {
                                                    (window == stream.source_window)
                                                        .then_some((existing, backend))
                                                },
                                            )
                                        })
                                        .flatten();
                                    if let Some((existing, backend)) = replaced {
                                        if !should_replace_same_window_backend(
                                            &hooks,
                                            backend,
                                            surface_identity.1,
                                        ) {
                                            eprintln!(
                                                "snowglass: ignored lower-priority {:?} duplicate for window {:#x}",
                                                stream.image_kind, stream.source_window
                                            );
                                            let _ = accepted.send(false);
                                            continue;
                                        }
                                        active_capture_connections.remove(&existing);
                                        revoked_connections
                                            .lock()
                                            .unwrap_or_else(|error| error.into_inner())
                                            .insert(existing);
                                        if let Err(error) = compositor.disconnect_stream(existing) {
                                            eprintln!(
                                                "snowglass: failed to replace prior stream for window: {error}"
                                            );
                                        }
                                    }
                                }
                                if !active_capture_connections.contains_key(&connection)
                                    && active_capture_connections.len() >= surface_limit as usize
                                    && !skipped_surface_backends.contains(&surface_identity.1)
                                {
                                    let provisional = active_capture_connections.iter().find_map(
                                        |(&existing, &(_, backend))| {
                                            skipped_surface_backends
                                                .contains(&backend)
                                                .then_some(existing)
                                        },
                                    );
                                    if let Some(existing) = provisional {
                                        active_capture_connections.remove(&existing);
                                        revoked_connections
                                            .lock()
                                            .unwrap_or_else(|error| error.into_inner())
                                            .insert(existing);
                                        if let Err(error) = compositor.disconnect_stream(existing) {
                                            eprintln!(
                                                "snowglass: failed to replace provisional skipped-backend stream: {error}"
                                            );
                                        }
                                        eprintln!(
                                            "snowglass: replaced provisional skipped-backend surface with {:?} window {:#x}",
                                            stream.image_kind, stream.source_window
                                        );
                                    }
                                }
                                if !active_capture_connections.contains_key(&connection)
                                    && active_capture_connections.len() >= surface_limit as usize
                                    && !skipped_surface_backends.contains(&surface_identity.1)
                                {
                                    // Probe swapchains and GUI shells (GDI blits, a
                                    // frontend's D3D11 surface) must not hold the slot
                                    // once the real renderer attaches: a stream from a
                                    // higher-priority backend preempts the lowest-ranked,
                                    // oldest surface.
                                    let victim = active_capture_connections
                                        .iter()
                                        .filter_map(|(&existing, &(_, backend))| {
                                            class_priority(&hooks, backend)
                                                .filter(|&rank| rank > stream_rank)
                                                .map(|rank| (existing, rank))
                                        })
                                        .max_by_key(|&(existing, rank)| {
                                            (rank, std::cmp::Reverse(existing))
                                        })
                                        .map(|(existing, _)| existing);
                                    if let Some(existing) = victim {
                                        active_capture_connections.remove(&existing);
                                        revoked_connections
                                            .lock()
                                            .unwrap_or_else(|error| error.into_inner())
                                            .insert(existing);
                                        if let Err(error) = compositor.disconnect_stream(existing) {
                                            eprintln!(
                                                "snowglass: failed to preempt lower-priority stream: {error}"
                                            );
                                        }
                                        eprintln!(
                                            "snowglass: preempted lower-priority surface for {:?} window {:#x}",
                                            stream.image_kind, stream.source_window
                                        );
                                    }
                                }
                                if !active_capture_connections.contains_key(&connection)
                                    && active_capture_connections.len() >= surface_limit as usize
                                {
                                    eprintln!(
                                        "snowglass: ignored {:?} surface window {:#x}; surface limit {} is full",
                                        stream.image_kind, stream.source_window, surface_limit
                                    );
                                    let _ = accepted.send(false);
                                    continue;
                                }
                                active_backend = match stream.image_kind {
                                    ExternalImageKind::D3d11SharedResourceNt
                                    | ExternalImageKind::D3d11D3d12Resource => "D3D11",
                                    ExternalImageKind::VulkanOpaqueWin32 => "Vulkan",
                                    ExternalImageKind::D3d12Resource
                                    | ExternalImageKind::D3d12Heap => "D3D12",
                                    ExternalImageKind::OpenGlD3d12Resource => "OpenGL",
                                    ExternalImageKind::VulkanD3d12Resource => "Vulkan",
                                    ExternalImageKind::GdiD3d12Resource => "GDI",
                                };
                                if stream.source_window != 0 {
                                    let source = NativeWindow(stream.source_window as isize);
                                    backing_window.store(source.0, Ordering::Release);
                                }
                                match compositor.import_stream(
                                    &window,
                                    connection,
                                    &stream,
                                    viewer_api,
                                ) {
                                    Ok(()) => {
                                        active_capture_connections
                                            .insert(connection, surface_identity);
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
                                        active_consumer = if viewer_api == ViewerApi::D3d12
                                            && d3d12_consumable(&stream)
                                        {
                                            "D3D12/DXGI"
                                        } else {
                                            "Vulkan"
                                        };
                                        eprintln!(
                                            "snowglass: imported {active_backend} stream {}x{} for window {:#x} with {} slots using {}",
                                            stream.width,
                                            stream.height,
                                            stream.source_window,
                                            stream.slot_count,
                                            active_consumer,
                                        );
                                        let _ = accepted.send(true);
                                    }
                                    Err(error) => {
                                        eprintln!("snowglass: direct import failed: {error}");
                                        let _ = accepted.send(false);
                                    }
                                }
                            }
                            CaptureEvent::Frame(connection, frame) => {
                                if !active_capture_connections.contains_key(&connection) {
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
                                compositor.set_latest(connection, frame);
                                if active_backend == "D3D11" {
                                    // Keyed-mutex frames must be consumed and released in order;
                                    // do not overwrite one with a newer socket message.
                                    break;
                                }
                            }
                            CaptureEvent::Disconnected(connection) => {
                                revoked_connections
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner())
                                    .remove(&connection);
                                if active_capture_connections.remove(&connection).is_none() {
                                    continue;
                                }
                                if let Err(error) = compositor.disconnect_stream(connection) {
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
    revoked: Arc<Mutex<HashSet<u64>>>,
) {
    thread::spawn(move || {
        let next_connection = Arc::new(AtomicU64::new(0));
        loop {
            let mut socket = match listener.accept() {
                Ok(socket) => socket,
                Err(error) => {
                    eprintln!("snowglass: IPC accept ended: {error}");
                    return;
                }
            };
            let connection = next_connection.fetch_add(1, Ordering::AcqRel) + 1;
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
            let revoked = revoked.clone();
            thread::spawn(move || {
                let result = (|| -> std::io::Result<()> {
                    let mut accepted = false;
                    loop {
                        let message = socket.receive()?;
                        if revoked
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .contains(&connection)
                        {
                            // The event loop evicted this stream; drop the
                            // socket so the producer parks its capture.
                            return Ok(());
                        }
                        match message {
                            Message::StreamReady(stream) => {
                                let (accepted_tx, accepted_rx) = mpsc::sync_channel(0);
                                events
                                    .send(CaptureEvent::Stream(connection, stream, accepted_tx))
                                    .map_err(|_| {
                                        std::io::Error::new(
                                            std::io::ErrorKind::BrokenPipe,
                                            "viewer closed",
                                        )
                                    })?;
                                accepted = accepted_rx.recv().unwrap_or(false);
                                if accepted {
                                    socket.send(Message::StreamAccepted)?;
                                } else {
                                    return Ok(());
                                }
                            }
                            Message::FrameReady(frame) => {
                                if accepted {
                                    let _ = events.send(CaptureEvent::Frame(connection, frame));
                                }
                            }
                            Message::Shutdown => return Ok(()),
                            Message::DirectInputData(_) => {}
                            Message::Hello(_) => {}
                            Message::StreamAccepted => {}
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
    stop: Arc<AtomicBool>,
    done: mpsc::Sender<()>,
) {
    thread::spawn(move || {
        let TargetProcess {
            mut child,
            job,
            desktop,
        } = target_process;
        loop {
            if stop.load(Ordering::Acquire) {
                break;
            }
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => thread::sleep(Duration::from_millis(8)),
            }
        }
        // Closing the job terminates the root and every descendant before the
        // viewer is notified that the target has finished.
        drop(job);
        let _ = child.wait();
        drop(desktop);
        let _ = done.send(());
    });
}

struct ViewerOptions {
    target: Vec<String>,
    track_render_targets: Option<u32>,
    surfaces: u32,
    skip_surfaces: u32,
    isolate: bool,
    api: ViewerApi,
    force_keyed_mutex: bool,
    hooks: Vec<HookBackend>,
}

fn parse_target() -> Result<ViewerOptions, Box<dyn Error>> {
    parse_arguments(env::args().skip(1))
}

fn parse_arguments(
    arguments: impl IntoIterator<Item = String>,
) -> Result<ViewerOptions, Box<dyn Error>> {
    let mut arguments = arguments.into_iter().peekable();
    let mut track_render_targets = None;
    let mut surfaces = 1;
    let mut skip_surfaces = 0;
    let mut isolate = true;
    let mut api = ViewerApi::default();
    let mut force_keyed_mutex = false;
    let mut hooks = Vec::new();
    let mut found_separator = false;
    while let Some(argument) = arguments.next() {
        if argument == "--" {
            found_separator = true;
            break;
        }
        if argument == "--track-render-target" {
            track_render_targets = Some(1);
        } else if argument == "--track-render-targets" {
            track_render_targets = Some(match arguments.peek() {
                Some(value) if value != "--" => parse_render_target_count(
                    &arguments.next().expect("peeked render-target count"),
                )?,
                _ => 1,
            });
        } else if let Some(value) = argument.strip_prefix("--track-render-targets=") {
            track_render_targets = Some(parse_render_target_count(value)?);
        } else if argument == "--surfaces" {
            surfaces = match arguments.peek() {
                Some(value) if value != "--" => parse_bounded_count(
                    &arguments.next().expect("peeked surface count"),
                    "surface",
                )?,
                _ => 1,
            };
        } else if let Some(value) = argument.strip_prefix("--surfaces=") {
            surfaces = parse_bounded_count(value, "surface")?;
        } else if matches!(
            argument.as_str(),
            "--skip-surface" | "--skip-surfaces" | "--skip-window" | "--skip-windows"
        ) {
            skip_surfaces = match arguments.peek() {
                Some(value) if value != "--" => {
                    parse_skip_count(&arguments.next().expect("peeked surface skip count"))?
                }
                _ => 1,
            };
        } else if let Some(value) = argument
            .strip_prefix("--skip-surfaces=")
            .or_else(|| argument.strip_prefix("--skip-surface="))
            .or_else(|| argument.strip_prefix("--skip-windows="))
            .or_else(|| argument.strip_prefix("--skip-window="))
        {
            skip_surfaces = parse_skip_count(value)?;
        } else if argument == "--isolate" {
            isolate = true;
        } else if argument == "--no-isolate" {
            isolate = false;
        } else if argument == "--force-km" {
            force_keyed_mutex = true;
        } else if argument == "--hook" {
            parse_hook_list(
                &mut hooks,
                &arguments
                    .next()
                    .ok_or("--hook requires auto or a comma-separated list of hooks")?,
            )?;
        } else if let Some(value) = argument.strip_prefix("--hook=") {
            parse_hook_list(&mut hooks, value)?;
        } else if argument == "--api" {
            api = ViewerApi::parse(&arguments.next().ok_or("--api requires d3d12 or vulkan")?)?;
        } else if let Some(value) = argument.strip_prefix("--api=") {
            api = ViewerApi::parse(value)?;
        } else {
            return Err(format!(
                "unknown viewer option '{argument}'; usage: snowglass-viewer [--api d3d12|vulkan] [--track-render-targets [COUNT]] [--skip-surfaces [COUNT]] [--surfaces [COUNT]] [--force-km] [--hook auto|vulkan,d3d11,d3d12,opengl,gdi] [--no-isolate] -- <program> [arguments...]"
            )
            .into());
        }
    }
    if !found_separator {
        return Err(
            "usage: snowglass-viewer [--api d3d12|vulkan] [--track-render-targets [COUNT]] [--skip-surfaces [COUNT]] [--surfaces [COUNT]] [--force-km] [--hook auto|vulkan,d3d11,d3d12,opengl,gdi] [--no-isolate] -- <program> [arguments...]".into(),
        );
    }
    let target = arguments.collect::<Vec<_>>();
    if target.is_empty() {
        return Err("target program is required".into());
    }
    if hooks.is_empty() {
        hooks.extend(AUTO_HOOKS);
    }
    Ok(ViewerOptions {
        target,
        track_render_targets,
        surfaces,
        skip_surfaces,
        isolate,
        api,
        force_keyed_mutex,
        hooks,
    })
}

fn parse_render_target_count(value: &str) -> Result<u32, Box<dyn Error>> {
    parse_bounded_count(value, "render-target")
}

fn parse_bounded_count(value: &str, name: &str) -> Result<u32, Box<dyn Error>> {
    let count = value
        .parse::<u32>()
        .map_err(|_| format!("invalid {name} count '{value}'; expected 1..=16"))?;
    if !(1..=16).contains(&count) {
        return Err(format!("invalid {name} count '{value}'; expected 1..=16").into());
    }
    Ok(count)
}

fn parse_skip_count(value: &str) -> Result<u32, Box<dyn Error>> {
    value.parse::<u32>().map_err(|_| {
        format!("invalid surface skip count '{value}'; expected a non-negative integer").into()
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
        assert_eq!(options.track_render_targets, None);
        assert_eq!(options.surfaces, 1);
        assert_eq!(options.skip_surfaces, 0);
        assert!(options.isolate);
        assert!(!options.force_keyed_mutex);
        assert_eq!(options.hooks, AUTO_HOOKS);
        assert_eq!(options.target, ["game.exe"]);
        Ok(())
    }

    #[test]
    fn hook_selection_defaults_to_auto_without_gdi() -> Result<(), Box<dyn Error>> {
        assert_eq!(arguments(&["--hook", "auto", "--", "game.exe"])?.hooks, AUTO_HOOKS);
        assert!(!AUTO_HOOKS.contains(&HookBackend::Gdi));
        Ok(())
    }

    #[test]
    fn hook_order_defines_priority() -> Result<(), Box<dyn Error>> {
        let options = arguments(&["--hook", "gdi,d3d11", "--hook", "vulkan", "--", "game.exe"])?;
        assert_eq!(
            options.hooks,
            [HookBackend::Gdi, HookBackend::D3d11, HookBackend::Vulkan]
        );
        let gdi = HookBackend::Gdi.class();
        let vulkan = HookBackend::Vulkan.class();
        assert!(class_priority(&options.hooks, gdi) < class_priority(&options.hooks, vulkan));
        assert_eq!(class_priority(&options.hooks, HookBackend::OpenGl.class()), None);
        Ok(())
    }

    #[test]
    fn hook_auto_appends_the_remaining_backends() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            arguments(&["--hook=gdi,auto", "--", "game.exe"])?.hooks,
            [
                HookBackend::Gdi,
                HookBackend::Vulkan,
                HookBackend::OpenGl,
                HookBackend::D3d12,
                HookBackend::D3d11,
            ]
        );
        Ok(())
    }

    #[test]
    fn rejects_unknown_and_repeated_hooks() {
        assert!(arguments(&["--hook", "metal", "--", "game.exe"]).is_err());
        assert!(arguments(&["--hook", "vulkan,vulkan", "--", "game.exe"]).is_err());
    }

    #[test]
    fn accepts_vulkan_override_and_options_in_either_order() -> Result<(), Box<dyn Error>> {
        let options = arguments(&[
            "--track-render-target",
            "--no-isolate",
            "--force-km",
            "--api=vulkan",
            "--",
            "game.exe",
            "--api",
            "target-argument",
        ])?;
        assert_eq!(options.api, ViewerApi::Vulkan);
        assert_eq!(options.track_render_targets, Some(1));
        assert!(!options.isolate);
        assert!(options.force_keyed_mutex);
        assert_eq!(options.target, ["game.exe", "--api", "target-argument"]);
        Ok(())
    }

    #[test]
    fn accepts_a_render_target_count_and_defaults_to_one() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            arguments(&["--track-render-targets", "2", "--", "game.exe"])?.track_render_targets,
            Some(2)
        );
        assert_eq!(
            arguments(&["--track-render-targets", "--", "game.exe"])?.track_render_targets,
            Some(1)
        );
        assert_eq!(
            arguments(&["--track-render-targets=3", "--", "game.exe"])?.track_render_targets,
            Some(3)
        );
        Ok(())
    }

    #[test]
    fn rejects_an_invalid_render_target_count() {
        assert!(arguments(&["--track-render-targets", "0", "--", "game.exe"]).is_err());
        assert!(arguments(&["--track-render-targets=17", "--", "game.exe"]).is_err());
    }

    #[test]
    fn accepts_a_surface_count_and_defaults_to_one() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            arguments(&["--surfaces", "2", "--", "game.exe"])?.surfaces,
            2
        );
        assert_eq!(arguments(&["--surfaces", "--", "game.exe"])?.surfaces, 1);
        assert_eq!(arguments(&["--surfaces=3", "--", "game.exe"])?.surfaces, 3);
        Ok(())
    }

    #[test]
    fn rejects_an_invalid_surface_count() {
        assert!(arguments(&["--surfaces", "0", "--", "game.exe"]).is_err());
        assert!(arguments(&["--surfaces=17", "--", "game.exe"]).is_err());
    }

    #[test]
    fn accepts_a_surface_skip_count_and_defaults_to_one() -> Result<(), Box<dyn Error>> {
        assert_eq!(
            arguments(&["--skip-surfaces", "2", "--", "game.exe"])?.skip_surfaces,
            2
        );
        assert_eq!(
            arguments(&["--skip-surfaces", "--", "game.exe"])?.skip_surfaces,
            1
        );
        assert_eq!(
            arguments(&["--skip-windows=3", "--", "game.exe"])?.skip_surfaces,
            3
        );
        Ok(())
    }

    #[test]
    fn rejects_an_invalid_surface_skip_count() {
        assert!(arguments(&["--skip-surfaces=menu", "--", "game.exe"]).is_err());
    }

    #[test]
    fn originating_api_stream_wins_same_window_duplicates() {
        let d3d11 = stream_backend_class(ExternalImageKind::D3d11D3d12Resource);
        let d3d12 = stream_backend_class(ExternalImageKind::D3d12Resource);
        let opengl = stream_backend_class(ExternalImageKind::OpenGlD3d12Resource);
        let vulkan = stream_backend_class(ExternalImageKind::VulkanD3d12Resource);
        assert!(should_replace_same_window_backend(&AUTO_HOOKS, d3d11, d3d12));
        assert!(should_replace_same_window_backend(&AUTO_HOOKS, d3d12, opengl));
        assert!(should_replace_same_window_backend(&AUTO_HOOKS, d3d12, vulkan));
        assert!(!should_replace_same_window_backend(&AUTO_HOOKS, vulkan, d3d12));
        assert!(!should_replace_same_window_backend(&AUTO_HOOKS, opengl, d3d12));
        // A recreated surface from the same backend still replaces its predecessor.
        assert!(should_replace_same_window_backend(&AUTO_HOOKS, vulkan, vulkan));
        // The submitted hook order overrides the automatic ranking.
        let reversed = [HookBackend::D3d11, HookBackend::Vulkan];
        assert!(!should_replace_same_window_backend(&reversed, d3d11, vulkan));
        assert!(should_replace_same_window_backend(&reversed, vulkan, d3d11));
    }
}

enum TargetChild {
    Standard(Child),
    Isolated(IsolatedChild),
}

struct IsolatedChild {
    process: std::os::windows::io::OwnedHandle,
    process_id: u32,
}

impl TargetChild {
    fn id(&self) -> u32 {
        match self {
            Self::Standard(child) => child.id(),
            Self::Isolated(child) => child.process_id,
        }
    }

    fn raw_handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        use std::os::windows::io::AsRawHandle;

        match self {
            Self::Standard(child) => child.as_raw_handle() as _,
            Self::Isolated(child) => child.process.as_raw_handle(),
        }
    }

    fn kill(&mut self) -> std::io::Result<()> {
        use std::os::windows::io::AsRawHandle;

        match self {
            Self::Standard(child) => child.kill(),
            Self::Isolated(child) => {
                if unsafe {
                    windows_sys::Win32::System::Threading::TerminateProcess(
                        child.process.as_raw_handle(),
                        1,
                    )
                } == 0
                {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            }
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<()>> {
        match self {
            Self::Standard(child) => child.try_wait().map(|status| status.map(drop)),
            Self::Isolated(child) => {
                use std::os::windows::io::AsRawHandle;
                use windows_sys::Win32::{
                    Foundation::{WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
                    System::Threading::WaitForSingleObject,
                };

                match unsafe { WaitForSingleObject(child.process.as_raw_handle(), 0) } {
                    WAIT_OBJECT_0 => Ok(Some(())),
                    WAIT_TIMEOUT => Ok(None),
                    WAIT_FAILED => Err(std::io::Error::last_os_error()),
                    result => Err(std::io::Error::other(format!(
                        "unexpected process wait result {result}"
                    ))),
                }
            }
        }
    }

    fn wait(&mut self) -> std::io::Result<()> {
        match self {
            Self::Standard(child) => child.wait().map(drop),
            Self::Isolated(child) => {
                use std::os::windows::io::AsRawHandle;
                use windows_sys::Win32::{
                    Foundation::{WAIT_FAILED, WAIT_OBJECT_0},
                    System::Threading::{INFINITE, WaitForSingleObject},
                };

                match unsafe { WaitForSingleObject(child.process.as_raw_handle(), INFINITE) } {
                    WAIT_OBJECT_0 => Ok(()),
                    WAIT_FAILED => Err(std::io::Error::last_os_error()),
                    result => Err(std::io::Error::other(format!(
                        "unexpected process wait result {result}"
                    ))),
                }
            }
        }
    }
}

fn append_windows_argument(
    command_line: &mut Vec<u16>,
    value: &OsStr,
    always_quote: bool,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    let value = value.encode_wide().collect::<Vec<_>>();
    if value.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "process argument contains a null character",
        ));
    }
    let quote = always_quote
        || value.is_empty()
        || value
            .iter()
            .any(|unit| *unit == b' ' as u16 || *unit == b'\t' as u16);
    if !quote {
        command_line.extend_from_slice(&value);
        return Ok(());
    }

    command_line.push(b'"' as u16);
    let mut backslashes = 0;
    for unit in value {
        if unit == b'\\' as u16 {
            backslashes += 1;
        } else if unit == b'"' as u16 {
            command_line.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
            command_line.push(unit);
            backslashes = 0;
        } else {
            command_line.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
            command_line.push(unit);
            backslashes = 0;
        }
    }
    command_line.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2));
    command_line.push(b'"' as u16);
    Ok(())
}

fn windows_command_line(target: &[String]) -> std::io::Result<Vec<u16>> {
    let mut command_line = Vec::new();
    append_windows_argument(&mut command_line, OsStr::new(&target[0]), true)?;
    for argument in &target[1..] {
        command_line.push(b' ' as u16);
        append_windows_argument(&mut command_line, OsStr::new(argument), false)?;
    }
    command_line.push(0);
    Ok(command_line)
}

fn windows_environment(overrides: &[(OsString, OsString)]) -> std::io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;

    let mut environment = env::vars_os().collect::<Vec<_>>();
    for (name, value) in overrides {
        if let Some(existing) = environment.iter_mut().find(|(existing, _)| {
            existing
                .to_string_lossy()
                .eq_ignore_ascii_case(&name.to_string_lossy())
        }) {
            existing.1.clone_from(value);
        } else {
            environment.push((name.clone(), value.clone()));
        }
    }
    environment.sort_by_cached_key(|(name, _)| name.to_string_lossy().to_lowercase());

    let mut block = Vec::new();
    for (name, value) in environment {
        let name = name.encode_wide().collect::<Vec<_>>();
        let value = value.encode_wide().collect::<Vec<_>>();
        if name.contains(&0) || value.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "environment contains a null character",
            ));
        }
        block.extend(name);
        block.push(b'=' as u16);
        block.extend(value);
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn spawn_isolated_target(
    target: &[String],
    environment: &[(OsString, OsString)],
    desktop: &str,
) -> std::io::Result<IsolatedChild> {
    use std::{os::windows::io::FromRawHandle, ptr};
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, PROCESS_INFORMATION,
        STARTUPINFOW,
    };

    let mut command_line = windows_command_line(target)?;
    let environment = windows_environment(environment)?;
    let mut desktop = desktop.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        lpDesktop: desktop.as_mut_ptr(),
        ..Default::default()
    };
    let mut process_info = PROCESS_INFORMATION::default();
    if unsafe {
        CreateProcessW(
            ptr::null(),
            command_line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast(),
            ptr::null(),
            &raw const startup,
            &raw mut process_info,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let process =
        unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(process_info.hProcess) };
    let initial_thread =
        unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(process_info.hThread) };
    drop(initial_thread);
    Ok(IsolatedChild {
        process,
        process_id: process_info.dwProcessId,
    })
}

struct TargetProcess {
    child: TargetChild,
    job: KillOnCloseJob,
    desktop: Option<PrivateDesktop>,
}

fn launch_target(
    target: &[String],
    layer_dir: &Path,
    session: &str,
    track_render_targets: Option<u32>,
    force_keyed_mutex: bool,
    hooks: &[HookBackend],
    isolate: bool,
) -> std::io::Result<TargetProcess> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

    let dxgi_hook = layer_dir.join("snowglass_hook_dxgi.dll");
    let d3d11_hook = layer_dir.join("snowglass_hook_d3d11.dll");
    let d3d12_hook = layer_dir.join("snowglass_hook_d3d12.dll");
    let opengl_hook = layer_dir.join("snowglass_hook_opengl32.dll");
    let gdi_hook = layer_dir.join("snowglass_hook_gdi.dll");
    let dinput_hook = layer_dir.join("snowglass_hook_dinput.dll");
    let mut libraries: Vec<(&str, &Path, &std::ffi::CStr)> = Vec::new();
    // Both DXGI producers intercept swapchains through the factory-level hooks.
    if hooks
        .iter()
        .any(|hook| matches!(hook, HookBackend::D3d11 | HookBackend::D3d12))
    {
        libraries.push(("DXGI", dxgi_hook.as_path(), c"SnowglassInstallDxgiHook"));
    }
    if hooks.contains(&HookBackend::D3d11) {
        libraries.push(("D3D11", d3d11_hook.as_path(), c"SnowglassInstallD3d11Hook"));
    }
    if hooks.contains(&HookBackend::D3d12) {
        libraries.push(("D3D12", d3d12_hook.as_path(), c"SnowglassInstallD3d12Hook"));
    }
    if hooks.contains(&HookBackend::OpenGl) {
        libraries.push((
            "OpenGL",
            opengl_hook.as_path(),
            c"SnowglassInstallOpenGlHook",
        ));
    }
    if hooks.contains(&HookBackend::Gdi) {
        libraries.push(("GDI", gdi_hook.as_path(), c"SnowglassInstallGdiHook"));
    }
    libraries.push((
        "DirectInput",
        dinput_hook.as_path(),
        c"SnowglassInstallDinputHook",
    ));
    for &(name, hook, _) in &libraries {
        if hook.is_file() {
            continue;
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{name} hook is missing at {}", hook.display()),
        ));
    }
    let mut command = Command::new(&target[0]);
    command.args(&target[1..]);
    let mut environment = vec![
        (OsString::from("SNOWGLASS_SESSION"), OsString::from(session)),
        (
            OsString::from("SNOWGLASS_LOG_FILE"),
            layer_dir.join("snowglass-layer.log").into_os_string(),
        ),
        (
            OsString::from("SNOWGLASS_DINPUT_LOG_FILE"),
            layer_dir.join("snowglass-dinput.log").into_os_string(),
        ),
        (
            OsString::from("SNOWGLASS_DXGI_LOG_FILE"),
            layer_dir.join("snowglass-dxgi.log").into_os_string(),
        ),
        (
            OsString::from("SNOWGLASS_D3D11_LOG_FILE"),
            layer_dir.join("snowglass-d3d11.log").into_os_string(),
        ),
        (
            OsString::from("SNOWGLASS_D3D12_LOG_FILE"),
            layer_dir.join("snowglass-d3d12.log").into_os_string(),
        ),
        (
            OsString::from("SNOWGLASS_OPENGL_LOG_FILE"),
            layer_dir.join("snowglass-opengl.log").into_os_string(),
        ),
        (
            OsString::from("SNOWGLASS_GDI_LOG_FILE"),
            layer_dir.join("snowglass-gdi.log").into_os_string(),
        ),
    ];
    // The Vulkan producer installs as an instance layer rather than an
    // injected DLL, so its selection travels through the loader environment.
    if hooks.contains(&HookBackend::Vulkan) {
        environment.push((
            OsString::from("VK_LAYER_PATH"),
            layer_dir.as_os_str().to_owned(),
        ));
        environment.push((
            OsString::from("VK_INSTANCE_LAYERS"),
            OsString::from("VK_LAYER_SNOWGLASS_capture"),
        ));
    }
    if let Some(count) = track_render_targets {
        environment.push((
            OsString::from("SNOWGLASS_TRACK_RENDER_TARGET"),
            OsString::from("1"),
        ));
        environment.push((
            OsString::from("SNOWGLASS_TRACK_RENDER_TARGETS"),
            OsString::from(count.to_string()),
        ));
    }
    if force_keyed_mutex {
        environment.push((
            OsString::from("SNOWGLASS_FORCE_KEYED_MUTEX"),
            OsString::from("1"),
        ));
    }
    for (name, value) in &environment {
        command.env(name, value);
    }
    command.creation_flags(CREATE_SUSPENDED);
    let job = KillOnCloseJob::new()?;
    let (desktop, mut child) = if isolate {
        let (desktop, mut attachment) = PrivateDesktop::create_and_attach_current_thread(session)?;
        attachment.restore()?;
        let child = spawn_isolated_target(target, &environment, session)?;
        (Some(desktop), TargetChild::Isolated(child))
    } else {
        (None, TargetChild::Standard(command.spawn()?))
    };
    let process_id = ProcessId(child.id());
    let process = child.raw_handle();
    if let Err(error) = job.assign_process(process) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let libraries = libraries
        .iter()
        .map(|&(_, path, export)| (path, export))
        .collect::<Vec<_>>();
    if let Err(error) = inject_libraries_before_resume(process, process_id, &libraries) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(TargetProcess {
        child,
        job,
        desktop,
    })
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
