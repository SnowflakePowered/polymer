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
    InjectionLibrary, NativeWindow, TargetProcess, architecture_artifact_dir,
    executable_architecture, forward_character, performance_counter, performance_frequency,
};
use snowglass_protocol::{ExternalImageKind, ExternalSyncKind, FrameReady};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    error::Error,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicIsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, Event, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

#[cfg(test)]
mod argument_tests;
mod gui;
mod input;
mod ipc;
mod layout;

use gui::{LauncherState, draw_gamepad_window, draw_launcher_window, draw_layout_css_window};
use ipc::UiRequest;
#[cfg(test)]
use snowglass_renderer::cli::{AUTO_HOOKS, parse_arguments};
use snowglass_renderer::{
    CompositingPlan, D3d12Compositor, Presentation, RenderWindow, Renderer, VulkanCompositor,
    WebRtcCompositor,
    capture::{Capture, CaptureEvent, ConsumerPreference},
    cli::{
        GamepadArgument, HookBackend, TrackedRenderTarget, ViewerApi, ViewerOptions, parse_target,
        split_command_arguments,
    },
    debug::{
        FrameSequenceTracker, TargetStatisticsDiagnostics, TargetStatisticsTracker,
        draw_statistics_graph,
    },
    input::PointerRoutingState,
    webrtc,
};

use input::{
    dinput::{
        DirectInputForwarder, InputLatencyStats, RawKeyboardForwarder, cancel_pointer_connection,
        spawn_input_ipc,
    },
    load_window, native_window,
    pointer::mouse_button,
    pointer_axes,
};

type EventCallback = Box<dyn FnMut(Event<()>, &ActiveEventLoop)>;
type EventInitializer = Box<dyn FnOnce(&ActiveEventLoop) -> Result<EventCallback, Box<dyn Error>>>;

struct ViewerApplication {
    initializer: Option<EventInitializer>,
    callback: Option<EventCallback>,
    initialization_error: Option<Box<dyn Error>>,
}

impl ViewerApplication {
    fn new(
        initializer: impl FnOnce(&ActiveEventLoop) -> Result<EventCallback, Box<dyn Error>> + 'static,
    ) -> Self {
        Self {
            initializer: Some(Box::new(initializer)),
            callback: None,
            initialization_error: None,
        }
    }

    fn dispatch(&mut self, event: Event<()>, event_loop: &ActiveEventLoop) {
        if let Some(callback) = &mut self.callback {
            callback(event, event_loop);
        }
    }

    fn finish(self) -> Result<(), Box<dyn Error>> {
        self.initialization_error.map_or(Ok(()), Err)
    }
}

impl ApplicationHandler for ViewerApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.callback.is_none()
            && let Some(initializer) = self.initializer.take()
        {
            match initializer(event_loop) {
                Ok(callback) => self.callback = Some(callback),
                Err(error) => {
                    self.initialization_error = Some(error);
                    event_loop.exit();
                    return;
                }
            }
        }
        self.dispatch(Event::Resumed, event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        self.dispatch(Event::WindowEvent { window_id, event }, event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.dispatch(Event::AboutToWait, event_loop);
    }
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
    sequences: FrameSequenceTracker,
    target_statistics_tracker: TargetStatisticsTracker,
    target_statistics: Vec<TargetStatisticsDiagnostics>,
    dropped: u64,
}

