use snowglass_input::{
    layout::{InputBinding, InputLayout, SdlAxis, SdlButton},
    mouse::{PointerPhase, layout::Extent},
    provider::sdl3::{
        DeviceSelector, ProviderConfig as Sdl3ProviderConfig, Sdl3Provider, SlotConfig,
    },
};
use snowglass_ipc::StreamReady;
use snowglass_platform_windows::{
    NativeWindow, forward_character, performance_counter, performance_frequency,
};
use snowglass_protocol::{ExternalImageKind, ExternalSyncKind, FrameReady};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    error::Error,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicIsize, Ordering},
    },
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
mod layout;

use gui::{LauncherState, draw_gamepad_window, draw_launcher_window, draw_layout_css_window};
use snowglass_egui::{D3d12EguiPainter, VulkanEguiPainter, draw_statistics_graph};
#[cfg(test)]
use snowglass_renderer::cli::{AUTO_HOOKS, parse_arguments};
use snowglass_renderer::{
    CompositingPlan, D3d12Compositor, Presentation, RenderWindow, Renderer, SurfaceCapture,
    SurfaceCaptureEvent, VulkanCompositor, WebRtcCompositor,
    cli::{
        GamepadArgument, HookBackend, TrackedRenderTarget, ViewerApi, ViewerOptions, parse_target,
        split_command_arguments,
    },
    input::PointerRoutingState,
    webrtc,
};
use snowglass_runtime::UiRequest;
use snowglass_statistics::{
    FrameMetrics, LiveStatistics, PresentationMetrics, RecorderHandle, StatisticsSnapshot,
};

