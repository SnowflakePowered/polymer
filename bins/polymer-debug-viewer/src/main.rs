use polymer_config::input::SourceSelector;
use polymer_controllers::layout::InputLayout;
use polymer_input::InputHub;
#[cfg(windows)]
use polymer_ipc::StreamReady;
#[cfg(windows)]
use polymer_platform_windows::{
    NativeWindow, forward_character, performance_counter, performance_frequency,
};
#[cfg(windows)]
use polymer_protocol::ExternalSyncKind;
use polymer_protocol::{ExternalImageKind, FrameReady, input::PointerPhase};
use polymer_renderer::Extent;
use std::{
    collections::{HashMap, HashSet},
    env,
    error::Error,
    path::Path,
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

mod filesystem;
mod gui;
mod input;
mod layout;

use gui::{LauncherState, draw_gamepad_window, draw_launcher_window, draw_layout_css_window};
#[cfg(target_os = "macos")]
use polymer_platform_macos::{performance_counter, performance_frequency};
use polymer_renderer::egui::{EguiPainter, RenderTargetExtent, draw_statistics_graph};
#[cfg(windows)]
use polymer_renderer_d3d12::D3d12EguiPainter;
#[cfg(target_os = "macos")]
use polymer_renderer_metal::MetalCompositor;
#[cfg(target_os = "macos")]
use polymer_renderer_metal::MetalEguiPainter;
#[cfg(windows)]
use polymer_renderer_vulkan::VulkanEguiPainter;
#[cfg(target_os = "macos")]
type WebRtcCompositor = std::convert::Infallible;
#[cfg(windows)]
use polymer_renderer::webrtc;
use polymer_renderer::{
    CompositingPlan, Presentation, RenderWindow, Renderer, SurfaceCapture, SurfaceCaptureEvent,
    cli::{
        GraphicsHook, TrackedRenderTarget, ViewerApi, ViewerOptions, parse_target,
        split_command_arguments,
    },
    input::PointerRoutingState,
};
#[cfg(windows)]
use polymer_renderer_d3d12::D3d12Compositor;
#[cfg(windows)]
use polymer_renderer_vulkan::VulkanCompositor;
#[cfg(windows)]
mod video;
use polymer_statistics::{
    FrameMetrics, LiveStatistics, PresentationMetrics, RecorderHandle, StatisticsSnapshot,
};
#[cfg(windows)]
use video::WebRtcCompositor;

use input::{
    dinput::{InputEndpoint, cancel_pointer_connection},
    load_window, native_window,
    pointer::mouse_button,
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
    render_targets: Vec<RenderTargetExtent>,
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
            render_targets: Vec::new(),
            frames: HashMap::new(),
            live: LiveStatistics::new(recorder),
            presentation: PresentationMetrics::new(match backend {
                ViewerApi::D3d12 => "d3d12",
                ViewerApi::Vulkan => "vulkan",
                ViewerApi::Metal => "metal",
            }),
            snapshot: StatisticsSnapshot::default(),
        }
    }

    fn add_surface(&mut self, connection: u64, surface: u64, target: u32, extent: Extent) {
        self.frames
            .insert(connection, FrameMetrics::new(connection, surface, target));
        self.render_targets
            .retain(|render_target| render_target.connection != connection);
        self.render_targets.push(RenderTargetExtent {
            connection,
            target,
            extent,
        });
        self.render_targets
            .sort_by_key(|render_target| render_target.target);
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
    }

    fn remove_surface(&mut self, connection: u64) {
        self.frames.remove(&connection);
        self.render_targets
            .retain(|render_target| render_target.connection != connection);
        self.live.remove_connection(connection);
    }

    fn sample(&mut self) {
        self.snapshot = self.live.sample().clone();
    }
}

#[cfg(windows)]
struct VulkanViewer {
    // Dropped before the compositor destroys the Vulkan device.
    painter: VulkanEguiPainter,
    compositor: VulkanCompositor,
}

#[cfg(windows)]
struct D3d12Viewer {
    painter: D3d12EguiPainter,
    compositor: D3d12Compositor,
}

#[cfg(target_os = "macos")]
struct MetalViewer {
    painter: MetalEguiPainter,
    compositor: MetalCompositor,
}