enum ViewerCompositor {
    Vulkan(Box<VulkanCompositor>),
    D3d12(Box<D3d12Compositor>),
    Empty,
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

impl ViewerCompositor {
    fn new(
        window: &Window,
        api: ViewerApi,
        publish_video: bool,
    ) -> Result<(Self, Option<WebRtcCompositor>), Box<dyn Error>> {
        match api {
            ViewerApi::D3d12 if publish_video => {
                let (compositor, webrtc) = D3d12Compositor::new_default_with_webrtc(window)?;
                Ok((Self::D3d12(Box::new(compositor)), Some(webrtc)))
            }
            ViewerApi::D3d12 => Ok((
                Self::D3d12(Box::new(D3d12Compositor::new_default(window)?)),
                None,
            )),
            ViewerApi::Vulkan if publish_video => {
                let (compositor, webrtc) = VulkanCompositor::new_with_webrtc(window, None, true)?;
                Ok((Self::Vulkan(Box::new(compositor)), Some(webrtc)))
            }
            ViewerApi::Vulkan => Ok((
                Self::Vulkan(Box::new(VulkanCompositor::new(window, None, true)?)),
                None,
            )),
        }
    }

    fn import_stream(
        &mut self,
        window: &Window,
        connection: u64,
        stream: &StreamReady,
        api: ViewerApi,
        publish_video: bool,
        publish_stream: bool,
        quarter_turns: u8,
        video: &mut Option<WebRtcCompositor>,
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
                *video = None;
                drop(std::mem::replace(self, Self::Empty));
                if publish_video {
                    let (compositor, webrtc) =
                        D3d12Compositor::new_with_webrtc(window, stream.adapter_luid)?;
                    *self = Self::D3d12(Box::new(compositor));
                    *video = Some(webrtc);
                } else {
                    *self =
                        Self::D3d12(Box::new(D3d12Compositor::new(window, stream.adapter_luid)?));
                    *video = None;
                }
            }
            let Self::D3d12(value) = self else {
                unreachable!()
            };
            Renderer::import_stream(value.as_mut(), connection, stream, quarter_turns)?;
            if publish_stream && let Some(video) = video {
                video.import_d3d12_stream(connection)?;
            }
            return Ok(());
        }

