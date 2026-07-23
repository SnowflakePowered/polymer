use ash::{Entry, vk};
use clap::ValueEnum;
use egui_ash_renderer::{Options as EguiRendererOptions, Renderer as EguiRenderer};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use snowglass_consumer_vulkan::ImportedImageRing;
use snowglass_hook::config;
use snowglass_input::{
    gamepad::LatestGamepadMailbox,
    mouse::{PointerPhase, layout::Extent},
    provider::sdl3::{
        DeviceSelector, ProviderConfig as Sdl3ProviderConfig, Sdl3Provider, SlotConfig,
    },
    stone::{EMBEDDED_STONE_JSON, InputBinding, SdlAxis, SdlButton, StoneLayout, StoneRegistry},
};
use snowglass_ipc::{LocalSocketListener, StreamReady};
use snowglass_platform_windows::{
    IsolatedProcess, KillOnCloseJob, NativeWindow, PrivateDesktop, ProcessId, forward_character,
    inject_libraries_before_resume, performance_counter, performance_frequency,
};
use snowglass_protocol::{
    AdapterLuid, ExternalImageKind, ExternalSyncKind, FrameReady, RELEASE_ALL_SYNC_VALUE,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    error::Error,
    ffi::OsString,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicIsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::{
    dpi::LogicalSize,
    event::{ElementState, Event, MouseScrollDelta, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::Window,
};

mod cli;
mod d3d12;
mod gui;
mod input;
mod ipc;
mod layout;
mod webrtc;

use cli::{
    GamepadArgument, TrackedRenderTarget, ViewerOptions, parse_target, split_command_arguments,
};
use gui::{
    LauncherState, draw_gamepad_window, draw_launcher_window, draw_layout_css_window,
    draw_statistics_graph,
};
use ipc::{CaptureEvent, UiRequest};

use input::{
    dinput::{
        DirectInputForwarder, InputLatencyStats, RawKeyboardForwarder, cancel_pointer_connection,
        spawn_input_ipc,
    },
    load_window, native_window,
    pointer::{DisplayedSurface, PointerRoutingState, PointerTarget, mouse_button},
    pointer_axes,
};

const FRAMES_IN_FLIGHT: usize = 2;

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
    surface_id: u64,
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

struct DirectCompositor {
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

enum ViewerCompositor {
    Vulkan(Box<DirectCompositor>),
    D3d12(Box<d3d12::D3d12Compositor>),
    Empty,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum ViewerApi {
    #[default]
    D3d12,
    Vulkan,
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum HookChoice {
    Auto,
    Vulkan,
    #[value(name = "opengl")]
    OpenGl,
    D3d12,
    D3d11,
    Gdi,
}

impl HookChoice {
    fn backend(self) -> Option<HookBackend> {
        match self {
            Self::Auto => None,
            Self::Vulkan => Some(HookBackend::Vulkan),
            Self::OpenGl => Some(HookBackend::OpenGl),
            Self::D3d12 => Some(HookBackend::D3d12),
            Self::D3d11 => Some(HookBackend::D3d11),
            Self::Gdi => Some(HookBackend::Gdi),
        }
    }
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
    const ALL: [Self; 5] = [
        Self::Vulkan,
        Self::OpenGl,
        Self::D3d12,
        Self::D3d11,
        Self::Gdi,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Vulkan => "Vulkan",
            Self::OpenGl => "OpenGL",
            Self::D3d12 => "D3D12",
            Self::D3d11 => "D3D11",
            Self::Gdi => "GDI",
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

/// Expands submitted hook choices in order. `auto` appends [`AUTO_HOOKS`],
/// skipping backends that are already listed.
fn expand_hook_choices(
    hooks: &mut Vec<HookBackend>,
    choices: impl IntoIterator<Item = HookChoice>,
) -> Result<(), Box<dyn Error>> {
    for choice in choices {
        if matches!(choice, HookChoice::Auto) {
            for &hook in &AUTO_HOOKS {
                if !hooks.contains(&hook) {
                    hooks.push(hook);
                }
            }
            continue;
        }
        let hook = choice
            .backend()
            .expect("non-auto hook choice has a backend");
        if hooks.contains(&hook) {
            return Err(format!("hook '{choice:?}' is listed more than once").into());
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

fn matching_surface_connection(
    active: &HashMap<u64, (u64, u8)>,
    surface_id: u64,
) -> Option<(u64, u8)> {
    active
        .iter()
        .find_map(|(&connection, &(existing, backend))| {
            (existing == surface_id).then_some((connection, backend))
        })
}

fn graph_surface_index(surface_id: u64) -> Option<u32> {
    const GRAPH_SURFACE_TAG: u64 = 0x5347_0000_0000_0000;
    const GRAPH_SURFACE_TAG_MASK: u64 = 0xffff_0000_0000_0000;
    (surface_id & GRAPH_SURFACE_TAG_MASK == GRAPH_SURFACE_TAG)
        .then_some(surface_id & !GRAPH_SURFACE_TAG_MASK)
        .and_then(|index| u32::try_from(index).ok())
}

fn surface_sort_key(surface_id: u64) -> (u8, u64) {
    graph_surface_index(surface_id).map_or((1, surface_id), |index| (0, u64::from(index)))
}

impl ViewerCompositor {
    fn new(window: &Window, api: ViewerApi, publish_video: bool) -> Result<Self, Box<dyn Error>> {
        match api {
            ViewerApi::D3d12 => Ok(Self::D3d12(Box::new(d3d12::D3d12Compositor::new_default(
                window,
                publish_video,
            )?))),
            ViewerApi::Vulkan => Ok(Self::Vulkan(Box::new(DirectCompositor::new(
                window,
                None,
                true,
                publish_video,
            )?))),
        }
    }

    fn import_stream(
        &mut self,
        window: &Window,
        connection: u64,
        stream: &StreamReady,
        api: ViewerApi,
        publish_video: bool,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        if api == ViewerApi::D3d12 && !d3d12_consumable(stream) {
            return Err(format!(
                "the selected D3D12 viewer cannot consume {:?} output; enable a hook that exports D3D12-compatible resources",
                stream.image_kind
            )
            .into());
        }
        if api == ViewerApi::D3d12 && d3d12_consumable(stream) {
            let matches =
                matches!(self, Self::D3d12(value) if value.adapter_luid() == stream.adapter_luid);
            if !matches {
                drop(std::mem::replace(self, Self::Empty));
                *self = Self::D3d12(Box::new(d3d12::D3d12Compositor::new(
                    window,
                    stream.adapter_luid,
                    publish_video,
                )?));
            }
            let Self::D3d12(value) = self else {
                unreachable!()
            };
            return value.import_stream(connection, stream, quarter_turns);
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
                publish_video,
            )?));
        }
        let Self::Vulkan(value) = self else {
            unreachable!()
        };
        value.import_stream(connection, stream, quarter_turns)
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

    fn displayed_surfaces(&self) -> Vec<DisplayedSurface> {
        match self {
            Self::Vulkan(value) => value.displayed_surfaces(),
            Self::D3d12(value) => value.displayed_surfaces(),
            Self::Empty => Vec::new(),
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

    fn take_video_frames(&mut self) -> Vec<webrtc::VideoFrame> {
        match self {
            Self::Vulkan(value) => std::mem::take(&mut value.completed_video),
            Self::D3d12(value) => value.take_video_frames(),
            Self::Empty => Vec::new(),
        }
    }
}

impl DirectCompositor {
    fn new(
        window: &Window,
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

    fn import_stream(
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
                "rotate_quarter.comp.spv"
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

    fn displayed_surfaces(&self) -> Vec<DisplayedSurface> {
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
                    image_extent,
                    flip_y: surface.flip_y,
                    frame,
                })
            })
            .collect()
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
}

impl Drop for DirectCompositor {
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

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.target().starts_with("webrtc")
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{} {}] {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

fn main() -> Result<(), Box<dyn Error>> {
    if std::env::var_os("SNOWGLASS_WEBRTC_TRACE").is_some() {
        let _ = log::set_boxed_logger(Box::new(StderrLogger));
        log::set_max_level(log::LevelFilter::Trace);
    }
    let options = parse_target()?;
    layout::configure(options.layout_css.as_deref())?;
    let mut layout_css = options
        .layout_css
        .clone()
        .unwrap_or_else(|| layout::DEFAULT_CSS.to_owned());
    let mut layout_css_error = None::<String>;
    let mut show_layout_css = true;
    let mut launcher = LauncherState::from_options(&options);
    let start_immediately = !options.target.is_empty();
    // Rotation is applied viewer-side while compositing. Vulkan graph surface
    // IDs encode this entry order, so reconnect and arrival order cannot move
    // a rotation to another tracked target.
    let mut render_target_rotations = launcher.render_target_rotations();
    let webrtc_targets = options.webrtc.clone();
    let surface_limit = options.surfaces.max(
        webrtc_targets
            .as_ref()
            .and_then(|targets| targets.iter().max().copied())
            .map_or(1, |index| index.saturating_add(1)),
    );
    let mut remaining_surface_skips = options.skip_surfaces;
    let viewer_api = options.api;
    let webrtc = webrtc_targets
        .as_ref()
        .map(|targets| webrtc::Publisher::start(options.bind_webrtc, targets))
        .transpose()?;
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
    let ui_listener = LocalSocketListener::bind(&format!("{session}-ui"))?;
    let gamepad_mailbox = Arc::new(LatestGamepadMailbox::new(u8::MAX as usize + 1));
    let (input_tx, input_latency) = spawn_input_ipc(input_listener, gamepad_mailbox.clone());
    let stone_json = options
        .stone_db
        .as_ref()
        .map(std::fs::read_to_string)
        .transpose()?
        .unwrap_or_else(|| EMBEDDED_STONE_JSON.to_owned());
    let gamepad_registry = StoneRegistry::from_json(&stone_json).map_err(std::io::Error::other)?;
    let mut hooks = launcher.hooks.clone();
    let mut gamepad_provider = None;
    let mut gamepad_layouts = Vec::new();
    let mut gamepad_remap_drafts = Vec::new();
    let mut active_target = None;
    let mut launch_error = None::<String>;
    if start_immediately {
        match start_target_session(
            &launcher,
            &gamepad_registry,
            gamepad_mailbox.clone(),
            &executable_dir,
            &session,
            launcher.tracked_render_target_indices(),
            launcher.graph_tracking,
            options.force_keyed_mutex,
            options.stone_db.as_deref(),
            options.gamecontroller_db.clone(),
        ) {
            Ok(started) => {
                hooks = started.hooks;
                gamepad_layouts = started.layouts;
                gamepad_remap_drafts = vec![HashMap::new(); gamepad_layouts.len()];
                gamepad_provider = Some(started.provider);
                active_target = Some(started.target);
            }
            Err(error) => launch_error = Some(error.to_string()),
        }
    }
    let backing_window = Arc::new(AtomicIsize::new(0));
    let (capture_tx, capture_rx) = mpsc::channel();
    let (ui_tx, ui_rx) = mpsc::channel();
    ipc::spawn_ui(ui_listener, ui_tx);
    // Connections the event loop has evicted (preempted or replaced). The IPC
    // threads close these sockets so the producer stops capturing.
    let revoked_connections = Arc::new(Mutex::new(HashSet::<u64>::new()));
    ipc::spawn(
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
    let publish_video = webrtc.is_some();
    let mut compositor = ViewerCompositor::new(&window, viewer_api, publish_video)?;
    let frequency = performance_frequency().unwrap_or(1) as f64;
    let mut stats = Statistics {
        sample_started: Some(Instant::now()),
        ..Default::default()
    };
    let mut pointer_routing = PointerRoutingState::new(Instant::now());
    let mut pointer_targets = HashMap::<u64, PointerTarget>::new();
    let mut active_capture_connections = HashMap::<u64, (u64, u8)>::new();
    let mut stream_indices = HashMap::<u64, u32>::new();
    let mut skipped_surface_windows = HashSet::<(u64, u8)>::new();
    let mut skipped_surface_backends = HashSet::<u8>::new();
    let mut active_backend = "Waiting";
    let mut active_consumer = "Waiting";
    let mut direct_input = DirectInputForwarder::new(input_tx.clone());
    let mut pressed_keys = HashSet::<KeyCode>::new();
    let mut pending_ui = VecDeque::<UiRequest>::new();

    event_loop.run(move |event, target| match event {
        Event::AboutToWait => {
            window.request_redraw()
        }
        Event::WindowEvent { event, .. } => {
            let response = egui_state.on_window_event(&window, &event);
            if response.repaint {
                window.request_redraw();
            }
            match event {
                WindowEvent::CloseRequested => {
                    if let Some(active) = &active_target {
                        active.stop.store(true, Ordering::Release);
                    }
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
                        if let Some(provider) = &gamepad_provider {
                            provider.clear_keys();
                        }
                        if let Some(connection) = pointer_routing.captured_connection() {
                            let size = window.inner_size();
                            cancel_pointer_connection(
                                connection,
                                &mut pointer_routing,
                                Extent::new(size.width, size.height),
                                &compositor.displayed_surfaces(),
                                &pointer_targets,
                                &input_tx,
                                &mut direct_input,
                            );
                        }
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if !pending_ui.is_empty() {
                        return;
                    }
                    if let PhysicalKey::Code(code) = event.physical_key {
                        let pressed = event.state == ElementState::Pressed;
                        match event.state {
                            ElementState::Pressed => {
                                pressed_keys.insert(code);
                            }
                            ElementState::Released => {
                                pressed_keys.remove(&code);
                            }
                        }
                        if let Some(provider) = &gamepad_provider {
                            provider.set_key(format!("{code:?}"), pressed);
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
                    if !pending_ui.is_empty() {
                        return;
                    }
                    pointer_routing.set_cursor(position.x, position.y);
                    let size = window.inner_size();
                    let surfaces = compositor.displayed_surfaces();
                    if pointer_routing.send(
                        PointerPhase::Move,
                        0,
                        0,
                        0,
                        Extent::new(size.width, size.height),
                        &surfaces,
                        &pointer_targets,
                        &input_tx,
                    ) {
                        if let Some(mapped) = pointer_routing.last_sent() {
                            if let Some(provider) = &gamepad_provider {
                                provider.set_pointer(pointer_axes(
                                    mapped.normalized_x,
                                    mapped.normalized_y,
                                ));
                            }
                            direct_input.mouse_position(
                                mapped.surface_id,
                                mapped.x,
                                mapped.y,
                                mapped.coordinate_width,
                                mapped.coordinate_height,
                            );
                        }
                    } else {
                        if let Some(provider) = &gamepad_provider {
                            provider.set_pointer(None);
                        }
                        direct_input.reset_mouse_position();
                    }
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    if !pending_ui.is_empty() {
                        return;
                    }
                    if let Some(button) = mouse_button(button) {
                        let phase = if state == ElementState::Pressed {
                            PointerPhase::ButtonDown
                        } else {
                            PointerPhase::ButtonUp
                        };
                        let size = window.inner_size();
                        let surfaces = compositor.displayed_surfaces();
                        if pointer_routing.send(
                            phase,
                            u32::from(button),
                            0,
                            0,
                            Extent::new(size.width, size.height),
                            &surfaces,
                            &pointer_targets,
                            &input_tx,
                        ) {
                            direct_input.mouse_button(button, state == ElementState::Pressed);
                        }
                    }
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    if !pending_ui.is_empty() {
                        return;
                    }
                    let (horizontal, vertical) = match delta {
                        MouseScrollDelta::LineDelta(horizontal, vertical) => {
                            ((horizontal * 120.0) as i32, (vertical * 120.0) as i32)
                        }
                        MouseScrollDelta::PixelDelta(delta) => (delta.x as i32, delta.y as i32),
                    };
                    let size = window.inner_size();
                    let surfaces = compositor.displayed_surfaces();
                    if pointer_routing.send(
                        PointerPhase::Wheel,
                        0,
                        horizontal,
                        vertical,
                        Extent::new(size.width, size.height),
                        &surfaces,
                        &pointer_targets,
                        &input_tx,
                    ) {
                        direct_input.mouse_wheel(vertical);
                    }
                }
                WindowEvent::RedrawRequested => {
                    pending_ui.extend(ui_rx.try_iter());
                    let size = window.inner_size();
                    if size.width == 0 || size.height == 0 {
                        return;
                    }
                    if active_target
                        .as_ref()
                        .is_some_and(|active| active.done.try_recv().is_ok())
                    {
                        active_target = None;
                        gamepad_provider = None;
                        backing_window.store(0, Ordering::Release);
                        for connection in active_capture_connections.keys().copied().collect::<Vec<_>>() {
                            let _ = compositor.disconnect_stream(connection);
                        }
                        active_capture_connections.clear();
                        pointer_targets.clear();
                        stream_indices.clear();
                        active_backend = "Waiting";
                        active_consumer = "Waiting";
                    }
                    while let Ok(capture) = capture_rx.try_recv() {
                        match capture {
                            CaptureEvent::Stream(connection, stream, accepted) => {
                                let surface_identity = (
                                    stream.surface_id.0,
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
                                let previously_skipped = stream.surface_id.0 != 0
                                    && skipped_surface_windows.contains(&surface_identity);
                                if previously_skipped
                                    || (!active_capture_connections.contains_key(&connection)
                                        && remaining_surface_skips != 0)
                                {
                                    if !previously_skipped {
                                        remaining_surface_skips -= 1;
                                        if stream.surface_id.0 != 0 {
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
                                    let replaced = (stream.surface_id.0 != 0)
                                        .then(|| matching_surface_connection(
                                            &active_capture_connections,
                                            stream.surface_id.0,
                                        ))
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
                                        let size = window.inner_size();
                                        cancel_pointer_connection(
                                            existing,
                                            &mut pointer_routing,
                                            Extent::new(size.width, size.height),
                                            &compositor.displayed_surfaces(),
                                            &pointer_targets,
                                            &input_tx,
                                            &mut direct_input,
                                        );
                                        active_capture_connections.remove(&existing);
                                        stream_indices.remove(&existing);
                                        pointer_targets.remove(&existing);
                                        if let Some(publisher) = &webrtc {
                                            publisher.remove_stream(existing);
                                        }
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
                                        let size = window.inner_size();
                                        cancel_pointer_connection(
                                            existing,
                                            &mut pointer_routing,
                                            Extent::new(size.width, size.height),
                                            &compositor.displayed_surfaces(),
                                            &pointer_targets,
                                            &input_tx,
                                            &mut direct_input,
                                        );
                                        active_capture_connections.remove(&existing);
                                        stream_indices.remove(&existing);
                                        pointer_targets.remove(&existing);
                                        if let Some(publisher) = &webrtc {
                                            publisher.remove_stream(existing);
                                        }
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
                                        let size = window.inner_size();
                                        cancel_pointer_connection(
                                            existing,
                                            &mut pointer_routing,
                                            Extent::new(size.width, size.height),
                                            &compositor.displayed_surfaces(),
                                            &pointer_targets,
                                            &input_tx,
                                            &mut direct_input,
                                        );
                                        active_capture_connections.remove(&existing);
                                        stream_indices.remove(&existing);
                                        pointer_targets.remove(&existing);
                                        if let Some(publisher) = &webrtc {
                                            publisher.remove_stream(existing);
                                        }
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
                                let stream_index = stream_indices
                                    .get(&connection)
                                    .copied()
                                    .or_else(|| {
                                        graph_surface_index(stream.surface_id.0)
                                            .filter(|index| *index < surface_limit)
                                    })
                                    .or_else(|| {
                                        (0..surface_limit)
                                            .find(|index| !stream_indices.values().any(|v| v == index))
                                    })
                                    .expect("surface limit admitted a stream index");
                                let publish_stream = webrtc_targets
                                    .as_ref()
                                    .is_some_and(|targets| targets.contains(&stream_index));
                                let quarter_turns = render_target_rotations
                                    .get(stream_index as usize)
                                    .copied()
                                    .unwrap_or(0);
                                match compositor.import_stream(
                                    &window,
                                    connection,
                                    &stream,
                                    viewer_api,
                                    publish_video,
                                    quarter_turns,
                                ) {
                                    Ok(()) => {
                                        stream_indices.insert(connection, stream_index);
                                        if publish_stream
                                            && let Some(publisher) = &webrtc
                                        {
                                            publisher.add_stream(connection, stream_index);
                                        }
                                        active_capture_connections
                                            .insert(connection, surface_identity);
                                        pointer_targets.insert(
                                            connection,
                                            PointerTarget {
                                                surface_id: stream.surface_id.0,
                                                target_window: stream.source_window,
                                            },
                                        );
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
                                        launch_error = Some(format!(
                                            "The selected hooks did not publish output compatible with the fixed {viewer_api:?} viewer: {error}"
                                        ));
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
                                if let Some(publisher) = &webrtc {
                                    publisher.remove_stream(connection);
                                }
                                let size = window.inner_size();
                                cancel_pointer_connection(
                                    connection,
                                    &mut pointer_routing,
                                    Extent::new(size.width, size.height),
                                    &compositor.displayed_surfaces(),
                                    &pointer_targets,
                                    &input_tx,
                                    &mut direct_input,
                                );
                                stream_indices.remove(&connection);
                                pointer_targets.remove(&connection);
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
                    let webrtc_stats = webrtc.as_ref().map(webrtc::Publisher::stats);
                    let last_pointer_sent = pointer_routing.last_sent();
                    let mut launch_requested = false;
                    let mut close_requested = false;
                    let output = egui_context.run(input, |context| {
                        egui::TopBottomPanel::top("statistics").show(context, |ui| {
                            ui.horizontal(|ui| {
                                ui.strong(format!(
                                    "Snowglass {active_backend} via {active_consumer}"
                                ));
                                ui.toggle_value(&mut show_layout_css, "Layout CSS");
                                if active_target.is_some() && ui.button("Close program").clicked() {
                                    close_requested = true;
                                }
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
                                if let Some(sent) = last_pointer_sent {
                                    let color = if sent.sent_at.elapsed()
                                        <= Duration::from_millis(350)
                                    {
                                        egui::Color32::LIGHT_GREEN
                                    } else {
                                        egui::Color32::GRAY
                                    };
                                    ui.colored_label(
                                        color,
                                        format!(
                                            "Mouse sent #{} {:?} @ {},{}",
                                            sent.count, sent.phase, sent.x, sent.y
                                        ),
                                    )
                                    .on_hover_text(format!(
                                        "Surface {} in {}x{} presentation coordinates",
                                        sent.surface_id,
                                        sent.coordinate_width,
                                        sent.coordinate_height
                                    ));
                                } else {
                                    ui.colored_label(egui::Color32::GRAY, "Mouse sent: none");
                                }
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
                        draw_layout_css_window(
                            context,
                            &mut show_layout_css,
                            &mut layout_css,
                            &mut layout_css_error,
                        );
                        if active_target.is_none() {
                            launch_requested = draw_launcher_window(
                                context,
                                &mut launcher,
                                &gamepad_registry,
                                &mut launch_error,
                            );
                        } else if let Some(message) = launch_error.clone() {
                            egui::Window::new("Snowglass error")
                                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                                .collapsible(false)
                                .show(context, |ui| {
                                    ui.colored_label(egui::Color32::LIGHT_RED, message);
                                    if ui.button("Dismiss").clicked() {
                                        launch_error = None;
                                    }
                                });
                        }
                        if let Some(forwarded) = pending_ui.front() {
                            let request = &forwarded.request;
                            let title = if request.caption.is_empty() {
                                "Guest message"
                            } else {
                                &request.caption
                            };
                            let mut selected = None;
                            if context.input(|input| input.key_pressed(egui::Key::Escape)) {
                                selected = request.escape_result;
                            }
                            egui::Window::new(title)
                                .id(egui::Id::new("snowglass-forwarded-message-box"))
                                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                                .collapsible(false)
                                .resizable(true)
                                .show(context, |ui| {
                                    ui.set_max_width(720.0);
                                    ui.label(&request.text);
                                    ui.add_space(12.0);
                                    ui.horizontal(|ui| {
                                        for button in &request.buttons {
                                            let response = ui.button(&button.label);
                                            if request.default_result == Some(button.result) {
                                                response.request_focus();
                                            }
                                            if response.clicked() {
                                                selected = Some(button.result);
                                            }
                                        }
                                    });
                                });
                            if let Some(result) = selected
                                && let Some(forwarded) = pending_ui.pop_front()
                            {
                                let _ = forwarded.response.send(
                                    snowglass_ipc::ui::MessageBoxResponse { result },
                                );
                            }
                        }
                        let statistics_frame = egui::Frame::window(&context.style())
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(egui::Stroke::new(
                                1.0_f32,
                                egui::Color32::from_white_alpha(48),
                            ))
                            .shadow(egui::epaint::Shadow::NONE);
                        let mut show_frame_graph = active_target.is_some();
                        egui::Window::new("Frame Graph")
                            .open(&mut show_frame_graph)
                            .default_pos(egui::pos2(12.0, 48.0))
                            .resizable(false)
                            .collapsible(true)
                            .frame(statistics_frame)
                            .show(context, |ui| {
                                draw_statistics_graph(ui, &stats.history);
                            });
                        if let Some(provider) = &gamepad_provider {
                            draw_gamepad_window(
                                context,
                                provider,
                                &gamepad_layouts,
                                &mut gamepad_remap_drafts,
                                &input_latency,
                            );
                        }
                        if let Some(webrtc_stats) = &webrtc_stats
                            && webrtc_stats.connected_clients > 0
                        {
                            let webrtc_frame = egui::Frame::window(&context.style())
                                .fill(egui::Color32::TRANSPARENT)
                                .stroke(egui::Stroke::new(
                                    1.0_f32,
                                    egui::Color32::from_white_alpha(48),
                                ))
                                .shadow(egui::epaint::Shadow::NONE);
                            egui::Window::new("WebRTC Stats")
                                .default_pos(egui::pos2(12.0, 270.0))
                                .resizable(false)
                                .collapsible(true)
                                .frame(webrtc_frame)
                                .show(context, |ui| {
                                    egui::Grid::new("webrtc_statistics_grid")
                                        .num_columns(2)
                                        .spacing(egui::vec2(16.0, 4.0))
                                        .show(ui, |ui| {
                                            ui.label("Encode bitrate");
                                            ui.monospace(format!(
                                                "{:.2} Mbps",
                                                webrtc_stats.bitrate_bps / 1_000_000.0
                                            ));
                                            ui.end_row();
                                            ui.label("Encode latency");
                                            ui.monospace(format!(
                                                "{:.2} ms",
                                                webrtc_stats.encode_latency_ms
                                            ));
                                            ui.end_row();
                                            ui.label("Connected clients");
                                            ui.monospace(
                                                webrtc_stats.connected_clients.to_string(),
                                            );
                                            ui.end_row();
                                            ui.label("Render targets encoded");
                                            ui.monospace(webrtc_stats.render_targets.to_string());
                                            ui.end_row();
                                            ui.label("Encoder");
                                            ui.monospace(if webrtc_stats.encoders.is_empty() {
                                                "Waiting for frames"
                                            } else {
                                                &webrtc_stats.encoders
                                            });
                                            ui.end_row();
                                            ui.label("Codec");
                                            ui.monospace(if webrtc_stats.codecs.is_empty() {
                                                "Waiting for frames"
                                            } else {
                                                &webrtc_stats.codecs
                                            });
                                            ui.end_row();
                                        });
                                });
                        }
                    });
                    if close_requested
                        && let Some(active) = &active_target
                    {
                        active.stop.store(true, Ordering::Release);
                    }
                    if launch_requested && active_target.is_none() {
                        launch_error = None;
                        match start_target_session(
                            &launcher,
                            &gamepad_registry,
                            gamepad_mailbox.clone(),
                            &executable_dir,
                            &session,
                            launcher.tracked_render_target_indices(),
                            launcher.graph_tracking,
                            options.force_keyed_mutex,
                            options.stone_db.as_deref(),
                            options.gamecontroller_db.clone(),
                        ) {
                            Ok(started) => {
                                hooks = started.hooks;
                                render_target_rotations = launcher.render_target_rotations();
                                gamepad_layouts = started.layouts;
                                gamepad_remap_drafts =
                                    vec![HashMap::new(); gamepad_layouts.len()];
                                gamepad_provider = Some(started.provider);
                                active_target = Some(started.target);
                                remaining_surface_skips = options.skip_surfaces;
                                stats = Statistics {
                                    sample_started: Some(Instant::now()),
                                    ..Default::default()
                                };
                            }
                            Err(error) => launch_error = Some(error.to_string()),
                        }
                    }
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
                    if let Some(publisher) = &webrtc {
                        for frame in compositor.take_video_frames() {
                            publisher.publish(frame);
                        }
                    }
                }
                _ => {}
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

struct ActiveTarget {
    stop: Arc<AtomicBool>,
    done: mpsc::Receiver<()>,
}

struct StartedTarget {
    target: ActiveTarget,
    provider: Sdl3Provider,
    layouts: Vec<StoneLayout>,
    hooks: Vec<HookBackend>,
}

#[expect(clippy::too_many_arguments)]
fn start_target_session(
    launcher: &LauncherState,
    registry: &StoneRegistry,
    mailbox: Arc<LatestGamepadMailbox>,
    executable_dir: &Path,
    session: &str,
    tracked_render_targets: Option<Vec<u32>>,
    graph_tracking: bool,
    force_keyed_mutex: bool,
    stone_database: Option<&Path>,
    gamecontroller_database: Option<PathBuf>,
) -> Result<StartedTarget, Box<dyn Error>> {
    let target = launcher.target().map_err(std::io::Error::other)?;
    if launcher.hooks.is_empty() {
        return Err(std::io::Error::other("Select at least one graphics hook").into());
    }
    launcher
        .validate_render_targets()
        .map_err(std::io::Error::other)?;
    let gamepads = launcher
        .gamepad_arguments()
        .map_err(std::io::Error::other)?;
    let layouts = gamepads
        .iter()
        .map(|pad| {
            registry.layout(&pad.layout).cloned().ok_or_else(|| {
                std::io::Error::other(format!("Unknown Stone layout {}", pad.layout))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let provider = Sdl3Provider::start(
        Sdl3ProviderConfig {
            slots: gamepads
                .iter()
                .map(|pad| SlotConfig {
                    selector: pad.selector.clone(),
                    layout_id: pad.layout.clone(),
                    force_mouse: pad.force_mouse,
                })
                .collect(),
            stone_database: stone_database.map(Path::to_path_buf),
            gamecontroller_database,
            keyboard_mapping_directory: None,
        },
        mailbox,
    )
    .map_err(|error| std::io::Error::other(format!("SDL3 gamepad input failed: {error}")))?;
    let process = launch_target(
        &target,
        executable_dir,
        session,
        tracked_render_targets,
        graph_tracking,
        force_keyed_mutex,
        &launcher.hooks,
        launcher.isolate,
        &layouts
            .iter()
            .map(|layout| layout.id.clone())
            .collect::<Vec<_>>(),
        stone_database,
    )?;
    let stop = Arc::new(AtomicBool::new(false));
    let (done_tx, done) = mpsc::channel();
    spawn_target_monitor(process, stop.clone(), done_tx);
    Ok(StartedTarget {
        target: ActiveTarget { stop, done },
        provider,
        layouts,
        hooks: launcher.hooks.clone(),
    })
}

enum TargetChild {
    Standard(Child),
    Isolated(IsolatedProcess),
}

impl TargetChild {
    fn id(&self) -> u32 {
        match self {
            Self::Standard(child) => child.id(),
            Self::Isolated(child) => child.id(),
        }
    }

    fn raw_handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        use std::os::windows::io::AsRawHandle;

        match self {
            Self::Standard(child) => child.as_raw_handle() as _,
            Self::Isolated(child) => child.raw_handle(),
        }
    }

    fn kill(&mut self) -> std::io::Result<()> {
        match self {
            Self::Standard(child) => child.kill(),
            Self::Isolated(child) => child.kill(),
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<()>> {
        match self {
            Self::Standard(child) => child.try_wait().map(|status| status.map(drop)),
            Self::Isolated(child) => child.try_wait(),
        }
    }

    fn wait(&mut self) -> std::io::Result<()> {
        match self {
            Self::Standard(child) => child.wait().map(drop),
            Self::Isolated(child) => child.wait(),
        }
    }
}

struct TargetProcess {
    child: TargetChild,
    job: KillOnCloseJob,
    desktop: Option<PrivateDesktop>,
}

#[expect(clippy::too_many_arguments)]
fn launch_target(
    target: &[String],
    layer_dir: &Path,
    session: &str,
    track_render_targets: Option<Vec<u32>>,
    graph_tracking: bool,
    force_keyed_mutex: bool,
    hooks: &[HookBackend],
    isolate: bool,
    stone_layouts: &[String],
    stone_database: Option<&Path>,
) -> std::io::Result<TargetProcess> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

    let dxgi_hook = layer_dir.join("snowglass_hook_dxgi.dll");
    let d3d11_hook = layer_dir.join("snowglass_hook_d3d11.dll");
    let d3d12_hook = layer_dir.join("snowglass_hook_d3d12.dll");
    let opengl_hook = layer_dir.join("snowglass_hook_opengl32.dll");
    let gdi_hook = layer_dir.join("snowglass_hook_gdi.dll");
    let dinput_hook = layer_dir.join("snowglass_hook_dinput.dll");
    let win32_hook = layer_dir.join("snowglass_hook_win32.dll");
    let ui_hook = layer_dir.join("snowglass_hook_ui.dll");
    let sdl2_hook = layer_dir.join("snowglass_hook_sdl2.dll");
    let sdl3_hook = layer_dir.join("snowglass_hook_sdl3.dll");
    let sdl3_runtime = layer_dir.join("SDL3.dll");
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
    libraries.push((
        "Win32 input",
        win32_hook.as_path(),
        c"SnowglassInstallWin32Hook",
    ));
    if isolate {
        libraries.push(("UI", ui_hook.as_path(), c"SnowglassInstallUiHook"));
    }
    libraries.push(("SDL2", sdl2_hook.as_path(), c"SnowglassInstallSdl2Hook"));
    libraries.push(("SDL3", sdl3_hook.as_path(), c"SnowglassInstallSdl3Hook"));
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
        (OsString::from(config::SESSION), OsString::from(session)),
        (
            OsString::from(config::STONE_LAYOUTS),
            OsString::from(stone_layouts.join(",")),
        ),
        (
            OsString::from(config::SDL_GAMEPADS),
            OsString::from(stone_layouts.len().to_string()),
        ),
        (
            OsString::from(config::LAYER_LOG_FILE),
            layer_dir.join("snowglass-layer.log").into_os_string(),
        ),
        (
            OsString::from(config::DINPUT_LOG_FILE),
            layer_dir.join("snowglass-dinput.log").into_os_string(),
        ),
        (
            OsString::from(config::WIN32_LOG_FILE),
            layer_dir.join("snowglass-win32.log").into_os_string(),
        ),
        (
            OsString::from(config::SDL_LOG_FILE),
            layer_dir.join("snowglass-sdl.log").into_os_string(),
        ),
        (
            OsString::from(config::DXGI_LOG_FILE),
            layer_dir.join("snowglass-dxgi.log").into_os_string(),
        ),
        (
            OsString::from(config::D3D11_LOG_FILE),
            layer_dir.join("snowglass-d3d11.log").into_os_string(),
        ),
        (
            OsString::from(config::D3D12_LOG_FILE),
            layer_dir.join("snowglass-d3d12.log").into_os_string(),
        ),
        (
            OsString::from(config::OPENGL_LOG_FILE),
            layer_dir.join("snowglass-opengl.log").into_os_string(),
        ),
        (
            OsString::from(config::GDI_LOG_FILE),
            layer_dir.join("snowglass-gdi.log").into_os_string(),
        ),
        // Physical controllers belong exclusively to the viewer-side SDL3
        // provider. Disable every Windows hardware joystick backend before
        // the target initializes SDL; the virtual driver remains available.
        (OsString::from("SDL_JOYSTICK_HIDAPI"), OsString::from("0")),
        (
            OsString::from("SDL_JOYSTICK_DIRECTINPUT"),
            OsString::from("0"),
        ),
        (OsString::from("SDL_JOYSTICK_RAWINPUT"), OsString::from("0")),
        (OsString::from("SDL_JOYSTICK_WGI"), OsString::from("0")),
        (
            OsString::from("SDL_JOYSTICK_GAMEINPUT"),
            OsString::from("0"),
        ),
        (OsString::from("SDL_XINPUT_ENABLED"), OsString::from("0")),
    ];
    if let Some(path) = stone_database {
        environment.push((
            OsString::from(config::STONE_DATABASE),
            path.as_os_str().to_owned(),
        ));
    }
    // SDL3's dynamic API redirects calls from statically linked applications
    // (including Dolphin) through this DLL, making its public entry points
    // available to the SDL3 hook. Preserve an explicit caller override.
    if std::env::var_os("SDL3_DYNAMIC_API").is_none() && sdl3_runtime.is_file() {
        environment.push((
            OsString::from("SDL3_DYNAMIC_API"),
            sdl3_runtime.into_os_string(),
        ));
    }
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
    if let Some(indices) = track_render_targets {
        environment.push((
            OsString::from(config::TRACK_RENDER_TARGET),
            OsString::from("1"),
        ));
        environment.push((
            OsString::from(config::TRACK_RENDER_TARGETS),
            OsString::from(
                indices
                    .iter()
                    .map(|index| index.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ));
        if graph_tracking {
            environment.push((OsString::from(config::GRAPH_TRACKING), OsString::from("1")));
        }
    }
    if force_keyed_mutex {
        environment.push((
            OsString::from(config::FORCE_KEYED_MUTEX),
            OsString::from("1"),
        ));
    }
    for (name, value) in &environment {
        command.env(name, value);
    }
    command.creation_flags(CREATE_SUSPENDED);
    let job = KillOnCloseJob::new()?;
    let (desktop, mut child) = if isolate {
        let desktop = PrivateDesktop::create(session)?;
        let child = IsolatedProcess::spawn_suspended(target, &environment, session)?;
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

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

unsafe fn queue_family_video_operations(
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

/// Source offsets for a compositing blit. Mirrored blits express vertical
/// flips and half turns; quarter turns transpose through the rotation compute
/// pass instead and pass zero turns here.
fn blit_source_offsets(
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

fn vertical_layout_index(index: u32, count: u32, flip_y: bool) -> u32 {
    if flip_y {
        count.saturating_sub(index + 1)
    } else {
        index
    }
}