enum ViewerCompositor {
    #[cfg(windows)]
    Vulkan(Box<VulkanViewer>),
    #[cfg(windows)]
    D3d12(Box<D3d12Viewer>),
    #[cfg(target_os = "macos")]
    Metal(Box<MetalViewer>),
    Empty,
}

#[cfg(windows)]
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

#[cfg(windows)]
fn vulkan_viewer(compositor: VulkanCompositor) -> Result<VulkanViewer, Box<dyn Error>> {
    let painter = VulkanEguiPainter::new(&compositor)?;
    Ok(VulkanViewer {
        painter,
        compositor,
    })
}

/// Whether the native D3D12 consumer supports this shared-resource profile.
#[cfg(windows)]
fn d3d12_consumable(stream: &StreamReady) -> bool {
    matches!(
        stream.image_kind,
        ExternalImageKind::D3d12Resource | ExternalImageKind::D3d11Resource
    ) && stream.sync_kind == ExternalSyncKind::D3d12Fence
}

impl ViewerCompositor {
    fn new(
        window: &Window,
        api: ViewerApi,
        publish_video: bool,
    ) -> Result<(Self, Option<WebRtcCompositor>), Box<dyn Error>> {
        match api {
            #[cfg(target_os = "macos")]
            ViewerApi::Metal if !publish_video => {
                let compositor = MetalCompositor::new(window)?;
                Ok((
                    Self::Metal(Box::new(MetalViewer {
                        painter: MetalEguiPainter::default(),
                        compositor,
                    })),
                    None,
                ))
            }
            #[cfg(target_os = "macos")]
            _ => Err(
                "macOS requires --api metal; WebRTC capture is not implemented for Metal".into(),
            ),
            #[cfg(windows)]
            ViewerApi::Metal => Err("Metal is available only on macOS".into()),
            #[cfg(windows)]
            ViewerApi::D3d12 if publish_video => {
                let (compositor, webrtc) = D3d12Compositor::new_default_with_webrtc(window)?;
                Ok((
                    Self::D3d12(Box::new(d3d12_viewer(compositor)?)),
                    Some(WebRtcCompositor::D3d12(webrtc)),
                ))
            }
            #[cfg(windows)]
            ViewerApi::D3d12 => Ok((
                Self::D3d12(Box::new(d3d12_viewer(D3d12Compositor::new_default(
                    window,
                )?)?)),
                None,
            )),
            #[cfg(windows)]
            ViewerApi::Vulkan if publish_video => {
                let (compositor, webrtc) = VulkanCompositor::new_with_webrtc(window, None)?;
                Ok((
                    Self::Vulkan(Box::new(vulkan_viewer(compositor)?)),
                    Some(WebRtcCompositor::Vulkan(webrtc)),
                ))
            }
            #[cfg(windows)]
            ViewerApi::Vulkan => Ok((
                Self::Vulkan(Box::new(vulkan_viewer(VulkanCompositor::new(
                    window, None,
                )?)?)),
                None,
            )),
        }
    }

    fn import_surface_offer(
        &mut self,
        window: &Window,
        offer: &polymer_runtime::SurfaceOffer,
        api: ViewerApi,
        publish_video: bool,
        publish_stream: bool,
        quarter_turns: u8,
        video: &mut Option<WebRtcCompositor>,
    ) -> Result<(), Box<dyn Error>> {
        #[cfg(target_os = "macos")]
        {
            let Self::Metal(value) = self else {
                return Err("Metal compositor unavailable".into());
            };
            Renderer::import_surface_offer(&mut value.compositor, offer, quarter_turns)
        }
        #[cfg(windows)]
        {
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
                        *video = Some(WebRtcCompositor::D3d12(webrtc));
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
                    Renderer::import_surface_offer(video, offer, quarter_turns)?;
                }
                return Ok(());
            }