        let needs_keyed_mutex = stream.image_kind == ExternalImageKind::D3d11SharedResourceNt;
        let adapter = stream.adapter_luid;
        let matches = matches!(self, Self::Vulkan(value)
            if (!needs_keyed_mutex || value.keyed_mutex_enabled())
                && ((adapter.low_part == 0 && adapter.high_part == 0) || value.matches_adapter(adapter)));
        if !matches {
            *video = None;
            drop(std::mem::replace(self, Self::Empty));
            let preferred = (adapter.low_part != 0 || adapter.high_part != 0).then_some(adapter);
            if publish_video {
                let (compositor, webrtc) =
                    VulkanCompositor::new_with_webrtc(window, preferred, needs_keyed_mutex)?;
                *self = Self::Vulkan(Box::new(compositor));
                *video = Some(webrtc);
            } else {
                *self = Self::Vulkan(Box::new(VulkanCompositor::new(
                    window,
                    preferred,
                    needs_keyed_mutex,
                )?));
                *video = None;
            }
        }
        let Self::Vulkan(value) = self else {
            unreachable!()
        };
        Renderer::import_stream(value.as_mut(), connection, stream, quarter_turns)?;
        if publish_stream && let Some(video) = video {
            video.import_vulkan_stream(value, connection)?;
        }
        Ok(())
    }

    fn recreate_swapchain(&mut self, window: &Window) -> Result<bool, Box<dyn Error>> {
        match self {
            Self::Vulkan(value) => Renderer::recreate_swapchain(value.as_mut(), window),
            Self::D3d12(value) => Renderer::recreate_swapchain(value.as_mut(), window),
            Self::Empty => Ok(false),
        }
    }

    fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        match self {
            Self::Vulkan(value) => Renderer::set_latest(value.as_mut(), connection, frame),
            Self::D3d12(value) => Renderer::set_latest(value.as_mut(), connection, frame),
            Self::Empty => {}
        }
    }

    fn presentation(&self, viewer_extent: Extent) -> Presentation {
        match self {
            Self::Vulkan(value) => Renderer::received_presentation(value.as_ref(), viewer_extent),
            Self::D3d12(value) => Renderer::received_presentation(value.as_ref(), viewer_extent),
            Self::Empty => Presentation::new(viewer_extent, Vec::new()),
        }
    }

    fn resize_windows(&self, window: &impl RenderWindow) -> Vec<snowglass_protocol::ResizeWindow> {
        let presentation = self.presentation(window.render_extent());
        let plan = layout::composition_plan(&presentation);
        match self {
            Self::Vulkan(value) => Renderer::resize_windows(value.as_ref(), window, &plan),
            Self::D3d12(value) => Renderer::resize_windows(value.as_ref(), window, &plan),
            Self::Empty => Vec::new(),
        }
    }

    fn has_pending_frame(&self) -> bool {
        match self {
            Self::Vulkan(value) => Renderer::has_pending_frame(value.as_ref()),
            Self::D3d12(value) => Renderer::has_pending_frame(value.as_ref()),
            Self::Empty => false,
        }
    }

    fn disconnect_stream(
        &mut self,
        connection: u64,
        video: &mut Option<WebRtcCompositor>,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(video) = video {
            video.disconnect_stream(connection);
        }
        match self {
            Self::Vulkan(value) => Renderer::disconnect_stream(value.as_mut(), connection),
            Self::D3d12(value) => Renderer::disconnect_stream(value.as_mut(), connection),
            Self::Empty => Ok(()),
        }
    }

    fn draw(
        &mut self,
        output: &egui::FullOutput,
        primitives: &[egui::ClippedPrimitive],
        plan: &CompositingPlan,
        video: Option<&mut WebRtcCompositor>,
    ) -> Result<bool, Box<dyn Error>> {
        match self {
            Self::Vulkan(value) => value.draw_with_webrtc(output, primitives, plan, video),
            Self::D3d12(value) => value.draw_with_webrtc(output, primitives, plan, video),
            Self::Empty => Ok(false),
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
    let resize_guest = options.resize;
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
            launcher.render_target_surface_patterns(),
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
    let (ui_tx, ui_rx) = mpsc::channel();
    ipc::spawn_ui(ui_listener, ui_tx);
    let capture = Capture::bind(
        &session,
        match viewer_api {
            ViewerApi::D3d12 => ConsumerPreference::D3d12,
            ViewerApi::Vulkan => ConsumerPreference::Vulkan,
        },
    )?;

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    #[rustfmt::skip]
    let mut application = ViewerApplication::new(move |target| {
        let window = target.create_window(
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
        let (mut compositor, mut video_compositor) =
            ViewerCompositor::new(&window, viewer_api, publish_video)?;
        let frequency = performance_frequency().unwrap_or(1) as f64;
        let mut stats = Statistics::default();
        let mut pointer_routing = PointerRoutingState::new(Instant::now());
        let mut active_capture_connections = HashMap::<u64, (u64, u8)>::new();
        let mut stream_indices = HashMap::<u64, u32>::new();
        let mut skipped_surface_windows = HashSet::<(u64, u8)>::new();
        let mut skipped_surface_backends = HashSet::<u8>::new();
        let mut active_backend = "Waiting";
        let mut active_consumer = "Waiting";
        let mut direct_input = DirectInputForwarder::new(input_tx.clone());
        let mut pressed_keys = HashSet::<KeyCode>::new();
        let mut pending_ui = VecDeque::<UiRequest>::new();
        let mut last_resize_windows = Vec::new();

        Ok(Box::new(move |event, target| match event {
        Event::AboutToWait => {
            if resize_guest && launcher.isolate {
                let resize_windows = compositor.resize_windows(&window);
                if resize_windows != last_resize_windows {
                    for resize in &resize_windows {
                        let _ = input_tx.send(snowglass_ipc::Message::ResizeWindow(*resize));
                    }
                    last_resize_windows = resize_windows;
                }
            }
            window.request_redraw();
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
                            let presentation =
                                compositor.presentation(Extent::new(size.width, size.height));
                            let plan = layout::composition_plan(&presentation);
                            cancel_pointer_connection(
                                connection,
                                &mut pointer_routing,
                                &presentation,
                                &plan,
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
                    let presentation =
                        compositor.presentation(Extent::new(size.width, size.height));
                    let plan = layout::composition_plan(&presentation);
                    if pointer_routing.send(
                        PointerPhase::Move,
                        0,
                        0,
                        0,
                        &presentation,
                        &plan,
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
                        let presentation =
                            compositor.presentation(Extent::new(size.width, size.height));
                        let plan = layout::composition_plan(&presentation);
                        if pointer_routing.send(
                            phase,
                            u32::from(button),
                            0,
                            0,
                            &presentation,
                            &plan,
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
                    let presentation =
                        compositor.presentation(Extent::new(size.width, size.height));
                    let plan = layout::composition_plan(&presentation);
                    if pointer_routing.send(
                        PointerPhase::Wheel,
                        0,
                        horizontal,
                        vertical,
                        &presentation,
                        &plan,
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
                        for connection in active_capture_connections
                            .keys()
                            .copied()
                            .collect::<Vec<_>>()
                        {
                            let _ = compositor
                                .disconnect_stream(connection, &mut video_compositor);
                        }
                        active_capture_connections.clear();
                        stream_indices.clear();
                        active_backend = "Waiting";
                        active_consumer = "Waiting";
                    }
                    while let Ok(event) = capture.try_recv() {
                        match event {
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
                                        .then(|| {
                                            matching_surface_connection(
                                                &active_capture_connections,
                                                stream.surface_id.0,
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
                                        let size = window.inner_size();
                                        let presentation = compositor.presentation(Extent::new(
                                            size.width,
                                            size.height,
                                        ));
                                        let plan = layout::composition_plan(&presentation);
                                        cancel_pointer_connection(
                                            existing,
                                            &mut pointer_routing,
                                            &presentation,
                                            &plan,
                                            &input_tx,
                                            &mut direct_input,
                                        );
                                        active_capture_connections.remove(&existing);
                                        stream_indices.remove(&existing);
                                        if let Some(publisher) = &webrtc {
                                            publisher.remove_stream(existing);
                                        }
                                        capture.revoke(existing);
                                        if let Err(error) =
                                            compositor.disconnect_stream(
                                                existing,
                                                &mut video_compositor,
                                            )
                                        {
                                            eprintln!(
                                                "snowglass: failed to replace prior stream for window: {error}"
                                            );
                                        }
                                    }
                                }
                                if !active_capture_connections.contains_key(&connection)
                                    && active_capture_connections.len()
                                        >= surface_limit as usize
                                    && !skipped_surface_backends.contains(&surface_identity.1)
                                {
                                    let provisional = active_capture_connections
                                        .iter()
                                        .find_map(|(&existing, &(_, backend))| {
                                            skipped_surface_backends
                                                .contains(&backend)
                                                .then_some(existing)
                                    });
                                    if let Some(existing) = provisional {
                                        let size = window.inner_size();
                                        let presentation = compositor.presentation(Extent::new(
                                            size.width,
                                            size.height,
                                        ));
                                        let plan = layout::composition_plan(&presentation);
                                        cancel_pointer_connection(
                                            existing,
                                            &mut pointer_routing,
                                            &presentation,
                                            &plan,
                                            &input_tx,
                                            &mut direct_input,
                                        );
                                        active_capture_connections.remove(&existing);
                                        stream_indices.remove(&existing);
                                        if let Some(publisher) = &webrtc {
                                            publisher.remove_stream(existing);
                                        }
                                        capture.revoke(existing);
                                        if let Err(error) =
                                            compositor.disconnect_stream(
                                                existing,
                                                &mut video_compositor,
                                            )
                                        {
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
                                    && active_capture_connections.len()
                                        >= surface_limit as usize
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
                                        let presentation = compositor.presentation(Extent::new(
                                            size.width,
                                            size.height,
                                        ));
                                        let plan = layout::composition_plan(&presentation);
                                        cancel_pointer_connection(
                                            existing,
                                            &mut pointer_routing,
                                            &presentation,
                                            &plan,
                                            &input_tx,
                                            &mut direct_input,
                                        );
                                        active_capture_connections.remove(&existing);
                                        stream_indices.remove(&existing);
                                        if let Some(publisher) = &webrtc {
                                            publisher.remove_stream(existing);
                                        }
                                        capture.revoke(existing);
                                        if let Err(error) =
                                            compositor.disconnect_stream(
                                                existing,
                                                &mut video_compositor,
                                            )
                                        {
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
                                    && active_capture_connections.len()
                                        >= surface_limit as usize
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
                                        (0..surface_limit).find(|index| {
                                            !stream_indices.values().any(|v| v == index)
                                        })
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
                                    publish_stream,
                                    quarter_turns,
                                    &mut video_compositor,
                                ) {
                                    Ok(()) => {
                                        stream_indices.insert(connection, stream_index);
                                        if publish_stream && let Some(publisher) = &webrtc {
                                            publisher.add_stream(connection, stream_index);
                                        }
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
                                            "snowglass: imported {active_backend} stream {}x{} (surface {:#x}) for window {:#x} with {} slots using {}",
                                            stream.width,
                                            stream.height,
                                            stream.surface_id.0,
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
                                let observation = stats.sequences.observe(
                                    connection,
                                    frame.frame_number,
                                    frame.timestamp_qpc,
                                );
                                let frame_interval_ms = observation
                                    .interval_qpc
                                    .map(|interval_qpc| interval_qpc * 1000.0 / frequency);
                                stats.dropped = stats.sequences.dropped();
                                let target_index =
                                    stream_indices.get(&connection).copied().unwrap_or(0);
                                let latency_ms = performance_counter()
                                    .unwrap_or(frame.timestamp_qpc)
                                    .saturating_sub(frame.timestamp_qpc)
                                    as f64
                                    * 1000.0
                                    / frequency;
                                stats.target_statistics_tracker.observe(
                                    connection,
                                    target_index,
                                    latency_ms,
                                    frame_interval_ms,
                                );
                                stats.frame_number = frame.frame_number;
                                stats.slot = frame.slot;
                                stats.render_width = frame.render_width;
                                stats.render_height = frame.render_height;
                                stats.capture_x = frame.capture_x;
                                stats.capture_y = frame.capture_y;
                                stats.capture_width = frame.capture_width;
                                stats.capture_height = frame.capture_height;
                                compositor.set_latest(connection, frame);
                                if active_backend == "D3D11" {
                                    // Keyed-mutex frames must be consumed and released in order;
                                    // do not overwrite one with a newer socket message.
                                    break;
                                }
                            }
                            CaptureEvent::Disconnected(connection) => {
                                stats.sequences.remove(connection);
                                stats.target_statistics_tracker.remove(connection);
                                stats.target_statistics =
                                    stats.target_statistics_tracker.statistics();
                                if let Some(publisher) = &webrtc {
                                    publisher.remove_stream(connection);
                                }
                                let size = window.inner_size();
                                let presentation =
                                    compositor.presentation(Extent::new(size.width, size.height));
                                let plan = layout::composition_plan(&presentation);
                                cancel_pointer_connection(
                                    connection,
                                    &mut pointer_routing,
                                    &presentation,
                                    &plan,
                                    &input_tx,
                                    &mut direct_input,
                                );
                                stream_indices.remove(&connection);
                                capture.forget(connection);
                                if active_capture_connections.remove(&connection).is_none() {
                                    continue;
                                }
                                if let Err(error) = compositor
                                    .disconnect_stream(connection, &mut video_compositor)
                                {
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
                    if stats.target_statistics_tracker.sample() {
                        stats.target_statistics =
                            stats.target_statistics_tracker.statistics();
                    }
                    let input = egui_state.take_egui_input(&window);
                    let webrtc_stats = webrtc.as_ref().map(webrtc::Publisher::stats);
                    let last_pointer_sent = pointer_routing.last_sent();
                    let mut launch_requested = false;
                    let mut close_requested = false;
                    let output = egui_context.run(input, |context| {
                        egui::TopBottomPanel::top("statistics").show(context, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.strong(format!(
                                    "Snowglass {active_backend} via {active_consumer}"
                                ));
                                ui.toggle_value(&mut show_layout_css, "Layout CSS");
                                if active_target.is_some()
                                    && ui.button("Close program").clicked()
                                {
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
                                if stats.target_statistics.is_empty() {
                                    ui.label("Target statistics waiting");
                                } else {
                                    for target in &stats.target_statistics {
                                        ui.label(format!(
                                            "Target {}: {:.1} FPS · latency {:.2} ms · frame Δ {:.2} ms",
                                            target.target,
                                            target.fps,
                                            target.latency_ms,
                                            target.frame_interval_ms,
                                        ));
                                    }
                                }
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
                                    .on_hover_text(
                                        format!(
                                            "Surface {} in {}x{} presentation coordinates",
                                            sent.surface_id,
                                            sent.coordinate_width,
                                            sent.coordinate_height
                                        ),
                                    );
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
                            let dialog_position =
                                context.screen_rect().center() - egui::vec2(360.0, 120.0);
                            let mut selected = None;
                            if context.input(|input| input.key_pressed(egui::Key::Escape)) {
                                selected =
                                    request.escape_result.or(request.default_result).or_else(
                                        || request.buttons.first().map(|button| button.result),
                                    );
                            }
                            egui::Window::new(title)
                                .id(egui::Id::new("snowglass-forwarded-message-box"))
                                .order(egui::Order::Foreground)
                                .fixed_pos(dialog_position)
                                .collapsible(false)
                                .resizable(true)
                                .show(context, |ui| {
                                    ui.set_max_width(720.0);
                                    ui.label(&request.text);
                                    ui.add_space(12.0);
                                    ui.horizontal(|ui| {
                                        let dismiss = ui.button("Dismiss");
                                        dismiss.request_focus();
                                        if dismiss.clicked() {
                                            selected = request
                                                .escape_result
                                                .or(request.default_result)
                                                .or_else(|| {
                                                    request
                                                        .buttons
                                                        .first()
                                                        .map(|button| button.result)
                                                });
                                        }
                                        if ui.button("Close program").clicked() {
                                            close_requested = true;
                                        }
                                    });
                                });
                            if close_requested {
                                pending_ui.pop_front();
                            } else if let Some(result) = selected
                                && let Some(forwarded) = pending_ui.pop_front()
                            {
                                let _ = forwarded
                                    .response
                                    .send(snowglass_ipc::ui::MessageBoxResponse { result });
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
                        egui::Window::new("Frame Graphs · rolling 120 seconds")
                            .open(&mut show_frame_graph)
                            .default_pos(egui::pos2(12.0, 48.0))
                            .resizable(false)
                            .collapsible(true)
                            .frame(statistics_frame)
                            .show(context, |ui| {
                                if stats.target_statistics.is_empty() {
                                    ui.label("Collecting per-target frame statistics…");
                                }
                                for target in &stats.target_statistics {
                                    ui.strong(format!("Target {}", target.target));
                                    draw_statistics_graph(ui, &target.history);
                                }
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
                                            ui.monospace(
                                                webrtc_stats.render_targets.to_string(),
                                            );
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
                    if close_requested && let Some(active) = &active_target {
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
                            launcher.render_target_surface_patterns(),
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
                                stats = Statistics::default();
                            }
                            Err(error) => launch_error = Some(error.to_string()),
                        }
                    }
                    egui_state.handle_platform_output(&window, output.platform_output.clone());
                    let primitives =
                        egui_context.tessellate(output.shapes.clone(), output.pixels_per_point);
                    let size = window.inner_size();
                    let presentation =
                        compositor.presentation(Extent::new(size.width, size.height));
                    let composition_plan = layout::composition_plan(&presentation);
                    match compositor.draw(
                        &output,
                        &primitives,
                        &composition_plan,
                        video_compositor.as_mut(),
                    ) {
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
                    if let (Some(publisher), Some(video_compositor)) =
                        (&webrtc, &mut video_compositor)
                    {
                        for frame in video_compositor.take_frames() {
                            publisher.publish(frame);
                        }
                    }
                }
                _ => {}
            }
        }
        _ => {}
        }))
    });
    event_loop.run_app(&mut application)?;
    application.finish()?;
    Ok(())
}

fn spawn_target_monitor(
    mut target_process: TargetProcess,
    stop: Arc<AtomicBool>,
    done: mpsc::Sender<()>,
) {
    thread::spawn(move || {
        loop {
            if stop.load(Ordering::Acquire) {
                break;
            }
            match target_process.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => thread::sleep(Duration::from_millis(8)),
            }
        }
        let _ = target_process.terminate_tree_and_wait();
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
    tracked_surface_patterns: Vec<Option<String>>,
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
        tracked_surface_patterns,
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

fn create_vulkan_layer_manifest(layer_dll: &Path) -> std::io::Result<tempfile::TempDir> {
    if !layer_dll.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Vulkan hook is missing at {}", layer_dll.display()),
        ));
    }
    let directory = tempfile::Builder::new()
        .prefix("snowglass-vulkan-layer-")
        .tempdir()?;
    let manifest = serde_json::json!({
        "file_format_version": "1.2.0",
        "layer": {
            "name": "VK_LAYER_SNOWGLASS_capture",
            "type": "GLOBAL",
            "library_path": layer_dll,
            "api_version": "1.4.0",
            "implementation_version": "1",
            "description": "Snowglass Vulkan capture layer",
            "functions": {
                "vkNegotiateLoaderLayerInterfaceVersion":
                    "vkNegotiateLoaderLayerInterfaceVersion"
            },
            "enable_environment": {
                "SNOWGLASS_VULKAN_LAYER": "1"
            },
            "disable_environment": {
                "DISABLE_SNOWGLASS_VULKAN_LAYER": "1"
            }
        }
    });
    let file = std::fs::File::create(directory.path().join("snowglass-layer.json"))?;
    serde_json::to_writer_pretty(file, &manifest).map_err(std::io::Error::other)?;
    Ok(directory)
}

#[expect(clippy::too_many_arguments)]
fn launch_target(
    target: &[String],
    layer_dir: &Path,
    session: &str,
    track_render_targets: Option<Vec<u32>>,
    tracked_surface_patterns: Vec<Option<String>>,
    graph_tracking: bool,
    force_keyed_mutex: bool,
    hooks: &[HookBackend],
    isolate: bool,
    stone_layouts: &[String],
    stone_database: Option<&Path>,
) -> std::io::Result<TargetProcess> {
    let target_architecture = executable_architecture(Path::new(&target[0])).ok();
    let artifact_dir = target_architecture
        .map(|architecture| architecture_artifact_dir(layer_dir, architecture))
        .unwrap_or_else(|| layer_dir.to_path_buf());
    let sdl2_runtime = artifact_dir.join("SDL2.dll");
    let sdl3_runtime = artifact_dir.join("SDL3.dll");
    let vulkan_manifest = hooks
        .contains(&HookBackend::Vulkan)
        .then(|| {
            create_vulkan_layer_manifest(&artifact_dir.join("snowglass_hook_vulkan_layer.dll"))
        })
        .transpose()?;
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
    // SDL's dynamic API redirects calls from statically linked applications
    // (including Cemu and Dolphin) through these DLLs, making their public
    // entry points available to the hooks. Preserve explicit caller overrides.
    if std::env::var_os("SDL_DYNAMIC_API").is_none() && sdl2_runtime.is_file() {
        environment.push((
            OsString::from("SDL_DYNAMIC_API"),
            sdl2_runtime.into_os_string(),
        ));
    }
    if std::env::var_os("SDL3_DYNAMIC_API").is_none() && sdl3_runtime.is_file() {
        environment.push((
            OsString::from("SDL3_DYNAMIC_API"),
            sdl3_runtime.into_os_string(),
        ));
    }
    // The Vulkan producer installs as an instance layer rather than an
    // injected DLL, so its selection travels through the loader environment.
    if let Some(manifest) = &vulkan_manifest {
        environment.push((
            OsString::from("VK_LAYER_PATH"),
            manifest.path().as_os_str().to_owned(),
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
        if tracked_surface_patterns.iter().any(Option::is_some) {
            environment.push((
                OsString::from(config::TRACK_RENDER_TARGET_SURFACE_PATTERNS),
                OsString::from(
                    serde_json::to_string(&tracked_surface_patterns)
                        .map_err(std::io::Error::other)?,
                ),
            ));
        }
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
    let libraries = hook_libraries(hooks, isolate);
    let mut process = TargetProcess::launch(
        target,
        layer_dir,
        &environment,
        isolate.then_some(session),
        &libraries,
    )?;
    if let Some(manifest) = vulkan_manifest {
        // The Vulkan loader may consult the manifest more than once during the
        // process lifetime, so retain it with the shared target lifecycle.
        process.retain(manifest);
    }
    Ok(process)
}

fn hook_libraries(hooks: &[HookBackend], isolate: bool) -> Vec<InjectionLibrary> {
    let mut libraries = Vec::new();
    if hooks
        .iter()
        .any(|hook| matches!(hook, HookBackend::D3d11 | HookBackend::D3d12))
    {
        libraries.push(InjectionLibrary::new(
            "DXGI",
            "snowglass_hook_dxgi.dll",
            c"SnowglassInstallDxgiHook",
        ));
    }
    if hooks.contains(&HookBackend::D3d11) {
        libraries.push(InjectionLibrary::new(
            "D3D11",
            "snowglass_hook_d3d11.dll",
            c"SnowglassInstallD3d11Hook",
        ));
    }
    if hooks.contains(&HookBackend::D3d12) {
        libraries.push(InjectionLibrary::new(
            "D3D12",
            "snowglass_hook_d3d12.dll",
            c"SnowglassInstallD3d12Hook",
        ));
    }
    if hooks.contains(&HookBackend::OpenGl) {
        libraries.push(InjectionLibrary::new(
            "OpenGL",
            "snowglass_hook_opengl32.dll",
            c"SnowglassInstallOpenGlHook",
        ));
    }
    if hooks.contains(&HookBackend::Gdi) {
        libraries.push(InjectionLibrary::new(
            "GDI",
            "snowglass_hook_gdi.dll",
            c"SnowglassInstallGdiHook",
        ));
    }
    libraries.push(InjectionLibrary::new(
        "DirectInput",
        "snowglass_hook_dinput.dll",
        c"SnowglassInstallDinputHook",
    ));
    libraries.push(InjectionLibrary::new(
        "Win32 input",
        "snowglass_hook_win32.dll",
        c"SnowglassInstallWin32Hook",
    ));
    if isolate {
        libraries.push(InjectionLibrary::new(
            "UI",
            "snowglass_hook_ui.dll",
            c"SnowglassInstallUiHook",
        ));
    }
    libraries.push(InjectionLibrary::new(
        "SDL2",
        "snowglass_hook_sdl2.dll",
        c"SnowglassInstallSdl2Hook",
    ));
    libraries.push(InjectionLibrary::new(
        "SDL3",
        "snowglass_hook_sdl3.dll",
        c"SnowglassInstallSdl3Hook",
    ));
    libraries
}