use input::{
    dinput::{
        DirectInputForwarder, RawKeyboardForwarder, RoutedInputSender, cancel_pointer_connection,
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

struct Statistics {
    frame_number: u64,
    slot: u32,
    render_width: u32,
    render_height: u32,
    capture_x: u32,
    capture_y: u32,
    capture_width: u32,
    capture_height: u32,
    frames: HashMap<u64, FrameMetrics>,
    live: LiveStatistics,
    presentation: PresentationMetrics,
    snapshot: StatisticsSnapshot,
}

impl Statistics {
    fn new(recorder: RecorderHandle, backend: ViewerApi) -> Self {
        Self {
            frame_number: 0,
            slot: 0,
            render_width: 0,
            render_height: 0,
            capture_x: 0,
            capture_y: 0,
            capture_width: 0,
            capture_height: 0,
            frames: HashMap::new(),
            live: LiveStatistics::new(recorder),
            presentation: PresentationMetrics::new(match backend {
                ViewerApi::D3d12 => "d3d12",
                ViewerApi::Vulkan => "vulkan",
            }),
            snapshot: StatisticsSnapshot::default(),
        }
    }

    fn add_surface(&mut self, connection: u64, surface: u64, target: u32) {
        self.frames
            .insert(connection, FrameMetrics::new(connection, surface, target));
    }

    fn observe_frame(&mut self, connection: u64, frame: FrameReady, frequency: f64) {
        let latency_ticks = performance_counter()
            .unwrap_or(frame.timestamp_qpc)
            .saturating_sub(frame.timestamp_qpc);
        if let Some(metrics) = self.frames.get_mut(&connection) {
            metrics.observe(
                frame.frame_number,
                frame.timestamp_qpc,
                frequency,
                Duration::from_secs_f64(latency_ticks as f64 / frequency.max(1.0)),
            );
        }
        self.frame_number = frame.frame_number;
        self.slot = frame.slot;
        self.render_width = frame.render_width;
        self.render_height = frame.render_height;
        self.capture_x = frame.capture_x;
        self.capture_y = frame.capture_y;
        self.capture_width = frame.capture_width;
        self.capture_height = frame.capture_height;
    }

    fn remove_surface(&mut self, connection: u64) {
        self.frames.remove(&connection);
        self.live.remove_connection(connection);
    }

    fn sample(&mut self) {
        self.snapshot = self.live.sample().clone();
    }
}

struct VulkanViewer {
    // Dropped before the compositor destroys the Vulkan device.
    painter: VulkanEguiPainter,
    compositor: VulkanCompositor,
}

struct D3d12Viewer {
    painter: D3d12EguiPainter,
    compositor: D3d12Compositor,
}

enum ViewerCompositor {
    Vulkan(Box<VulkanViewer>),
    D3d12(Box<D3d12Viewer>),
    Empty,
}

fn d3d12_viewer(compositor: D3d12Compositor) -> Result<D3d12Viewer, Box<dyn Error>> {
    let painter = D3d12EguiPainter::new(
        compositor.d3d12_device().clone(),
        compositor.target_format(),
        compositor.frames_in_flight(),
    )?;
    Ok(D3d12Viewer {
        painter,
        compositor,
    })
}

fn vulkan_viewer(compositor: VulkanCompositor) -> Result<VulkanViewer, Box<dyn Error>> {
    let painter = VulkanEguiPainter::new(&compositor)?;
    Ok(VulkanViewer {
        painter,
        compositor,
    })
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

impl ViewerCompositor {
    fn new(
        window: &Window,
        api: ViewerApi,
        publish_video: bool,
    ) -> Result<(Self, Option<WebRtcCompositor>), Box<dyn Error>> {
        match api {
            ViewerApi::D3d12 if publish_video => {
                let (compositor, webrtc) = D3d12Compositor::new_default_with_webrtc(window)?;
                Ok((
                    Self::D3d12(Box::new(d3d12_viewer(compositor)?)),
                    Some(webrtc),
                ))
            }
            ViewerApi::D3d12 => Ok((
                Self::D3d12(Box::new(d3d12_viewer(D3d12Compositor::new_default(
                    window,
                )?)?)),
                None,
            )),
            ViewerApi::Vulkan if publish_video => {
                let (compositor, webrtc) = VulkanCompositor::new_with_webrtc(window, None, true)?;
                Ok((
                    Self::Vulkan(Box::new(vulkan_viewer(compositor)?)),
                    Some(webrtc),
                ))
            }
            ViewerApi::Vulkan => Ok((
                Self::Vulkan(Box::new(vulkan_viewer(VulkanCompositor::new(
                    window, None, true,
                )?)?)),
                None,
            )),
        }
    }

    fn import_surface_offer(
        &mut self,
        window: &Window,
        offer: &snowglass_runtime::SurfaceOffer,
        api: ViewerApi,
        publish_video: bool,
        publish_stream: bool,
        quarter_turns: u8,
        video: &mut Option<WebRtcCompositor>,
    ) -> Result<(), Box<dyn Error>> {
        let connection = offer.connection_id();
        let stream = offer.stream();
        if api == ViewerApi::D3d12 && !d3d12_consumable(stream) {
            return Err(format!(
                "the selected D3D12 viewer cannot consume {:?} output; enable a hook that exports D3D12-compatible resources",
                stream.image_kind
            )
            .into());
        }
        if api == ViewerApi::D3d12 && d3d12_consumable(stream) {
            let matches = matches!(
                self,
                Self::D3d12(value)
                    if value.compositor.adapter_luid() == stream.adapter_luid
            );
            if !matches {
                *video = None;
                drop(std::mem::replace(self, Self::Empty));
                if publish_video {
                    let (compositor, webrtc) =
                        D3d12Compositor::new_with_webrtc(window, stream.adapter_luid)?;
                    *self = Self::D3d12(Box::new(d3d12_viewer(compositor)?));
                    *video = Some(webrtc);
                } else {
                    *self = Self::D3d12(Box::new(d3d12_viewer(D3d12Compositor::new(
                        window,
                        stream.adapter_luid,
                    )?)?));
                    *video = None;
                }
            }
            let Self::D3d12(value) = self else {
                unreachable!()
            };
            Renderer::import_surface_offer(&mut value.compositor, offer, quarter_turns)?;
            if publish_stream && let Some(video) = video {
                video.import_d3d12_stream(connection)?;
            }
            return Ok(());
        }

        let needs_keyed_mutex = stream.image_kind == ExternalImageKind::D3d11SharedResourceNt;
        let adapter = stream.adapter_luid;
        let matches = matches!(self, Self::Vulkan(value)
            if (!needs_keyed_mutex || value.compositor.keyed_mutex_enabled())
                && ((adapter.low_part == 0 && adapter.high_part == 0)
                    || value.compositor.matches_adapter(adapter)));
        if !matches {
            *video = None;
            drop(std::mem::replace(self, Self::Empty));
            let preferred = (adapter.low_part != 0 || adapter.high_part != 0).then_some(adapter);
            if publish_video {
                let (compositor, webrtc) =
                    VulkanCompositor::new_with_webrtc(window, preferred, needs_keyed_mutex)?;
                *self = Self::Vulkan(Box::new(vulkan_viewer(compositor)?));
                *video = Some(webrtc);
            } else {
                *self = Self::Vulkan(Box::new(vulkan_viewer(VulkanCompositor::new(
                    window,
                    preferred,
                    needs_keyed_mutex,
                )?)?));
                *video = None;
            }
        }
        let Self::Vulkan(value) = self else {
            unreachable!()
        };
        Renderer::import_surface_offer(&mut value.compositor, offer, quarter_turns)?;
        if publish_stream && let Some(video) = video {
            video.import_vulkan_stream(&value.compositor, connection)?;
        }
        Ok(())
    }

    fn recreate_swapchain(&mut self, window: &Window) -> Result<bool, Box<dyn Error>> {
        match self {
            Self::Vulkan(value) => Renderer::recreate_swapchain(&mut value.compositor, window),
            Self::D3d12(value) => Renderer::recreate_swapchain(&mut value.compositor, window),
            Self::Empty => Ok(false),
        }
    }

    fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        match self {
            Self::Vulkan(value) => Renderer::set_latest(&mut value.compositor, connection, frame),
            Self::D3d12(value) => Renderer::set_latest(&mut value.compositor, connection, frame),
            Self::Empty => {}
        }
    }

    fn presentation(&self, viewer_extent: Extent) -> Presentation {
        match self {
            Self::Vulkan(value) => {
                Renderer::received_presentation(&value.compositor, viewer_extent)
            }
            Self::D3d12(value) => Renderer::received_presentation(&value.compositor, viewer_extent),
            Self::Empty => Presentation::new(viewer_extent, Vec::new()),
        }
    }

    fn resize_windows(&self, window: &impl RenderWindow) -> Vec<snowglass_protocol::ResizeWindow> {
        let presentation = self.presentation(window.render_extent());
        let plan = layout::composition_plan(&presentation);
        match self {
            Self::Vulkan(value) => Renderer::resize_windows(&value.compositor, window, &plan),
            Self::D3d12(value) => Renderer::resize_windows(&value.compositor, window, &plan),
            Self::Empty => Vec::new(),
        }
    }

    fn has_pending_frame(&self) -> bool {
        match self {
            Self::Vulkan(value) => Renderer::has_pending_frame(&value.compositor),
            Self::D3d12(value) => Renderer::has_pending_frame(&value.compositor),
            Self::Empty => false,
        }
    }

    fn begin_frame(&mut self) {
        match self {
            Self::Vulkan(value) => Renderer::begin_frame(&mut value.compositor),
            Self::D3d12(value) => Renderer::begin_frame(&mut value.compositor),
            Self::Empty => {}
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
            Self::Vulkan(value) => Renderer::disconnect_stream(&mut value.compositor, connection),
            Self::D3d12(value) => Renderer::disconnect_stream(&mut value.compositor, connection),
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
            Self::Vulkan(value) => {
                let VulkanViewer {
                    painter,
                    compositor,
                } = value.as_mut();
                painter.prepare(compositor, output)?;
                compositor.present_with(plan, video, |pass| painter.paint(pass, output, primitives))
            }
            Self::D3d12(value) => {
                let D3d12Viewer {
                    painter,
                    compositor,
                } = value.as_mut();
                compositor.present_with(plan, video, |pass| painter.paint(pass, output, primitives))
            }
            Self::Empty => Ok(false),
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let default_filter = if std::env::var_os("SNOWGLASS_WEBRTC_TRACE").is_some() {
        "info,webrtc=trace"
    } else {
        "info"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    let statistics_recorder = snowglass_statistics::install()?;
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
    let mut capture = SurfaceCapture::default();
    let pointer_tx = capture.pointer_sender();
    let input_tx = RoutedInputSender::default();
    let mut gamepad_provider = None;
    let mut gamepad_layouts = Vec::new();
    let mut gamepad_remap_drafts = Vec::new();
    let mut active_target = None;
    let mut launch_error = None::<String>;
    if start_immediately {
        match start_target_session(
            &launcher,
            &executable_dir,
            &session,
            surface_limit,
            options.skip_surfaces,
            viewer_api,
            launcher.tracked_render_target_indices(),
            launcher.render_target_surface_patterns(),
            launcher.graph_tracking,
            options.force_keyed_mutex,
            options.gamecontroller_db.clone(),
        ) {
            Ok(started) => {
                gamepad_layouts = started.layouts;
                gamepad_remap_drafts = vec![HashMap::new(); gamepad_layouts.len()];
                gamepad_provider = Some(started.provider);
                input_tx.attach(&started.target.instance);
                capture.attach(&started.target.instance);
                active_target = Some(started.target);
            }
            Err(error) => launch_error = Some(error.to_string()),
        }
    }
    let backing_window = Arc::new(AtomicIsize::new(0));
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
        let mut stats = Statistics::new(statistics_recorder, viewer_api);
        let mut pointer_routing = PointerRoutingState::new(Instant::now());
        let mut stream_indices = HashMap::<u64, u32>::new();
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
            // Spend the wait for the next present slot here rather than inside
            // `Present` at the end of the redraw. Blocking afterwards
            // composites guest frames and input that were already a full
            // refresh interval old by the time they reached the screen;
            // blocking first means the redraw below starts the moment the slot
            // opens and draws from whatever is newest at that instant.
            //
            // This sits ahead of `request_redraw` deliberately. The redraw
            // arrives as WM_PAINT, which Windows delivers only once nothing
            // else is queued, so every input message that landed during the
            // wait is dispatched before the frame that should reflect it.
            let size = window.inner_size();
            if size.width != 0 && size.height != 0 {
                compositor.begin_frame();
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
                    if let Some(mut active) = active_target.take() {
                        let _ = active.instance.close();
                    }
                    gamepad_provider = None;
                    input_tx.detach();
                    target.exit();
                }
                WindowEvent::Resized(size) => {
                    if size.width != 0
                        && size.height != 0
                        && let Err(error) = compositor.recreate_swapchain(&window)
                    {
                        tracing::error!("snowglass: swapchain recreation failed: {error}");
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
                                &pointer_tx,
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
                        &pointer_tx,
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
                            &pointer_tx,
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
                        &pointer_tx,
                    ) {
                        direct_input.mouse_wheel(vertical);
                    }
                }
                WindowEvent::RedrawRequested => {
                    if let Some(active) = &active_target {
                        while let Ok(request) = active.ui.try_recv() {
                            pending_ui.push_back(request);
                        }
                    }
                    let size = window.inner_size();
                    if size.width == 0 || size.height == 0 {
                        return;
                    }
                    if active_target
                        .as_ref()
                        .is_some_and(|active| {
                            matches!(
                                active.instance.state(),
                                snowglass_runtime::InstanceState::Exited(_)
                            )
                        })
                    {
                        active_target = None;
                        gamepad_provider = None;
                        backing_window.store(0, Ordering::Release);
                        for connection in stream_indices
                            .keys()
                            .copied()
                            .collect::<Vec<_>>()
                        {
                            stats.remove_surface(connection);
                            let _ = compositor
                                .disconnect_stream(connection, &mut video_compositor);
                        }
                        stream_indices.clear();
                        active_backend = "Waiting";
                        active_consumer = "Waiting";
                        capture.detach();
                        input_tx.detach();
                    }
                    while let Ok(event) = capture.try_recv() {
                        match event {
                            SurfaceCaptureEvent::Offered(offer) => {
                                let connection = offer.connection_id();
                                let replaced = offer.replaced_connection_id();
                                let stream = *offer.stream();
                                let stream_index = offer.id().0;
                                if stream_index >= surface_limit {
                                    tracing::warn!(
                                        "snowglass: ignored {:?} surface window {:#x}; tracked surface {} is outside the viewer limit {}",
                                        stream.image_kind,
                                        stream.source_window,
                                        stream_index,
                                        surface_limit
                                    );
                                    capture.reject(offer);
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
                                let publish_stream = webrtc_targets
                                    .as_ref()
                                    .is_some_and(|targets| targets.contains(&stream_index));
                                let quarter_turns = render_target_rotations
                                    .get(stream_index as usize)
                                    .copied()
                                    .unwrap_or(0);
                                match compositor.import_surface_offer(
                                    &window,
                                    &offer,
                                    viewer_api,
                                    publish_video,
                                    publish_stream,
                                    quarter_turns,
                                    &mut video_compositor,
                                ) {
                                    Ok(()) => {
                                        let accepted = match capture.accept(offer) {
                                            Ok(accepted) => accepted,
                                            Err(error) => {
                                                let _ = compositor.disconnect_stream(
                                                    connection,
                                                    &mut video_compositor,
                                                );
                                                tracing::error!(
                                                    "snowglass: capture stream {connection} disappeared while it was being imported: {error}"
                                                );
                                                continue;
                                            }
                                        };
                                        if let Some(existing) =
                                            accepted.replaced_connection.or(replaced)
                                        {
                                            let size = window.inner_size();
                                            let presentation = compositor.presentation(
                                                Extent::new(size.width, size.height),
                                            );
                                            let plan =
                                                layout::composition_plan(&presentation);
                                            cancel_pointer_connection(
                                                existing,
                                                &mut pointer_routing,
                                                &presentation,
                                                &plan,
                                                &pointer_tx,
                                                &mut direct_input,
                                            );
                                            stream_indices.remove(&existing);
                                            stats.remove_surface(existing);
                                            if let Some(publisher) = &webrtc {
                                                publisher.remove_stream(existing);
                                            }
                                            capture.revoke(existing);
                                            if let Err(error) = compositor.disconnect_stream(
                                                existing,
                                                &mut video_compositor,
                                            ) {
                                                tracing::error!(
                                                    "snowglass: failed to replace prior stream: {error}"
                                                );
                                            }
                                        }
                                        stream_indices.insert(connection, stream_index);
                                        stats.add_surface(
                                            connection,
                                            stream.surface_id.0,
                                            stream_index,
                                        );
                                        if publish_stream && let Some(publisher) = &webrtc {
                                            publisher.add_stream(connection, stream_index);
                                        }
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
                                        tracing::info!(
                                            "snowglass: imported {active_backend} stream {}x{} (surface {:#x}) for window {:#x} with {} slots using {}",
                                            stream.width,
                                            stream.height,
                                            stream.surface_id.0,
                                            stream.source_window,
                                            stream.slot_count,
                                            active_consumer,
                                        );
                                    }
                                    Err(error) => {
                                        tracing::error!("snowglass: direct import failed: {error}");
                                        launch_error = Some(format!(
                                            "The selected hooks did not publish output compatible with the fixed {viewer_api:?} viewer: {error}"
                                        ));
                                        capture.reject(offer);
                                    }
                                }
                            }
                            SurfaceCaptureEvent::Frame { connection, update } => {
                                let frame = update.frame;
                                if !stream_indices.contains_key(&connection) {
                                    continue;
                                }
                                if frame.frame_number == 1 {
                                    tracing::debug!(
                                        "snowglass: received first {active_backend} frame on connection {connection}"
                                    );
                                }
                                stats.observe_frame(connection, frame, frequency);
                                compositor.set_latest(connection, frame);
                                if active_backend == "D3D11" {
                                    // Keyed-mutex frames must be consumed and released in order;
                                    // do not overwrite one with a newer socket message.
                                    break;
                                }
                            }
                            SurfaceCaptureEvent::Removed { connection, .. } => {
                                stats.remove_surface(connection);
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
                                    &pointer_tx,
                                    &mut direct_input,
                                );
                                if stream_indices.remove(&connection).is_none() {
                                    continue;
                                }
                                if let Err(error) = compositor
                                    .disconnect_stream(connection, &mut video_compositor)
                                {
                                    tracing::error!("snowglass: stream disconnect failed: {error}");
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
                    let has_pending_frame = compositor.has_pending_frame();
                    stats.sample();
                    let input = egui_state.take_egui_input(&window);
                    let webrtc_stats = webrtc.as_ref().map(webrtc::Publisher::stats);
                    let last_pointer_sent = pointer_routing.last_sent();
                    let mut launch_requested = false;
                    let mut close_requested = false;
                    let mut output = egui_context.run(input, |context| {
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
                                if stats.snapshot.targets.is_empty() {
                                    ui.label("Target statistics waiting");
                                } else {
                                    for target in &stats.snapshot.targets {
                                        ui.label(format!(
                                            "Target {}: {:.1} FPS · latency {:.2} ms · frame Δ {:.2} ms",
                                            target.target,
                                            target.fps,
                                            target.capture_latency.average,
                                            target.frame_interval.average,
                                        ));
                                    }
                                }
                                ui.label(format!("Dropped {}", stats.snapshot.dropped_frames));
                                ui.label(format!(
                                    "Composited {:.1}/s · repeated {:.1}/s",
                                    stats.snapshot.pacing.presented,
                                    stats.snapshot.pacing.repeated,
                                ))
                                .on_hover_text(
                                    "Composition passes per second, and how many of them \
                                     re-presented a frame the display had already shown. \
                                     A steady repeat rate means the guest and the display \
                                     are running at nominally the same rate with drifting \
                                     phase.",
                                );
                                if let Some(latency) =
                                    stats.snapshot.display_latency.values().next()
                                {
                                    ui.colored_label(
                                        egui::Color32::LIGHT_BLUE,
                                        format!(
                                            "To display {:.1} ms · worst {:.1} ms",
                                            latency.average, latency.maximum
                                        ),
                                    )
                                    .on_hover_text(format!(
                                        "Guest capture to the vblank that scanned the frame \
                                         out, measured on one clock: transport, the wait for \
                                         a present slot, and the scanout. Panel response is \
                                         not included. {} present(s) confirmed in the last \
                                         window.",
                                        latency.samples
                                    ));
                                }
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
                                let _ = forwarded.respond(
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
                        egui::Window::new("Frame Graphs · rolling 120 seconds")
                            .open(&mut show_frame_graph)
                            .default_pos(egui::pos2(12.0, 48.0))
                            .resizable(false)
                            .collapsible(true)
                            .frame(statistics_frame)
                            .show(context, |ui| {
                                if stats.snapshot.targets.is_empty() {
                                    ui.label("Collecting per-target frame statistics…");
                                }
                                for target in &stats.snapshot.targets {
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
                                &stats.snapshot.input.combined(),
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
                    if close_requested {
                        if let Some(mut active) = active_target.take() {
                            let _ = active.instance.close();
                        }
                        gamepad_provider = None;
                        capture.detach();
                        input_tx.detach();
                    }
                    if launch_requested && active_target.is_none() {
                        launch_error = None;
                        match start_target_session(
                            &launcher,
                            &executable_dir,
                            &session,
                            surface_limit,
                            options.skip_surfaces,
                            viewer_api,
                            launcher.tracked_render_target_indices(),
                            launcher.render_target_surface_patterns(),
                            launcher.graph_tracking,
                            options.force_keyed_mutex,
                            options.gamecontroller_db.clone(),
                        ) {
                            Ok(started) => {
                                render_target_rotations = launcher.render_target_rotations();
                                gamepad_layouts = started.layouts;
                                gamepad_remap_drafts =
                                    vec![HashMap::new(); gamepad_layouts.len()];
                                gamepad_provider = Some(started.provider);
                                input_tx.attach(&started.target.instance);
                                capture.attach(&started.target.instance);
                                active_target = Some(started.target);
                                for connection in
                                    stats.frames.keys().copied().collect::<Vec<_>>()
                                {
                                    stats.remove_surface(connection);
                                }
                            }
                            Err(error) => launch_error = Some(error.to_string()),
                        }
                    }
                    // Both are consumed here and never read again, so they move
                    // out rather than being copied out of the frame's output.
                    egui_state
                        .handle_platform_output(&window, std::mem::take(&mut output.platform_output));
                    let primitives = egui_context
                        .tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
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
                        Ok(true) => stats.presentation.record_present(has_pending_frame),
                        Ok(false) => {
                            if let Err(error) = compositor.recreate_swapchain(&window) {
                                tracing::error!("snowglass: swapchain recreation failed: {error}");
                                target.exit();
                            }
                        }
                        Err(error) => {
                            tracing::error!("snowglass: direct composition failed: {error}");
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

struct ActiveTarget {
    instance: snowglass_runtime::EmulatorInstance,
    ui: snowglass_runtime::UiRequestReceiver,
}

struct StartedTarget {
    target: ActiveTarget,
    provider: Sdl3Provider,
    layouts: Vec<InputLayout>,
}

#[expect(clippy::too_many_arguments)]
fn start_target_session(
    launcher: &LauncherState,
    executable_dir: &Path,
    session: &str,
    surface_limit: u32,
    skip_surfaces: u32,
    viewer_api: ViewerApi,
    tracked_render_targets: Option<Vec<u32>>,
    tracked_surface_patterns: Vec<Option<String>>,
    graph_tracking: bool,
    force_keyed_mutex: bool,
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
            InputLayout::get(&pad.layout).cloned().ok_or_else(|| {
                std::io::Error::other(format!("unknown input layout {}", pad.layout))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let configured_surfaces = tracked_render_targets
        .as_ref()
        .map(|indices| {
            indices
                .iter()
                .enumerate()
                .map(|(slot, &index)| snowglass_runtime::TrackedSurfaceConfig {
                    id: snowglass_runtime::TrackedSurfaceId(index),
                    render_target_index: index,
                    window_pattern: tracked_surface_patterns.get(slot).cloned().flatten(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            (0..surface_limit)
                .map(|index| snowglass_runtime::TrackedSurfaceConfig {
                    id: snowglass_runtime::TrackedSurfaceId(index),
                    render_target_index: index,
                    window_pattern: None,
                })
                .collect()
        });
    let mut instance =
        snowglass_runtime::EmulatorInstance::launch(snowglass_runtime::EmulatorLaunchConfig {
            session: session.to_owned(),
            command: target[0].clone(),
            arguments: target[1..].to_vec(),
            artifact_root: executable_dir.to_path_buf(),
            hooks: launcher.hooks.clone(),
            consumer: match viewer_api {
                ViewerApi::D3d12 => snowglass_runtime::ConsumerPreference::D3d12,
                ViewerApi::Vulkan => snowglass_runtime::ConsumerPreference::Vulkan,
            },
            isolate: launcher.isolate,
            tracked_surfaces: configured_surfaces,
            tracking_enabled: tracked_render_targets.is_some(),
            skip_surfaces,
            graph_tracking,
            force_keyed_mutex,
            gamepad_layouts: layouts.iter().map(|layout| layout.id.clone()).collect(),
            generated_configuration: None,
        })?;
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
            gamecontroller_database,
            keyboard_mapping_directory: None,
        },
        instance.gamepad_input(),
    );
    let provider = match provider {
        Ok(provider) => provider,
        Err(error) => {
            let _ = instance.close();
            return Err(
                std::io::Error::other(format!("SDL3 gamepad input failed: {error}")).into(),
            );
        }
    };
    let ui = instance.subscribe_ui_requests();
    Ok(StartedTarget {
        target: ActiveTarget { instance, ui },
        provider,
        layouts,
    })
}