            let adapter = stream.adapter_luid;
            let matches = matches!(self, Self::Vulkan(value)
            if ((adapter.low_part == 0 && adapter.high_part == 0)
                    || value.compositor.matches_adapter(adapter)));
            if !matches {
                *video = None;
                drop(std::mem::replace(self, Self::Empty));
                let preferred =
                    (adapter.low_part != 0 || adapter.high_part != 0).then_some(adapter);
                if publish_video {
                    let (compositor, webrtc) =
                        VulkanCompositor::new_with_webrtc(window, preferred)?;
                    *self = Self::Vulkan(Box::new(vulkan_viewer(compositor)?));
                    *video = Some(WebRtcCompositor::Vulkan(webrtc));
                } else {
                    *self = Self::Vulkan(Box::new(vulkan_viewer(VulkanCompositor::new(
                        window, preferred,
                    )?)?));
                    *video = None;
                }
            }
            let Self::Vulkan(value) = self else {
                unreachable!()
            };
            Renderer::import_surface_offer(&mut value.compositor, offer, quarter_turns)?;
            if publish_stream && let Some(video) = video {
                Renderer::import_surface_offer(video, offer, quarter_turns)?;
            }
            Ok(())
        }
    }

    fn recreate_swapchain(&mut self, window: &Window) -> Result<bool, Box<dyn Error>> {
        match self {
            #[cfg(windows)]
            Self::Vulkan(value) => Renderer::recreate_swapchain(&mut value.compositor, window),
            #[cfg(windows)]
            Self::D3d12(value) => Renderer::recreate_swapchain(&mut value.compositor, window),
            #[cfg(target_os = "macos")]
            Self::Metal(value) => Renderer::recreate_swapchain(&mut value.compositor, window),
            Self::Empty => Ok(false),
        }
    }

    fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        match self {
            #[cfg(windows)]
            Self::Vulkan(value) => Renderer::set_latest(&mut value.compositor, connection, frame),
            #[cfg(windows)]
            Self::D3d12(value) => Renderer::set_latest(&mut value.compositor, connection, frame),
            #[cfg(target_os = "macos")]
            Self::Metal(value) => Renderer::set_latest(&mut value.compositor, connection, frame),
            Self::Empty => {}
        }
    }

    fn presentation(&self, viewer_extent: Extent) -> Presentation {
        match self {
            #[cfg(windows)]
            Self::Vulkan(value) => {
                Renderer::received_presentation(&value.compositor, viewer_extent)
            }
            #[cfg(windows)]
            Self::D3d12(value) => Renderer::received_presentation(&value.compositor, viewer_extent),
            #[cfg(target_os = "macos")]
            Self::Metal(value) => Renderer::received_presentation(&value.compositor, viewer_extent),
            Self::Empty => Presentation::new(viewer_extent, Vec::new()),
        }
    }

    fn resize_windows(&self, window: &impl RenderWindow) -> Vec<polymer_protocol::ResizeWindow> {
        let presentation = self.presentation(window.render_extent());
        let plan = layout::composition_plan(&presentation);
        match self {
            #[cfg(windows)]
            Self::Vulkan(value) => Renderer::resize_windows(&value.compositor, window, &plan),
            #[cfg(windows)]
            Self::D3d12(value) => Renderer::resize_windows(&value.compositor, window, &plan),
            #[cfg(target_os = "macos")]
            Self::Metal(value) => Renderer::resize_windows(&value.compositor, window, &plan),
            Self::Empty => Vec::new(),
        }
    }

    fn has_pending_frame(&self) -> bool {
        match self {
            #[cfg(windows)]
            Self::Vulkan(value) => Renderer::has_pending_frame(&value.compositor),
            #[cfg(windows)]
            Self::D3d12(value) => Renderer::has_pending_frame(&value.compositor),
            #[cfg(target_os = "macos")]
            Self::Metal(value) => Renderer::has_pending_frame(&value.compositor),
            Self::Empty => false,
        }
    }

    fn begin_frame(&mut self) {
        match self {
            #[cfg(windows)]
            Self::Vulkan(value) => Renderer::begin_frame(&mut value.compositor),
            #[cfg(windows)]
            Self::D3d12(value) => Renderer::begin_frame(&mut value.compositor),
            #[cfg(target_os = "macos")]
            Self::Metal(value) => Renderer::begin_frame(&mut value.compositor),
            Self::Empty => {}
        }
    }

    fn disconnect_stream(
        &mut self,
        connection: u64,
        video: &mut Option<WebRtcCompositor>,
    ) -> Result<(), Box<dyn Error>> {
        #[cfg(windows)]
        if let Some(video) = video {
            Renderer::disconnect_stream(video, connection)?;
        }
        match self {
            #[cfg(windows)]
            Self::Vulkan(value) => Renderer::disconnect_stream(&mut value.compositor, connection),
            #[cfg(windows)]
            Self::D3d12(value) => Renderer::disconnect_stream(&mut value.compositor, connection),
            #[cfg(target_os = "macos")]
            Self::Metal(value) => Renderer::disconnect_stream(&mut value.compositor, connection),
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
            #[cfg(target_os = "macos")]
            Self::Metal(value) => {
                let MetalViewer {
                    painter,
                    compositor,
                } = value.as_mut();
                painter.prepare(compositor, output)?;
                compositor.present_with(plan, |pass| painter.paint(pass, output, primitives))
            }
            #[cfg(windows)]
            Self::Vulkan(value) => {
                let VulkanViewer {
                    painter,
                    compositor,
                } = value.as_mut();
                painter.prepare(compositor, output)?;
                let video = match video {
                    Some(WebRtcCompositor::Vulkan(value)) => Some(value),
                    _ => None,
                };
                compositor.present_with(plan, video, |pass| painter.paint(pass, output, primitives))
            }
            #[cfg(windows)]
            Self::D3d12(value) => {
                let D3d12Viewer {
                    painter,
                    compositor,
                } = value.as_mut();
                painter.prepare(compositor, output)?;
                let video = match video {
                    Some(WebRtcCompositor::D3d12(value)) => Some(value),
                    _ => None,
                };
                compositor.present_with(plan, video, |pass| painter.paint(pass, output, primitives))
            }
            Self::Empty => Ok(false),
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let default_filter = if std::env::var_os("POLYMER_WEBRTC_TRACE").is_some() {
        "info,webrtc=trace"
    } else {
        "info"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    let statistics_recorder = polymer_statistics::install()?;
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
    #[cfg(target_os = "macos")]
    if options.webrtc.is_some() {
        return Err("Metal WebRTC capture is not implemented".into());
    }
    let viewer_api = options.api;
    let resize_guest = options.resize;
    #[cfg(windows)]
    let webrtc = webrtc_targets
        .as_ref()
        .map(|targets| {
            webrtc::Publisher::start(options.bind_webrtc, targets).map(std::sync::Arc::new)
        })
        .transpose()?;
    let executable_dir = env::current_exe()?
        .parent()
        .ok_or("viewer executable has no parent directory")?
        .to_path_buf();
    // Include a monotonic nonce so a stale AF_UNIX path left by a forcibly
    // terminated viewer can never collide with a subsequently reused PID.
    let session = format!(
        "polymer-{}-{:x}",
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
    let input_tx = InputEndpoint::default();
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
            options.skip_surfaces,
            viewer_api,
            launcher.tracked_render_target_indices(),
            launcher.render_target_surface_patterns(),
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
                .with_title("Polymer")
                .with_inner_size(LogicalSize::new(1280, 720))
                .with_resizable(true),
        )?;
        #[cfg(windows)]
        let viewer_window = native_window(&window)?;
        let mut egui_context = egui::Context::default();
        let mut egui_state = egui_winit::State::new(
            egui_context.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        #[cfg(windows)]
        let publish_video = webrtc.is_some();
        #[cfg(target_os = "macos")]
        let publish_video = false;
        let (mut compositor, mut video_compositor) =
            ViewerCompositor::new(&window, viewer_api, publish_video)?;
        let frequency = performance_frequency().unwrap_or(1) as f64;
        let mut stats = Statistics::new(statistics_recorder, viewer_api);
        let mut pointer_routing = PointerRoutingState::new(Instant::now());
        let mut stream_indices = HashMap::<u64, u32>::new();
        let mut active_backend = "Waiting";
        let mut active_consumer = "Waiting";
        let mut pressed_keys = HashSet::<KeyCode>::new();
        let mut last_resize_windows = Vec::new();

        Ok(Box::new(move |event, target| match event {
        Event::AboutToWait => {
            if resize_guest && launcher.isolate {
                let resize_windows = compositor.resize_windows(&window);
                if resize_windows != last_resize_windows {
                    for resize in &resize_windows {
                        let _ = input_tx.send(polymer_ipc::Message::ResizeWindow(*resize));
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
                        tracing::error!("polymer: swapchain recreation failed: {error}");
                        target.exit();
                    }
                }
                WindowEvent::Focused(focused) => {
                    if !focused {
                        pressed_keys.clear();
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
                            );
                        }
                    }
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        #[cfg(target_os = "macos")]
                        if let Some(provider) = &gamepad_provider {
                            use winit::platform::scancode::PhysicalKeyExtScancode;
                            provider.publish_host(polymer_input::HostInputEvent::Key {
                                code: polymer_input::KeyCode::new(&format!("{code:?}")),
                                scan_code: event.physical_key.to_scancode().unwrap_or(0) as u16,
                                virtual_key: 0,
                                pressed: event.state == ElementState::Pressed,
                                repeat: event.repeat,
                                extended: false,
                                target_window: 0,
                                timestamp_ms: 0,
                                sequence: 0,
                            });
                        }
                        match event.state {
                            ElementState::Pressed => {
                                pressed_keys.insert(code);
                            }
                            ElementState::Released => {
                                pressed_keys.remove(&code);
                            }
                        }
                    }
                    #[cfg(windows)]
                    if let Some(backing) = load_window(&backing_window) {
                        if event.state == ElementState::Pressed
                            && let Some(text) = event.text
                        {
                            for character in text.chars() {
                                if !character.is_control() {
                                    #[cfg(windows)]
                                    forward_character(backing, character);
                                }
                            }
                        }
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    pointer_routing.set_cursor(position.x, position.y);
                    let size = window.inner_size();
                    let presentation =
                        compositor.presentation(Extent::new(size.width, size.height));
                    let plan = layout::composition_plan(&presentation);
                    pointer_routing.send(
                        PointerPhase::Move,
                        0,
                        0,
                        0,
                        &presentation,
                        &plan,
                        &pointer_tx,
                    );
                }
                WindowEvent::MouseInput { state, button, .. } => {
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
                        pointer_routing.send(
                            phase,
                            u32::from(button),
                            0,
                            0,
                            &presentation,
                            &plan,
                            &pointer_tx,
                        );
                    }
                }
                WindowEvent::MouseWheel { delta, .. } => {
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
                    pointer_routing.send(
                        PointerPhase::Wheel,
                        0,
                        horizontal,
                        vertical,
                        &presentation,
                        &plan,
                        &pointer_tx,
                    );
                }
                WindowEvent::RedrawRequested => {
                    let size = window.inner_size();
                    if size.width == 0 || size.height == 0 {
                        return;
                    }
                    if active_target
                        .as_ref()
                        .is_some_and(|active| {
                            matches!(
                                active.instance.state(),
                                polymer_runtime::InstanceState::Exited(_)
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
                                        "polymer: ignored {:?} surface window {:#x}; tracked surface {} is outside the viewer limit {}",
                                        stream.image_kind,
                                        stream.source_window,
                                        stream_index,
                                        surface_limit
                                    );
                                    capture.reject(offer);
                                    continue;
                                }
                                active_backend = stream.hook.label();
                                #[cfg(windows)]
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
                                                    "polymer: capture stream {connection} disappeared while it was being imported: {error}"
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
                                            );
                                            stream_indices.remove(&existing);
                                            stats.remove_surface(existing);
                                            #[cfg(windows)]
if let Some(publisher) = &webrtc {
                                                publisher.remove_stream(existing);
                                            }
                                            capture.revoke(existing);
                                            if let Err(error) = compositor.disconnect_stream(
                                                existing,
                                                &mut video_compositor,
                                            ) {
                                                tracing::error!(
                                                    "polymer: failed to replace prior stream: {error}"
                                                );
                                            }
                                        }
                                        stream_indices.insert(connection, stream_index);
                                        stats.add_surface(
                                            connection,
                                            stream.surface_id.0,
                                            stream_index,
                                            Extent::new(stream.width, stream.height),
                                        );
                                        #[cfg(windows)]
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
                                        #[cfg(windows)]
                                        { active_consumer = if viewer_api == ViewerApi::D3d12
                                            && d3d12_consumable(&stream)
                                        {
                                            "D3D12/DXGI"
                                        } else {
                                            "Vulkan"
                                        }; }
                                        #[cfg(target_os = "macos")]
                                        { active_consumer = "Metal"; }
                                        tracing::info!(
                                            "polymer: imported {active_backend} stream {}x{} format {} {:?}/{:?} (surface {:#x}) for window {:#x} with {} slots using {}",
                                            stream.width,
                                            stream.height,
                                            stream.format,
                                            stream.image_kind,
                                            stream.sync_kind,
                                            stream.surface_id.0,
                                            stream.source_window,
                                            stream.slot_count,
                                            active_consumer,
                                        );
                                    }
                                    Err(error) => {
                                        tracing::error!("polymer: direct import failed: {error}");
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
                                        "polymer: received first {active_backend} frame on connection {connection}"
                                    );
                                }
                                stats.observe_frame(connection, frame, frequency);
                                compositor.set_latest(connection, frame);
                                #[cfg(windows)]
                                if let Some(video) = &mut video_compositor { Renderer::set_latest(video, connection, frame); }
                                if active_backend == "D3D11" {
                                    // Keyed-mutex frames must be consumed and released in order;
                                    // do not overwrite one with a newer socket message.
                                    break;
                                }
                            }
                            SurfaceCaptureEvent::Removed { connection, .. } => {
                                stats.remove_surface(connection);
                                #[cfg(windows)]
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
                                );
                                if stream_indices.remove(&connection).is_none() {
                                    continue;
                                }
                                if let Err(error) = compositor
                                    .disconnect_stream(connection, &mut video_compositor)
                                {
                                    tracing::error!("polymer: stream disconnect failed: {error}");
                                }
                            }
                        }
                    }
                    let has_pending_frame = compositor.has_pending_frame();
                    stats.sample();
                    let input = egui_state.take_egui_input(&window);
                    #[cfg(windows)]
let webrtc_stats = webrtc.as_ref().map(|publisher| publisher.stats());
                    let last_pointer_sent = pointer_routing.last_sent();
                    let mut launch_requested = false;
                    let mut close_requested = false;
                    let mut output = egui_context.run(input, |context| {
                        egui::TopBottomPanel::top("statistics").show(context, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.strong(format!(
                                    "Polymer {active_backend} via {active_consumer}"
                                ));
                                ui.toggle_value(&mut show_layout_css, "Layout CSS");
                                if active_target.is_some()
                                    && ui.button("Close program").clicked()
                                {
                                    close_requested = true;
                                }
                                ui.label(format!("Frame {}", stats.frame_number));
                                ui.label(format!("Slot {}", stats.slot));
                                for render_target in &stats.render_targets {
                                    ui.label(format!(
                                        "Render target {}: {}x{}",
                                        render_target.target,
                                        render_target.extent.width,
                                        render_target.extent.height
                                    ));
                                }
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
                            egui::Window::new("Polymer error")
                                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                                .collapsible(false)
                                .show(context, |ui| {
                                    ui.colored_label(egui::Color32::LIGHT_RED, message);
                                    if ui.button("Dismiss").clicked() {
                                        launch_error = None;
                                    }
                                });
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
                        #[cfg(windows)]
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
                            options.skip_surfaces,
                            viewer_api,
                            launcher.tracked_render_target_indices(),
                            launcher.render_target_surface_patterns(),

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
                                tracing::error!("polymer: swapchain recreation failed: {error}");
                                target.exit();
                            }
                        }
                        Err(error) => {
                            tracing::error!("polymer: direct composition failed: {error}");
                            target.exit();
                        }
                    }
                    #[cfg(windows)]
if let (Some(publisher), Some(video_compositor)) =
                        (&webrtc, &mut video_compositor)
                    {
                        video_compositor.set_publisher(publisher.clone());
                        if let Err(error) = Renderer::present(video_compositor, &composition_plan) {
                            tracing::error!("polymer: video publication failed: {error}");
                            target.exit();
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
    instance: polymer_runtime::EmulatorInstance,
    // Declared after instance so the guest stops before the mount is released.
    _filesystem: polymer_vfs::MountedFilesystem,
}

struct StartedTarget {
    target: ActiveTarget,
    provider: InputHub,
    layouts: Vec<InputLayout>,
}

#[expect(clippy::too_many_arguments)]
fn start_target_session(
    launcher: &LauncherState,
    executable_dir: &Path,
    session: &str,
    skip_surfaces: u32,
    viewer_api: ViewerApi,
    tracked_render_targets: Option<Vec<u32>>,
    tracked_surface_patterns: Vec<Option<String>>,
) -> Result<StartedTarget, Box<dyn Error>> {
    let mut target = launcher.target().map_err(std::io::Error::other)?;
    if launcher.hooks.is_empty() {
        return Err(std::io::Error::other("Select at least one graphics hook").into());
    }
    launcher
        .validate_render_targets()
        .map_err(std::io::Error::other)?;
    let gamepads = launcher
        .configured_gamepads()
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
                .map(|(slot, &index)| polymer_runtime::TrackedSurfaceConfig {
                    id: polymer_runtime::TrackedSurfaceId(index),
                    render_target_index: index,
                    window_pattern: tracked_surface_patterns.get(slot).cloned().flatten(),

                    rotation: 0,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            (0..1)
                .map(|index| polymer_runtime::TrackedSurfaceConfig {
                    id: polymer_runtime::TrackedSurfaceId(index),
                    render_target_index: index,
                    window_pattern: None,
                    rotation: 0,
                })
                .collect()
        });
    let filesystem = filesystem::DebugFilesystem::mount(&mut target)?;
    let instance = polymer_runtime::EmulatorInstance::launch(
        polymer_runtime::EmulatorLaunchConfig {
            session: session.to_owned(),
            command: target[0].clone(),
            working_directory: None,
            env: Vec::new(),
            arguments: target[1..].to_vec(),
            artifact_root: executable_dir.to_path_buf(),
            hooks: polymer_runtime::Hooks {
                graphics: launcher.hooks.clone(),
                #[cfg(windows)]
                input: vec![polymer_runtime::InputHook::Sdl3],
                #[cfg(target_os = "macos")]
                input: Vec::new(),
            },
            consumer: match viewer_api {
                ViewerApi::D3d12 => polymer_runtime::ConsumerPreference::D3d12,
                ViewerApi::Vulkan => polymer_runtime::ConsumerPreference::Vulkan,
                ViewerApi::Metal => polymer_runtime::ConsumerPreference::Metal,
            },
            isolate: launcher.isolate,
            tracked_surfaces: configured_surfaces,
            tracking_enabled: tracked_render_targets.is_some(),
            skip_surfaces,
        },
        &filesystem,
    )?;
    let provider = InputHub::default();
    #[cfg(windows)]
    {
        let backend = Arc::new(
            polymer_input_sdl3::Sdl3Backend::new(Default::default())
                .map_err(|error| std::io::Error::other(error.to_string()))?,
        );
        provider
            .register_backend(backend)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
    }
    instance.input().attach(&provider);
    let topology = polymer_controllers::topology::InputTopology {
        revision: 1,
        machine: None,
        ports: gamepads
            .iter()
            .enumerate()
            .map(|(player, pad)| polymer_controllers::topology::PortBinding {
                player: player as u8,
                keyboard_mouse: matches!(
                    pad.source,
                    Some(polymer_config::input::SourceSelector::KeyboardMouse)
                ),
                group: "controller".to_owned(),
                port: player as u8,
                layout: pad.layout.clone(),
                mode: polymer_controllers::topology::RemapMode::Position,
                bindings: Vec::new(),
                pointer: polymer_controllers::topology::PointerBinding::default(),
            })
            .collect(),
        raw_hid: Vec::new(),
    };
    let _ = instance.input().publish_topology(&topology);
    Ok(StartedTarget {
        target: ActiveTarget {
            instance,
            _filesystem: filesystem,
        },
        provider,
        layouts,
    })
}
