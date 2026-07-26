#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod background_host;
mod cli;
mod composition;
mod host;
mod input;
mod launch;

use api::{LaunchOptions, parse_input_binding};
use cli::ShellOptions;
use composition::{
    CompositionTree, ForegroundHitRegion, SurfaceLayout, SurfaceWindow, WebViewLayer,
};
use host::IGameViewHost;
use input::InputTransport;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use snowglass_input::{
    mouse::{PointerPhase, layout::Rect},
    provider::sdl3::{ProviderConfig as Sdl3ProviderConfig, Sdl3Provider, SlotConfig},
};
use snowglass_input_dinput::DirectInputForwarder;
use snowglass_platform_windows::{TargetProcess, performance_counter, performance_frequency};
use snowglass_protocol::{PresentationMappingKind, PresentationTransform};
use snowglass_renderer::{
    CompositingPlan, D3d12Compositor, Extent, Renderer, SurfacePlacement, WebRtcCompositor,
    capture::{Capture, CaptureEvent, ConsumerPreference},
    cli::ViewerApi,
    debug::{
        DebugOverlay, FrameSequenceTracker, InputAxisDiagnostics, InputDiagnostics,
        InputSlotDiagnostics, TargetStatisticsTracker, draw_debug_overlay,
    },
    input::PointerRoutingState,
    webrtc,
};
use std::{
    collections::HashMap,
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_MOUSE_EVENT_KIND, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS,
    COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC, ICoreWebView2CompositionController,
};
use windows::{
    Win32::{
        Foundation::{POINT, WAIT_FAILED, WAIT_OBJECT_0},
        Graphics::DirectComposition::DCompositionWaitForCompositorClock,
        Media::{TIMERR_NOERROR, timeBeginPeriod, timeEndPeriod},
        System::{Com::IDispatch, Threading::INFINITE, Variant::VARIANT},
        UI::{
            Input::KeyboardAndMouse::SetFocus,
            WindowsAndMessaging::{
                WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL,
                WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN,
                WM_XBUTTONUP,
            },
        },
    },
    core::{Interface, w},
};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};
use wry::{WebView, WebViewBuilder, WebViewBuilderExtWindows, WebViewExtWindows};

#[derive(Debug)]
enum UserEvent {
    WakeCapture,
    CompositorTick,
    FrontendReady,
    Launch {
        instance_id: String,
        options: LaunchOptions,
    },
    CloseInstance(String),
    GameMessage {
        instance_id: String,
        name: String,
        args: serde_json::Value,
    },
    Layout(Vec<SurfaceLayout>),
    InputPriority(InputPriority),
    BackgroundMessage(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputPriority {
    Game,
    Foreground,
}

// Capture can produce events much faster than the window message queue when
// several streams are active. Keep each dispatch small so Win32/winit input
// and egui repaint messages get a turn between capture batches.
const MAX_CAPTURE_EVENTS_PER_TURN: usize = 32;
/// Composition-tree key for the single visual every stream shares when the
/// shell composites them together. Not a real connection.
const MERGED_VISUAL_KEY: u64 = u64::MAX;
/// Target index for the background page, which occupies no tracked slot.
const BACKGROUND_TARGET_INDEX: u32 = u32::MAX;
const PENDING_SURFACE_RETRY: Duration = Duration::from_millis(1);
const SURFACE_RESIZE_SETTLE: Duration = Duration::from_millis(75);
const EGUI_STATISTICS_REFRESH: Duration = Duration::from_millis(100);
const DOM_MK_LBUTTON: u32 = 0x0001;
const DOM_MK_RBUTTON: u32 = 0x0002;
const DOM_MK_SHIFT: u32 = 0x0004;
const DOM_MK_CONTROL: u32 = 0x0008;
const DOM_MK_MBUTTON: u32 = 0x0010;
const DOM_MK_XBUTTON1: u32 = 0x0020;
const DOM_MK_XBUTTON2: u32 = 0x0040;
const DOM_MOUSE_BUTTONS: u32 =
    DOM_MK_LBUTTON | DOM_MK_RBUTTON | DOM_MK_MBUTTON | DOM_MK_XBUTTON1 | DOM_MK_XBUTTON2;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct WebRenderTarget {
    connection: String,
    index: u32,
    width: u32,
    height: u32,
}

struct CapturedSurface {
    index: u32,
    preferred_width: u32,
    preferred_height: u32,
    swapchain_width: u32,
    swapchain_height: u32,
    viewport_width: u32,
    viewport_height: u32,
    viewport_x: i32,
    viewport_y: i32,
    quarter_turns: u8,
    pointer_events: bool,
    z_index: i32,
    needs_redraw: bool,
    resize_due: Option<Instant>,
    /// `None` when the shell composites every stream into one shared swapchain,
    /// in which case the compositor lives on the shell instead.
    compositor: Option<D3d12Compositor>,
    webrtc: Option<WebRtcCompositor>,
}

struct ActiveGame {
    instance_id: String,
    options: LaunchOptions,
    webrtc: Option<webrtc::Publisher>,
}

struct EguiOverlay {
    compositor: D3d12Compositor,
    context: egui::Context,
    state: egui_winit::State,
    diagnostics: DebugOverlay,
    sequences: FrameSequenceTracker,
    target_statistics: TargetStatisticsTracker,
    frequency: f64,
    next_repaint: Instant,
}

impl EguiOverlay {
    fn new(window: &Window) -> Result<Self, Box<dyn Error>> {
        let size = window.inner_size();
        let render_window = SurfaceWindow::new(window, size.width.max(1), size.height.max(1));
        let compositor = D3d12Compositor::new_default_for_transparent_composition(&render_window)?;
        let context = egui::Context::default();
        let state = egui_winit::State::new(
            context.clone(),
            egui::ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        let mut diagnostics = DebugOverlay::default();
        diagnostics.frame.backend = "Waiting".to_owned();
        diagnostics.frame.consumer = "D3D12/DXGI".to_owned();
        Ok(Self {
            compositor,
            context,
            state,
            diagnostics,
            sequences: FrameSequenceTracker::default(),
            target_statistics: TargetStatisticsTracker::default(),
            frequency: performance_frequency().unwrap_or(1) as f64,
            next_repaint: Instant::now(),
        })
    }

    fn content(&self) -> Result<windows::core::IUnknown, windows::core::Error> {
        self.compositor.composition_content()
    }

    fn set_backend(&mut self, backend: String) {
        self.diagnostics.frame.backend = backend;
    }

    fn observe_frame(
        &mut self,
        connection: u64,
        target: u32,
        frame: snowglass_protocol::FrameReady,
    ) {
        let diagnostics = &mut self.diagnostics.frame;
        let observation =
            self.sequences
                .observe(connection, frame.frame_number, frame.timestamp_qpc);
        let frame_interval_ms = observation
            .interval_qpc
            .map(|interval_qpc| interval_qpc * 1000.0 / self.frequency);
        diagnostics.dropped = self.sequences.dropped();
        let latency_ms = performance_counter()
            .unwrap_or(frame.timestamp_qpc)
            .saturating_sub(frame.timestamp_qpc) as f64
            * 1000.0
            / self.frequency;
        self.target_statistics
            .observe(connection, target, latency_ms, frame_interval_ms);
        diagnostics.frame_number = frame.frame_number;
        diagnostics.slot = frame.slot;
        diagnostics.render_width = frame.render_width;
        diagnostics.render_height = frame.render_height;
        diagnostics.capture_x = frame.capture_x;
        diagnostics.capture_y = frame.capture_y;
        diagnostics.capture_width = frame.capture_width;
        diagnostics.capture_height = frame.capture_height;
    }

    fn sample(&mut self) {
        if !self.target_statistics.sample() {
            return;
        }
        self.diagnostics.frame.target_statistics = self.target_statistics.statistics();
    }

    fn remove_target(&mut self, connection: u64) {
        self.sequences.remove(connection);
        self.target_statistics.remove(connection);
        self.diagnostics.frame.target_statistics = self.target_statistics.statistics();
    }

    fn resize(&mut self, window: &Window) -> Result<(), Box<dyn Error>> {
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let render_window = SurfaceWindow::new(window, size.width, size.height);
        Renderer::recreate_swapchain(&mut self.compositor, &render_window)?;
        Ok(())
    }

    fn draw(&mut self, window: &Window) -> Result<bool, Box<dyn Error>> {
        if !self.compositor.try_begin_frame() {
            self.next_repaint = Instant::now() + PENDING_SURFACE_RETRY;
            return Ok(false);
        }
        self.sample();
        let input = self.state.take_egui_input(window);
        let diagnostics = &self.diagnostics;
        let output = self.context.run(input, |context| {
            draw_debug_overlay(context, diagnostics);
        });
        self.state
            .handle_platform_output(window, output.platform_output.clone());
        let primitives = self
            .context
            .tessellate(output.shapes.clone(), output.pixels_per_point);
        let repaint_delay = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(EGUI_STATISTICS_REFRESH, |viewport| viewport.repaint_delay)
            .min(EGUI_STATISTICS_REFRESH);
        self.next_repaint = Instant::now() + repaint_delay;
        let plan = CompositingPlan::new([0.0, 0.0, 0.0, 0.0], Vec::new());
        Renderer::draw(&mut self.compositor, &output, &primitives, &plan)?;
        Ok(true)
    }

    fn repaint_due(&self, now: Instant) -> bool {
        self.next_repaint <= now
    }

    fn next_repaint(&self) -> Instant {
        self.next_repaint
    }
}

impl CapturedSurface {
    fn composition_plan(&self) -> CompositingPlan {
        let extent = Extent::new(self.swapchain_width, self.swapchain_height);
        let Some(compositor) = self.compositor.as_ref() else {
            return CompositingPlan::new([0.0, 0.0, 0.0, 1.0], Vec::new());
        };
        let presentation = Renderer::received_presentation(compositor, extent);
        CompositingPlan::new(
            [0.0, 0.0, 0.0, 1.0],
            presentation
                .surfaces()
                .iter()
                .map(|surface| SurfacePlacement {
                    connection: surface.connection,
                    surface_id: surface.surface_id,
                    rectangle: snowglass_input::mouse::layout::Rect::new(
                        0,
                        0,
                        extent.width,
                        extent.height,
                    ),
                })
                .collect(),
        )
    }
}

/// Wakes the event loop once per DirectComposition frame.
///
/// DWM samples composition swapchains once per composition pass. A present that
/// lands just after that sampling waits out a whole refresh interval, so pacing
/// deferred presents on a fixed timer leaves them landing on either side of the
/// deadline at random. Waking on the compositor clock instead puts the retry in
/// phase with DWM, giving each frame a full interval of headroom.
struct CompositorClock {
    waiter: std::thread::Thread,
    active: Arc<AtomicBool>,
    healthy: Arc<AtomicBool>,
    tick_pending: Arc<AtomicBool>,
}

impl CompositorClock {
    fn spawn(proxy: winit::event_loop::EventLoopProxy<UserEvent>) -> std::io::Result<Self> {
        let active = Arc::new(AtomicBool::new(false));
        let healthy = Arc::new(AtomicBool::new(true));
        let tick_pending = Arc::new(AtomicBool::new(false));
        let thread_active = Arc::clone(&active);
        let thread_healthy = Arc::clone(&healthy);
        let thread_tick_pending = Arc::clone(&tick_pending);
        let handle = std::thread::Builder::new()
            .name("snowglass-compositor-clock".to_owned())
            .spawn(move || {
                loop {
                    // A shell with no streams on screen has nothing to pace.
                    // Park instead of waking once per refresh for no reason;
                    // `park` may return spuriously, so re-check the flag.
                    while !thread_active.load(Ordering::Acquire) {
                        std::thread::park();
                    }
                    // SAFETY: the call reads the process-wide compositor clock
                    // and is passed no handles to borrow.
                    let status = unsafe { DCompositionWaitForCompositorClock(None, INFINITE) };
                    if status == WAIT_FAILED.0 {
                        // Fall back to timer pacing rather than spinning on a
                        // clock this session cannot wait on.
                        thread_healthy.store(false, Ordering::Release);
                        return;
                    }
                    // With no handles supplied, a clock tick is the first (and
                    // only) wait object.
                    if status != WAIT_OBJECT_0.0 {
                        continue;
                    }
                    // Coalesce ticks so a busy loop cannot accumulate a backlog
                    // of wake-ups it will never drain.
                    if !thread_tick_pending.swap(true, Ordering::AcqRel)
                        && proxy.send_event(UserEvent::CompositorTick).is_err()
                    {
                        return;
                    }
                }
            })?;
        Ok(Self {
            waiter: handle.thread().clone(),
            active,
            healthy,
            tick_pending,
        })
    }

    /// Paces the event loop only while streams are on screen.
    fn set_active(&self, active: bool) {
        if self.active.swap(active, Ordering::AcqRel) != active && active {
            self.waiter.unpark();
        }
    }

    /// Reports whether the clock is driving redraws, so the event loop can keep
    /// its timer fallback for sessions where the wait is unavailable.
    fn is_pacing(&self) -> bool {
        self.healthy.load(Ordering::Acquire) && self.active.load(Ordering::Acquire)
    }

    fn clear_tick(&self) {
        self.tick_pending.store(false, Ordering::Release);
    }
}

struct ShellState {
    // Drop WebView2 and its COM host before tearing down their visual tree.
    foreground_webview: WebView,
    /// `None` when the page is hosted on its own desktop instead, in which case
    /// it is reached through `background_host`.
    background_webview: Option<WebView>,
    background_host: Option<background_host::BackgroundHost>,
    _gameview_host: IGameViewHost,
    window: Window,
    composition: CompositionTree,
    overlay: EguiOverlay,
    capture: Capture,
    capture_wake_pending: Arc<AtomicBool>,
    capture_dirty: Arc<AtomicBool>,
    capture_work_pending: bool,
    compositor_clock: CompositorClock,
    // Retained so the hosted background page keeps rendering and being captured.
    /// One window-spanning opaque swapchain shared by every stream. Opaque is
    /// what earns the overlay plane, and it is only safe to be opaque because
    /// the captured background fills the pixels no stream covers.
    merged: Option<D3d12Compositor>,
    merged_attached: bool,
    merged_without_background: bool,
    merged_presents_every_tick: bool,
    /// Stream carrying the captured background page, if it has connected.
    background_connection: Option<u64>,
    session: String,
    surfaces: HashMap<u64, CapturedSurface>,
    target: Option<TargetProcess>,
    active_game: Option<ActiveGame>,
    pending_cli_launch: Option<LaunchOptions>,
    _input: InputTransport,
    input_provider: Option<Sdl3Provider>,
    pointer_routing: PointerRoutingState,
    direct_input: DirectInputForwarder,
    input_priority: InputPriority,
    cursor: POINT,
    dom_mouse_keys: u32,
    foreground_hit_regions: Vec<ForegroundHitRegion>,
    foreground_mouse_capture: bool,
    last_input_state: String,
    ui_controls: HashMap<String, i32>,
    next_input_publish: Instant,
}

impl ShellState {
    fn new(
        event_loop: &ActiveEventLoop,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
        options: ShellOptions,
    ) -> Result<Self, Box<dyn Error>> {
        let ShellOptions {
            viewer: options,
            foreground_url,
            background_url,
        } = options;
        if options.api != ViewerApi::D3d12 {
            return Err(
                "snowglass-shell-webview2 requires `--api d3d12` for DirectComposition".into(),
            );
        }
        let pending_cli_launch =
            LaunchOptions::from_cli(&options).map_err(std::io::Error::other)?;

        let window = event_loop.create_window(
            Window::default_attributes()
                .with_title("Snowglass")
                .with_inner_size(LogicalSize::new(1280.0, 720.0)),
        )?;
        let composition = CompositionTree::new(&window)?;
        // The session has to exist before the background page does. WebView2
        // spawns its processes from this one and they inherit this environment,
        // which is the only way a hook injected into them afterwards can find
        // the shell to publish frames to.
        let session = unique_session();
        let capture_background = std::env::var_os("SNOWGLASS_BACKGROUND_CAPTURE").is_some();
        if capture_background || std::env::var_os("SNOWGLASS_BACKGROUND_HOOK").is_some() {
            // Must happen before the first WebView2 process is created.
            if !arm_webview_process_hook() {
                return Err(
                    "the WebView2 process hook requires its DLL and injector beside the shell"
                        .into(),
                );
            }
            let log = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf));
            // SAFETY: set before any thread that reads the environment is
            // started, while the shell is still single-threaded here.
            unsafe {
                std::env::set_var(snowglass_hook::config::SESSION, &session);
                if let Some(root) = log {
                    std::env::set_var(
                        snowglass_hook::config::DXGI_LOG_FILE,
                        root.join("snowglass-dxgi.log"),
                    );
                    std::env::set_var(
                        snowglass_hook::config::D3D11_LOG_FILE,
                        root.join("snowglass-d3d11.log"),
                    );
                }
            }
        }
        let background_script = include_str!(concat!(env!("OUT_DIR"), "/background.js"));
        // WebView2 cannot render offscreen, so capturing the background page
        // means hosting it in a real window and reading that window back.
        // Visual hosting has no HWND to capture.
        let (background_webview, background_host) = if capture_background {
            let size = window.inner_size();
            let animate = std::env::var_os("SNOWGLASS_BACKGROUND_ANIMATE").is_some();
            let url = background_url.clone();
            let background_data_directory = std::env::temp_dir()
                .join(format!("snowglass-background-{}", std::process::id()));
            let host = background_host::BackgroundHost::start(
                size.width as i32,
                size.height as i32,
                move |target| {
                    // Its own user data folder, because it now has its own
                    // environment with its own browser arguments. Two
                    // environments sharing a folder with different arguments is
                    // rejected outright.
                    let mut context = wry::WebContext::new(Some(background_data_directory));
                    let webview = WebViewBuilder::new_with_web_context(&mut context)
                        .with_focused(false)
                        .with_initialization_script(background_script)
                        // Chromium stops painting windows it decides are not
                        // being seen, and nobody is looking at this desktop.
                        // `--no-sandbox` is what lets the graphics hook reach
                        // the GPU process that presents this page.
                        .with_additional_browser_args(
                            "--disable-features=CalculateNativeWinOcclusion \
                             --disable-backgrounding-occluded-windows \
                             --no-sandbox",
                        )
                        .build(target)?;
                    if animate {
                        webview.load_html(ANIMATED_BACKGROUND)?;
                    } else {
                        webview.load_url(&url)?;
                    }
                    Ok(webview)
                },
            )?;
            (None, Some(host))
        } else {
            let background_visual = composition.webview_visual(WebViewLayer::Background)?;
            let webview = WebViewBuilder::new()
                .with_transparent(true)
                .with_focused(false)
                .with_initialization_script(background_script)
                .with_composition_visual_target(background_visual)
                .with_composition_input_enabled(false)
                .build(&window)?;
            (Some(webview), None)
        };
        let foreground_visual = composition.webview_visual(WebViewLayer::Foreground)?;
        let foreground_proxy = proxy.clone();
        let gameview_script = include_str!(concat!(env!("OUT_DIR"), "/gameview.js"));
        let mut foreground_builder = WebViewBuilder::new()
            .with_focused(false)
            .with_transparent(true)
            .with_initialization_script(gameview_script);
        // Sharing the background's environment would put both pages in one
        // browser process. A desktop-hosted background keeps its own, which is
        // what confines its process tree to that desktop.
        if let Some(background) = background_webview.as_ref() {
            foreground_builder = foreground_builder.with_environment(background.environment());
        }
        let foreground_webview = foreground_builder
            .with_composition_visual_target(foreground_visual)
            .with_composition_input_enabled(false)
            .with_ipc_handler(move |request| {
                let Ok(message) = serde_json::from_str::<serde_json::Value>(request.body()) else {
                    return;
                };
                match message.get("type").and_then(serde_json::Value::as_str) {
                    Some("frontendReady") => {
                        let _ = foreground_proxy.send_event(UserEvent::FrontendReady);
                    }
                    Some("setInputPriority") => {
                        let priority =
                            if message.get("priority").and_then(serde_json::Value::as_str)
                                == Some("foreground")
                            {
                                InputPriority::Foreground
                            } else {
                                InputPriority::Game
                            };
                        let _ = foreground_proxy.send_event(UserEvent::InputPriority(priority));
                    }
                    Some("background") => {
                        if let Some(payload) = message.get("message") {
                            let _ = foreground_proxy
                                .send_event(UserEvent::BackgroundMessage(payload.to_string()));
                        }
                    }
                    Some("layout") => {
                        let Some(layouts) = message.get("layouts") else {
                            return;
                        };
                        match serde_json::from_value::<Vec<SurfaceLayout>>(layouts.clone()) {
                            Ok(layouts) => {
                                let _ = foreground_proxy.send_event(UserEvent::Layout(layouts));
                            }
                            Err(error) => {
                                eprintln!("snowglass: invalid WebView2 surface layout: {error}");
                            }
                        }
                    }
                    _ => {}
                }
            })
            .build(&window)?;

        let gameview_host = host::create(proxy.clone());
        let dispatch: IDispatch = gameview_host.cast()?;
        let mut host_variant = VARIANT::from(dispatch);
        let core: ICoreWebView2 = foreground_webview.webview();
        // SAFETY: the variant owns an IDispatch reference for the duration of
        // the call; WebView2 retains its own reference to the host object.
        unsafe { core.AddHostObjectToScript(w!("snowglass"), &mut host_variant)? };

        // A page driven by `requestAnimationFrame`, which is what Chromium
        // throttles when it decides a window is not being seen. Used to check
        // that a hidden, captured background keeps producing frames.
        const ANIMATED_BACKGROUND: &str = r#"<!doctype html><html><body style="margin:0">
<script>
let n = 0;
function tick() {
  n = (n + 1) % 256;
  document.body.style.background = `rgb(${n},${n},${n})`;
  requestAnimationFrame(tick);
}
requestAnimationFrame(tick);
</script></body></html>"#;
        // A desktop-hosted page loads on its own thread, at build time.
        if let Some(background) = background_webview.as_ref() {
            if std::env::var_os("SNOWGLASS_BACKGROUND_ANIMATE").is_some() {
                background.load_html(ANIMATED_BACKGROUND)?;
            } else {
                background.load_url(&background_url)?;
            }
        }
        foreground_webview.load_url(&foreground_url)?;

        let mut overlay = EguiOverlay::new(&window)?;
        composition.set_egui_overlay_content(&overlay.content()?)?;
        overlay.draw(&window)?;

        let input = InputTransport::bind(&session)?;
        let input_provider = None;
        let control_sender = input.control_sender();
        let wake_proxy = proxy.clone();
        let capture_wake_pending = Arc::new(AtomicBool::new(false));
        let capture_dirty = Arc::new(AtomicBool::new(false));
        let capture_wake_pending_for_thread = Arc::clone(&capture_wake_pending);
        let capture_dirty_for_thread = Arc::clone(&capture_dirty);
        let capture = Capture::bind_with_waker(&session, ConsumerPreference::D3d12, move || {
            capture_dirty_for_thread.store(true, Ordering::Release);
            if !capture_wake_pending_for_thread.swap(true, Ordering::AcqRel)
                && wake_proxy.send_event(UserEvent::WakeCapture).is_err()
            {
                capture_wake_pending_for_thread.store(false, Ordering::Release);
            }
        })?;
        let compositor_clock = CompositorClock::spawn(proxy.clone())?;
        let target = None;

        composition.commit()?;
        Ok(Self {
            foreground_webview,
            background_webview,
            background_host,
            _gameview_host: gameview_host,
            window,
            composition,
            overlay,
            capture,
            capture_wake_pending,
            capture_dirty,
            capture_work_pending: false,
            compositor_clock,
            merged: None,
            merged_attached: false,
            merged_without_background: std::env::var_os("SNOWGLASS_MERGED_ONLY").is_some(),
            merged_presents_every_tick: std::env::var_os("SNOWGLASS_MERGED_TICK").is_some(),
            background_connection: None,
            session,
            surfaces: HashMap::new(),
            target,
            active_game: None,
            pending_cli_launch,
            _input: input,
            input_provider,
            pointer_routing: PointerRoutingState::new(Instant::now()),
            direct_input: DirectInputForwarder::new(control_sender),
            input_priority: InputPriority::Foreground,
            cursor: POINT::default(),
            dom_mouse_keys: 0,
            foreground_hit_regions: Vec::new(),
            foreground_mouse_capture: false,
            last_input_state: String::new(),
            ui_controls: HashMap::new(),
            next_input_publish: Instant::now(),
        })
    }

    fn deliver_cli_launch(&mut self) {
        let Some(options) = self.pending_cli_launch.take() else {
            return;
        };
        let Ok(options) = serde_json::to_string(&options) else {
            return;
        };
        let script = format!(
            "window.postMessage({{ type: 'snowglass-cli-launch', options: {options} }}, '*');"
        );
        if let Err(error) = self.foreground_webview.evaluate_script(&script) {
            eprintln!("snowglass: delivering CLI launch to frontend failed: {error}");
            self.pending_cli_launch = serde_json::from_str(&options).ok();
        }
    }

    fn launch_game(&mut self, instance_id: String, options: LaunchOptions) {
        if self.active_game.is_some() {
            self.emit_instance_event(
                &instance_id,
                "error",
                &serde_json::json!({ "message": "another GameView instance is active" }),
            );
            self.emit_instance_event(&instance_id, "closed", &serde_json::Value::Null);
            return;
        }
        self.emit_instance_event(&instance_id, "start", &serde_json::Value::Null);
        let provider = if options.gamepad_mapping.is_empty() {
            Ok(None)
        } else {
            let mut mappings = options.gamepad_mapping.iter().collect::<Vec<_>>();
            mappings.sort_by_key(|mapping| mapping.slot);
            self._input
                .start_provider(Sdl3ProviderConfig {
                    slots: mappings
                        .into_iter()
                        .map(|mapping| SlotConfig {
                            selector: mapping.selector(),
                            layout_id: mapping.layout.clone(),
                            force_mouse: mapping.mouse,
                        })
                        .collect(),
                    stone_database: None,
                    gamecontroller_database: None,
                    keyboard_mapping_directory: None,
                })
                .map(Some)
        };
        let provider = match provider {
            Ok(provider) => provider,
            Err(error) => {
                self.emit_instance_event(
                    &instance_id,
                    "error",
                    &serde_json::json!({ "message": error }),
                );
                self.emit_instance_event(&instance_id, "closed", &serde_json::Value::Null);
                return;
            }
        };
        if let Some(provider) = &provider {
            for mapping in &options.gamepad_mapping {
                for (target, source) in &mapping.mapping {
                    match parse_input_binding(source) {
                        Ok(binding) => provider.set_binding(mapping.slot, target.clone(), binding),
                        Err(error) => {
                            self.emit_instance_event(
                                &instance_id,
                                "error",
                                &serde_json::json!({ "message": error }),
                            );
                            self.emit_instance_event(
                                &instance_id,
                                "closed",
                                &serde_json::Value::Null,
                            );
                            return;
                        }
                    }
                }
            }
        }
        let webrtc_targets = options
            .surfaces
            .iter()
            .filter(|surface| surface.webrtc)
            .map(|surface| surface.index)
            .collect::<Vec<_>>();
        let publisher = if webrtc_targets.is_empty() {
            None
        } else {
            let bind = match options.webrtc_bind() {
                Ok(bind) => bind,
                Err(error) => {
                    self.emit_instance_event(
                        &instance_id,
                        "error",
                        &serde_json::json!({ "message": error }),
                    );
                    self.emit_instance_event(&instance_id, "closed", &serde_json::Value::Null);
                    return;
                }
            };
            match webrtc::Publisher::start(bind, &webrtc_targets) {
                Ok(publisher) => Some(publisher),
                Err(error) => {
                    self.emit_instance_event(
                        &instance_id,
                        "error",
                        &serde_json::json!({ "message": error.to_string() }),
                    );
                    self.emit_instance_event(&instance_id, "closed", &serde_json::Value::Null);
                    return;
                }
            }
        };
        match launch::launch(&options, &self.session) {
            Ok(target) => {
                self.target = Some(target);
                self.input_provider = provider;
                self.active_game = Some(ActiveGame {
                    instance_id: instance_id.clone(),
                    options,
                    webrtc: publisher,
                });
                self.set_input_priority(InputPriority::Game);
                self.emit_instance_event(&instance_id, "ready", &serde_json::Value::Null);
            }
            Err(error) => {
                self.emit_instance_event(
                    &instance_id,
                    "error",
                    &serde_json::json!({ "message": error.to_string() }),
                );
                self.emit_instance_event(&instance_id, "closed", &serde_json::Value::Null);
            }
        }
    }

    fn close_game(&mut self, instance_id: &str) {
        if self
            .active_game
            .as_ref()
            .is_none_or(|active| active.instance_id != instance_id)
        {
            return;
        }
        if let Some(target) = self.target.take() {
            let _ = target.terminate_tree_and_wait();
        }
        self.finish_active_game();
    }

    fn finish_active_game(&mut self) {
        let Some(active) = self.active_game.take() else {
            return;
        };
        let connections = self.surfaces.keys().copied().collect::<Vec<_>>();
        for connection in connections {
            self.cancel_pointer_connection(connection);
            if let Some(publisher) = &active.webrtc {
                publisher.remove_stream(connection);
            }
            if let Some(mut surface) = self.surfaces.remove(&connection) {
                self.capture.revoke(connection);
                let _ = disconnect_from(&mut surface.compositor, &mut self.merged, connection);
                let _ = self.composition.remove_render_target(connection);
                self.capture.forget(connection);
            }
        }
        let _ = self.composition.commit();
        self.input_provider = None;
        self.set_input_priority(InputPriority::Foreground);
        self.foreground_hit_regions.clear();
        self.publish_targets_for(&active.instance_id);
        self.emit_instance_event(&active.instance_id, "closed", &serde_json::Value::Null);
    }

    fn game_message(&self, instance_id: &str, name: &str, args: serde_json::Value) {
        if self
            .active_game
            .as_ref()
            .is_some_and(|active| active.instance_id == instance_id)
        {
            self.emit_instance_event(
                instance_id,
                "message",
                &serde_json::json!({ "name": name, "args": args }),
            );
        }
    }

    fn emit_instance_event(
        &self,
        instance_id: &str,
        event_name: &str,
        payload: &serde_json::Value,
    ) {
        let event = serde_json::json!({
            "instanceId": instance_id,
            "eventName": event_name,
            "payload": payload,
        });
        let script = format!("window.__snowglassGameViewNativeEvent?.({event});");
        if let Err(error) = self.foreground_webview.evaluate_script(&script) {
            eprintln!("snowglass: publishing GameView event failed: {error}");
        }
    }

    fn process_capture(&mut self) {
        // Leave the notification latched while draining. Producers can mark
        // `capture_dirty`, but cannot flood the event loop with one user event
        // per captured frame.
        self.capture_dirty.store(false, Ordering::Release);
        let mut changed = false;
        let mut processed = 0;
        while processed < MAX_CAPTURE_EVENTS_PER_TURN {
            let Ok(event) = self.capture.try_recv() else {
                break;
            };
            processed += 1;
            match event {
                CaptureEvent::Stream(connection, stream, accepted) => {
                    // The background page is not a guest surface: it connects
                    // before any game is launched and occupies no tracked
                    // target, so neither gate below applies to it.
                    if !is_webview_producer(stream.producer_process_id) {
                        let Some(active) = &self.active_game else {
                            let _ = accepted.send(false);
                            continue;
                        };
                        let tracked = self
                            .surfaces
                            .values()
                            .filter(|surface| surface.index != BACKGROUND_TARGET_INDEX)
                            .count();
                        if tracked >= active.options.surfaces.len() {
                            let _ = accepted.send(false);
                            continue;
                        }
                    }
                    match self.connect_stream(connection, &stream) {
                        Ok(()) => {
                            let _ = accepted.send(true);
                            changed = true;
                        }
                        Err(error) => {
                            eprintln!("snowglass: rejected capture stream {connection}: {error}");
                            let _ = accepted.send(false);
                        }
                    }
                }
                CaptureEvent::Frame(connection, frame) => {
                    let Some(surface) = self.surfaces.get_mut(&connection) else {
                        continue;
                    };
                    self.overlay.observe_frame(connection, surface.index, frame);
                    let (width, height) = rotated_extent(
                        frame.render_width.max(1),
                        frame.render_height.max(1),
                        surface.quarter_turns,
                    );
                    if (surface.preferred_width, surface.preferred_height) != (width, height) {
                        surface.preferred_width = width;
                        surface.preferred_height = height;
                        changed = true;
                    }
                    match (surface.compositor.as_mut(), self.merged.as_mut()) {
                        (Some(compositor), _) => {
                            Renderer::set_latest(compositor, connection, frame);
                        }
                        (None, Some(merged)) => Renderer::set_latest(merged, connection, frame),
                        (None, None) => {}
                    }
                }
                CaptureEvent::Disconnected(connection) => {
                    self.overlay.remove_target(connection);
                    self.cancel_pointer_connection(connection);
                    if let Some(publisher) = self
                        .active_game
                        .as_ref()
                        .and_then(|active| active.webrtc.as_ref())
                    {
                        publisher.remove_stream(connection);
                    }
                    if let Some(mut surface) = self.surfaces.remove(&connection) {
                        let _ =
                            disconnect_from(&mut surface.compositor, &mut self.merged, connection);
                        if let Err(error) = self.composition.remove_render_target(connection) {
                            eprintln!(
                                "snowglass: removing composition surface {connection} failed: {error}"
                            );
                        }
                        changed = true;
                    }
                    self.capture.forget(connection);
                }
            }
        }
        if changed
            && let Some(instance_id) = self
                .active_game
                .as_ref()
                .map(|active| active.instance_id.clone())
        {
            self.publish_targets_for(&instance_id);
        }
        self.draw_pending_surfaces();
        self.capture_wake_pending.store(false, Ordering::Release);

        // A producer that ran while the notification was latched could not
        // enqueue another WakeCapture. Carry that work into the next event-loop
        // cycle instead. Resetting the latch before reading `capture_dirty`
        // closes the race: a later producer will enqueue a fresh notification.
        let arrived_while_draining = self.capture_dirty.swap(false, Ordering::AcqRel);
        self.capture_work_pending =
            processed == MAX_CAPTURE_EVENTS_PER_TURN || arrived_while_draining;
    }

    fn draw_pending_surfaces(&mut self) {
        if self.merged.is_some() {
            self.draw_merged();
            return;
        }
        let publisher = self
            .active_game
            .as_ref()
            .and_then(|active| active.webrtc.as_ref());
        for (&connection, surface) in &mut self.surfaces {
            let ready = surface.compositor.as_mut().is_some_and(|compositor| {
                (Renderer::has_pending_frame(compositor) || surface.needs_redraw)
                    && compositor.try_begin_frame()
            });
            if !ready {
                continue;
            }
            let plan = surface.composition_plan();
            let compositor = surface
                .compositor
                .as_mut()
                .expect("a dedicated compositor was just checked");
            match compositor.draw_game_with_webrtc(&plan, surface.webrtc.as_mut()) {
                Ok(_) => surface.needs_redraw = false,
                Err(error) => {
                    eprintln!("snowglass: rendering stream {connection} failed: {error}");
                }
            }
            if let (Some(publisher), Some(video)) = (publisher, surface.webrtc.as_mut()) {
                for frame in video.take_frames() {
                    publisher.publish(frame);
                }
            }
        }
    }

    fn has_pending_surfaces(&self) -> bool {
        if let Some(merged) = self.merged.as_ref() {
            return self.merged_presents_every_tick
                || Renderer::has_pending_frame(merged)
                || self.surfaces.values().any(|surface| surface.needs_redraw);
        }
        self.surfaces.values().any(|surface| {
            surface.needs_redraw
                || surface
                    .compositor
                    .as_ref()
                    .is_some_and(Renderer::has_pending_frame)
        })
    }

    fn connect_stream(
        &mut self,
        connection: u64,
        stream: &snowglass_ipc::StreamReady,
    ) -> Result<(), Box<dyn Error>> {
        // The background page publishes through the same producer path as a
        // guest, so it arrives here like any other stream and has to be told
        // apart before it consumes a tracked render target.
        if is_webview_producer(stream.producer_process_id) {
            return self.connect_background_stream(connection, stream);
        }
        let (index, quarter_turns, publish_webrtc) = self
            .next_target_configuration()
            .ok_or("the shell already has a surface for every tracked render target")?;
        let (width, height) = rotated_extent(stream.width, stream.height, quarter_turns);
        // Merged compositing normally depends on the captured background to
        // fill the pixels no stream covers. It can be enabled without one to
        // separate the cost of merging from the cost of hosting the capture.
        if self.background_host.is_some() || self.merged_without_background {
            return self.connect_merged_stream(connection, stream, index, quarter_turns, width, height);
        }
        let render_window = SurfaceWindow::new(&self.window, width.max(1), height.max(1));
        let (mut compositor, mut video) = if publish_webrtc {
            let (compositor, video) = D3d12Compositor::new_for_composition_stream_with_webrtc(
                &render_window,
                stream.adapter_luid,
                connection,
            )?;
            (compositor, Some(video))
        } else {
            (
                D3d12Compositor::new_for_composition_stream(
                    &render_window,
                    stream.adapter_luid,
                    connection,
                )?,
                None,
            )
        };
        Renderer::import_stream(&mut compositor, connection, stream, quarter_turns)?;
        if let Some(video) = &mut video {
            video.import_d3d12_stream(connection)?;
        }
        let content = compositor.composition_content()?;
        self.composition
            .add_render_target(connection, index, &content)?;
        self.composition.commit()?;
        if publish_webrtc
            && let Some(publisher) = self
                .active_game
                .as_ref()
                .and_then(|active| active.webrtc.as_ref())
        {
            publisher.add_stream(connection, index);
        }
        self.overlay.set_backend(format!("{:?}", stream.image_kind));
        self.surfaces.insert(
            connection,
            CapturedSurface {
                index,
                preferred_width: width,
                preferred_height: height,
                swapchain_width: width.max(1),
                swapchain_height: height.max(1),
                viewport_width: width.max(1),
                viewport_height: height.max(1),
                viewport_x: 0,
                viewport_y: 0,
                quarter_turns,
                pointer_events: true,
                z_index: index as i32,
                needs_redraw: false,
                resize_due: None,
                compositor: Some(compositor),
                webrtc: video,
            },
        );
        Ok(())
    }

    /// Adds a stream to the single window-spanning swapchain that every stream
    /// shares, creating it on the first connection.
    fn connect_merged_stream(
        &mut self,
        connection: u64,
        stream: &snowglass_ipc::StreamReady,
        index: u32,
        quarter_turns: u8,
        width: u32,
        height: u32,
    ) -> Result<(), Box<dyn Error>> {
        let size = self.window.inner_size();
        let (swapchain_width, swapchain_height) = (size.width.max(1), size.height.max(1));
        if self.merged.is_none() {
            let render_window =
                SurfaceWindow::new(&self.window, swapchain_width, swapchain_height);
            self.merged = Some(D3d12Compositor::new_for_shared_composition(
                &render_window,
                stream.adapter_luid,
            )?);
        }
        let merged = self
            .merged
            .as_mut()
            .expect("the shared compositor exists by now");
        Renderer::import_stream(merged, connection, stream, quarter_turns)?;
        if !self.merged_attached {
            let content = merged.composition_content()?;
            self.composition
                .add_render_target(MERGED_VISUAL_KEY, 0, &content)?;
            self.composition.commit()?;
            self.merged_attached = true;
        }
        self.overlay.set_backend(format!("{:?}", stream.image_kind));
        self.surfaces.insert(
            connection,
            CapturedSurface {
                index,
                preferred_width: width,
                preferred_height: height,
                swapchain_width,
                swapchain_height,
                viewport_width: width.max(1),
                viewport_height: height.max(1),
                viewport_x: 0,
                viewport_y: 0,
                quarter_turns,
                pointer_events: true,
                z_index: index as i32,
                needs_redraw: false,
                resize_due: None,
                compositor: None,
                webrtc: None,
            },
        );
        Ok(())
    }

    /// Adopts the captured background page as the lowest surface in the shared
    /// swapchain. It is an ordinary placement rather than a special case: given
    /// the lowest `z_index` and the whole window as its rectangle, the existing
    /// ordering paints it under every stream and it fills the pixels no stream
    /// covers, which is what lets the swapchain stay opaque.
    fn connect_background_stream(
        &mut self,
        connection: u64,
        stream: &snowglass_ipc::StreamReady,
    ) -> Result<(), Box<dyn Error>> {
        let size = self.window.inner_size();
        let (swapchain_width, swapchain_height) = (size.width.max(1), size.height.max(1));
        if self.merged.is_none() {
            let render_window =
                SurfaceWindow::new(&self.window, swapchain_width, swapchain_height);
            self.merged = Some(D3d12Compositor::new_for_shared_composition(
                &render_window,
                stream.adapter_luid,
            )?);
        }
        let merged = self
            .merged
            .as_mut()
            .expect("the shared compositor exists by now");
        Renderer::import_stream(merged, connection, stream, 0)?;
        if !self.merged_attached {
            let content = merged.composition_content()?;
            self.composition
                .add_render_target(MERGED_VISUAL_KEY, 0, &content)?;
            self.composition.commit()?;
            self.merged_attached = true;
        }
        self.background_connection = Some(connection);
        self.surfaces.insert(
            connection,
            CapturedSurface {
                index: BACKGROUND_TARGET_INDEX,
                preferred_width: stream.width,
                preferred_height: stream.height,
                swapchain_width,
                swapchain_height,
                viewport_width: swapchain_width,
                viewport_height: swapchain_height,
                viewport_x: 0,
                viewport_y: 0,
                quarter_turns: 0,
                // The page behind the streams is decoration; pointer routing
                // belongs to the foreground WebView2 and the streams.
                pointer_events: false,
                z_index: i32::MIN,
                needs_redraw: true,
                resize_due: None,
                compositor: None,
                webrtc: None,
            },
        );
        eprintln!("snowglass: background page connected as stream {connection}");
        Ok(())
    }

    /// Placements for the shared swapchain, ordered so a higher `z_index` is
    /// painted later and therefore on top.
    fn merged_plan(&self) -> CompositingPlan {
        let Some(merged) = self.merged.as_ref() else {
            return CompositingPlan::new([0.0, 0.0, 0.0, 1.0], Vec::new());
        };
        let mut ordered = Renderer::received_surfaces(merged)
            .into_iter()
            .filter_map(|received| {
                let surface = self.surfaces.get(&received.connection)?;
                Some((
                    surface.z_index,
                    SurfacePlacement {
                        connection: received.connection,
                        surface_id: received.surface_id,
                        rectangle: snowglass_input::mouse::layout::Rect::new(
                            surface.viewport_x,
                            surface.viewport_y,
                            surface.viewport_width,
                            surface.viewport_height,
                        ),
                    },
                ))
            })
            .collect::<Vec<_>>();
        ordered.sort_by_key(|(z_index, _)| *z_index);
        CompositingPlan::new(
            [0.0, 0.0, 0.0, 1.0],
            ordered.into_iter().map(|(_, placement)| placement).collect(),
        )
    }

    /// Draws every stream and the captured background into the shared
    /// swapchain. The background slot is read with a plain atomic load, so a
    /// stalled capture cannot hold a received frame back.
    fn draw_merged(&mut self) {
        // Presenting only when a stream delivers ties both streams to one
        // timeline: a frame that lands just after a present has to wait for the
        // next credit, which is the other stream's problem leaking into this
        // one. Presenting on the compositor tick instead gives each stream the
        // next tick regardless of what the other is doing, at the cost of
        // redrawing content that has not changed.
        let pending = self
            .merged
            .as_ref()
            .is_some_and(Renderer::has_pending_frame)
            || self.surfaces.values().any(|surface| surface.needs_redraw);
        if !pending && !self.merged_presents_every_tick {
            return;
        }
        let plan = self.merged_plan();
        let Some(merged) = self.merged.as_mut() else {
            return;
        };
        if !merged.try_begin_frame() {
            return;
        }
        match merged.draw_game_with_webrtc(&plan, None) {
            Ok(_) => {
                for surface in self.surfaces.values_mut() {
                    surface.needs_redraw = false;
                }
            }
            Err(error) => eprintln!("snowglass: rendering merged surfaces failed: {error}"),
        }
    }

    fn next_target_configuration(&self) -> Option<(u32, u8, bool)> {
        let in_use = |index| self.surfaces.values().any(|surface| surface.index == index);
        self.active_game
            .as_ref()?
            .options
            .surfaces
            .iter()
            .find(|target| !in_use(target.index))
            .map(|target| (target.index, target.quarter_turns(), target.webrtc))
    }

    fn apply_layout(&mut self, layouts: Vec<SurfaceLayout>) {
        self.foreground_hit_regions = layouts
            .iter()
            .flat_map(|layout| {
                layout
                    .foreground_hit_region
                    .iter()
                    .chain(&layout.foreground_hit_regions)
                    .copied()
            })
            .filter(|region| region.width != 0 && region.height != 0)
            .collect();
        let resize_due = Instant::now() + SURFACE_RESIZE_SETTLE;
        for item in layouts {
            if let Err(error) = self
                .composition
                .set_render_target_z_index(item.connection, item.z_index)
            {
                eprintln!(
                    "snowglass: reordering composition surface {} failed: {error}",
                    item.connection
                );
            }
            let Some(surface) = self.surfaces.get_mut(&item.connection) else {
                continue;
            };
            surface.viewport_x = item.x;
            surface.viewport_y = item.y;
            surface.viewport_width = item.width;
            surface.viewport_height = item.height;
            surface.pointer_events = item.pointer_events;
            surface.z_index = item.z_index;
            if let Err(error) = self.composition.apply_layout(
                &item,
                surface.swapchain_width,
                surface.swapchain_height,
            ) {
                eprintln!(
                    "snowglass: positioning composition surface {} failed: {error}",
                    item.connection
                );
            }
            surface.resize_due = (item.width != 0
                && item.height != 0
                && (surface.swapchain_width, surface.swapchain_height)
                    != (item.width, item.height))
                .then_some(resize_due);
        }
        if let Err(error) = self.composition.commit() {
            eprintln!("snowglass: committing WebView2 layout failed: {error}");
        }
    }

    fn foreground_hit_test(&self) -> bool {
        let x = i64::from(self.cursor.x);
        let y = i64::from(self.cursor.y);
        self.foreground_hit_regions.iter().any(|region| {
            let left = i64::from(region.x);
            let top = i64::from(region.y);
            x >= left
                && x < left + i64::from(region.width)
                && y >= top
                && y < top + i64::from(region.height)
        })
    }

    fn resize_pending_surfaces(&mut self, now: Instant) -> Option<Instant> {
        let mut next_due: Option<Instant> = None;
        let mut resized = Vec::new();
        for (&connection, surface) in &mut self.surfaces {
            let Some(due) = surface.resize_due else {
                continue;
            };
            if due > now {
                next_due = Some(next_due.map_or(due, |current| current.min(due)));
                continue;
            }
            surface.resize_due = None;
            let render_window = SurfaceWindow::new(
                &self.window,
                surface.viewport_width,
                surface.viewport_height,
            );
            let Some(compositor) = surface.compositor.as_mut() else {
                // A shared swapchain tracks the window, not the DOM box, so a
                // layout change only needs a redraw.
                surface.needs_redraw = true;
                continue;
            };
            match Renderer::recreate_swapchain(compositor, &render_window) {
                Ok(true) => {
                    surface.swapchain_width = surface.viewport_width;
                    surface.swapchain_height = surface.viewport_height;
                    surface.needs_redraw = true;
                    resized.push((
                        SurfaceLayout {
                            connection,
                            x: surface.viewport_x,
                            y: surface.viewport_y,
                            width: surface.viewport_width,
                            height: surface.viewport_height,
                            z_index: surface.z_index,
                            pointer_events: surface.pointer_events,
                            foreground_hit_region: None,
                            foreground_hit_regions: Vec::new(),
                        },
                        (surface.swapchain_width, surface.swapchain_height),
                    ));
                }
                Ok(false) => {}
                Err(error) => eprintln!(
                    "snowglass: resizing composition surface {connection} failed: {error}"
                ),
            }
        }
        let composition_changed = !resized.is_empty();
        for (layout, extent) in resized {
            if let Err(error) = self.composition.apply_layout(&layout, extent.0, extent.1) {
                eprintln!(
                    "snowglass: updating composition surface {} after resize failed: {error}",
                    layout.connection
                );
            }
        }
        if composition_changed {
            if let Err(error) = self.composition.commit() {
                eprintln!("snowglass: committing resized composition surfaces failed: {error}");
            }
            self.draw_pending_surfaces();
        }
        next_due
    }

    fn pointer_presentation(&self) -> (snowglass_renderer::Presentation, CompositingPlan) {
        let mut received = Vec::new();
        let mut placements = Vec::new();
        let mut surfaces = self
            .surfaces
            .iter()
            .filter(|(_, surface)| surface.pointer_events)
            .collect::<Vec<_>>();
        surfaces.sort_by_key(|(_, surface)| std::cmp::Reverse(surface.z_index));
        for (&connection, surface) in surfaces {
            let received_surfaces = match (surface.compositor.as_ref(), self.merged.as_ref()) {
                (Some(compositor), _) => Renderer::received_surfaces(compositor),
                (None, Some(merged)) => Renderer::received_surfaces(merged)
                    .into_iter()
                    .filter(|received| received.connection == connection)
                    .collect(),
                (None, None) => Vec::new(),
            };
            for mut received_surface in received_surfaces {
                received_surface.frame.presentation_transform = shell_pointer_transform(
                    received_surface.frame.presentation_transform,
                    received_surface.frame.presentation_width,
                    received_surface.frame.presentation_height,
                );
                placements.push(SurfacePlacement {
                    connection,
                    surface_id: received_surface.surface_id,
                    rectangle: Rect::new(
                        surface.viewport_x,
                        surface.viewport_y,
                        surface.viewport_width,
                        surface.viewport_height,
                    ),
                });
                received.push(received_surface);
            }
        }
        let size = self.window.inner_size();
        (
            snowglass_renderer::Presentation::new(Extent::new(size.width, size.height), received),
            CompositingPlan::new([0.0; 4], placements),
        )
    }

    fn route_pointer(&mut self, phase: PointerPhase, button: u32, horizontal: i32, vertical: i32) {
        let (presentation, plan) = self.pointer_presentation();
        if !self.pointer_routing.send(
            phase,
            button,
            horizontal,
            vertical,
            &presentation,
            &plan,
            &self._input.control_sender(),
        ) {
            if phase == PointerPhase::Move {
                self.direct_input.reset_mouse_position();
                if let Some(provider) = &self.input_provider {
                    provider.set_pointer(None);
                }
            }
            return;
        }
        let Some(sent) = self.pointer_routing.last_sent() else {
            return;
        };
        match phase {
            PointerPhase::Move => {
                self.direct_input.mouse_position(
                    sent.surface_id,
                    sent.x,
                    sent.y,
                    sent.coordinate_width,
                    sent.coordinate_height,
                );
                if let Some(provider) = &self.input_provider {
                    provider.set_pointer(Some(pointer_axes(sent.normalized_x, sent.normalized_y)));
                }
            }
            PointerPhase::ButtonDown | PointerPhase::ButtonUp => self
                .direct_input
                .mouse_button(button as u16, phase == PointerPhase::ButtonDown),
            PointerPhase::Wheel => self.direct_input.mouse_wheel(vertical),
            PointerPhase::Cancel => {}
        }
    }

    fn set_input_priority(&mut self, priority: InputPriority) {
        let priority = if priority == InputPriority::Game && self.active_game.is_none() {
            InputPriority::Foreground
        } else {
            priority
        };
        if self.input_priority == priority {
            return;
        }
        if let Some(connection) = self.pointer_routing.captured_connection() {
            self.cancel_pointer_connection(connection);
        }
        self.direct_input.reset_mouse_position();
        if let Some(provider) = &self.input_provider {
            provider.clear_keys();
            provider.set_pointer(None);
            provider.set_enabled(priority == InputPriority::Game);
        }
        self.input_priority = priority;
        match priority {
            InputPriority::Foreground => {
                // SAFETY: the controller is live while the WebView is retained.
                let _ = unsafe {
                    self.foreground_webview
                        .controller()
                        .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
                };
            }
            InputPriority::Game => {
                if let Ok(handle) = self.window.window_handle()
                    && let RawWindowHandle::Win32(handle) = handle.as_raw()
                {
                    // SAFETY: the winit HWND remains live for ShellState.
                    let _ = unsafe {
                        SetFocus(Some(windows::Win32::Foundation::HWND(
                            handle.hwnd.get() as *mut _
                        )))
                    };
                }
            }
        }
        let script = format!(
            "window.__snowglassSetInputPriority?.({});",
            serde_json::to_string(match priority {
                InputPriority::Game => "game",
                InputPriority::Foreground => "foreground",
            })
            .expect("static input priority serializes")
        );
        let _ = self.foreground_webview.evaluate_script(&script);
    }

    fn cancel_pointer_connection(&mut self, connection: u64) {
        let (presentation, plan) = self.pointer_presentation();
        if let Some(buttons) = self.pointer_routing.cancel(
            connection,
            &presentation,
            &plan,
            &self._input.control_sender(),
        ) {
            self.direct_input.release_mouse_buttons(buttons);
        }
    }

    fn route_foreground_mouse(&mut self, kind: u32, button: Option<(u16, bool)>, mouse_data: u32) {
        if let Some((button, pressed)) = button {
            let flag = match button {
                1 => DOM_MK_LBUTTON,
                2 => DOM_MK_RBUTTON,
                3 => DOM_MK_MBUTTON,
                4 => DOM_MK_XBUTTON1,
                5 => DOM_MK_XBUTTON2,
                _ => 0,
            };
            if pressed {
                self.dom_mouse_keys |= flag;
            } else {
                self.dom_mouse_keys &= !flag;
            }
            if pressed {
                let _ = unsafe {
                    self.foreground_webview
                        .controller()
                        .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
                };
            }
        }
        let Ok(controller) = self
            .foreground_webview
            .controller()
            .cast::<ICoreWebView2CompositionController>()
        else {
            return;
        };
        // SAFETY: coordinates are physical client pixels for this controller.
        let _ = unsafe {
            controller.SendMouseInput(
                COREWEBVIEW2_MOUSE_EVENT_KIND(kind as i32),
                COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS(self.dom_mouse_keys as i32),
                mouse_data,
                self.cursor,
            )
        };
    }

    fn publish_input_state(&mut self) {
        if Instant::now() < self.next_input_publish {
            return;
        }
        self.next_input_publish = Instant::now() + Duration::from_millis(16);
        let states = self
            .input_provider
            .as_ref()
            .map(Sdl3Provider::states)
            .unwrap_or_default();
        let dispatch = self._input.dispatch_summary();
        self.overlay.diagnostics.input = InputDiagnostics {
            active: self.input_priority == InputPriority::Game && self.target.is_some(),
            dispatch_samples: dispatch.samples,
            dispatch_p50_ms: dispatch.p50_ms,
            dispatch_p95_ms: dispatch.p95_ms,
            dispatch_p99_ms: dispatch.p99_ms,
            slots: self
                .active_game
                .as_ref()
                .map(|active| active.options.gamepad_mapping.as_slice())
                .unwrap_or_default()
                .iter()
                .map(|config| {
                    let Some(state) = states.get(config.slot).and_then(Option::as_ref) else {
                        return InputSlotDiagnostics {
                            slot: config.slot,
                            layout: config.layout.clone(),
                            ..InputSlotDiagnostics::default()
                        };
                    };
                    InputSlotDiagnostics {
                        slot: config.slot,
                        layout: state.layout_id.clone(),
                        connected: true,
                        name: state.name.clone(),
                        guid: state.guid.clone(),
                        pressed: state
                            .buttons
                            .iter()
                            .filter(|(_, pressed)| *pressed)
                            .map(|(button, _)| format!("{button:?}"))
                            .collect(),
                        axes: state
                            .axes
                            .iter()
                            .map(|(axis, value)| InputAxisDiagnostics {
                                name: format!("{axis:?}"),
                                value: *value,
                            })
                            .collect(),
                        mapped_buttons: state.mapped.buttons,
                        left_stick: state.mapped.left_stick,
                        right_stick: state.mapped.right_stick,
                        left_trigger: state.mapped.left_trigger,
                        right_trigger: state.mapped.right_trigger,
                        pointer: state.mapped.pointer,
                        accelerometer: state.mapped.accelerometer,
                        gyroscope: state.mapped.gyroscope,
                    }
                })
                .collect(),
        };
        let mut devices = Vec::new();
        let mut next_controls = HashMap::new();
        let foreground_priority = self.input_priority == InputPriority::Foreground;
        for (slot, state) in states.into_iter().enumerate() {
            let Some(state) = state else {
                continue;
            };
            let buttons = state
                .buttons
                .iter()
                .map(|&(button, pressed)| {
                    let name = format!("{button:?}");
                    let consumed = state.mapped_buttons.contains(&button);
                    if foreground_priority || !consumed {
                        next_controls.insert(format!("{slot}:button:{name}"), i32::from(pressed));
                    }
                    serde_json::json!({
                        "button": name,
                        "pressed": pressed,
                        "mapped": consumed,
                    })
                })
                .collect::<Vec<_>>();
            let axes = state
                .axes
                .iter()
                .map(|&(axis, value)| {
                    let name = format!("{axis:?}");
                    let consumed = state.mapped_axes.contains(&axis);
                    if foreground_priority || !consumed {
                        next_controls.insert(format!("{slot}:axis:{name}"), i32::from(value));
                    }
                    serde_json::json!({
                        "axis": name,
                        "value": value,
                        "mapped": consumed,
                    })
                })
                .collect::<Vec<_>>();
            devices.push(serde_json::json!({
                "slot": slot,
                "name": state.name,
                "guid": state.guid,
                "layout": state.layout_id,
                "buttons": buttons,
                "axes": axes,
                "mapped": {
                    "buttons": state.mapped.buttons,
                    "leftStick": state.mapped.left_stick,
                    "rightStick": state.mapped.right_stick,
                    "leftTrigger": state.mapped.left_trigger,
                    "rightTrigger": state.mapped.right_trigger,
                    "pointer": state.mapped.pointer,
                    "accelerometer": state.mapped.accelerometer,
                    "gyroscope": state.mapped.gyroscope,
                }
            }));
        }
        let mut routed = Vec::new();
        for (key, &value) in &next_controls {
            if self.ui_controls.get(key).copied().unwrap_or_default() != value {
                routed.push(control_event(key, value));
            }
        }
        for (key, &value) in &self.ui_controls {
            if value != 0 && !next_controls.contains_key(key) {
                routed.push(control_event(key, 0));
            }
        }
        self.ui_controls = next_controls;
        let Ok(json) = serde_json::to_string(&devices) else {
            return;
        };
        let state_changed = json != self.last_input_state;
        if !state_changed && routed.is_empty() {
            return;
        }
        self.overlay.next_repaint = Instant::now();
        if state_changed {
            self.last_input_state.clone_from(&json);
        }
        let routed = serde_json::to_string(&routed).unwrap_or_else(|_| "[]".to_owned());
        let script = format!("window.__snowglassSetInputState?.({json}, {routed});");
        let _ = self.foreground_webview.evaluate_script(&script);
    }

    fn forward_background_message(&self, json: &str) {
        let script = format!("window.__snowglassBackgroundMessage?.({json});");
        // A desktop-hosted page lives on another thread, and WebView2 is
        // apartment-threaded, so the script is queued for that thread rather
        // than called across it.
        if let Some(host) = self.background_host.as_ref() {
            host.evaluate(script);
            return;
        }
        if let Some(background) = self.background_webview.as_ref()
            && let Err(error) = background.evaluate_script(&script)
        {
            eprintln!("snowglass: forwarding background message failed: {error}");
        }
    }

    fn publish_keyboard_input(&self, key: &str, pressed: bool, repeat: bool) {
        let event = serde_json::json!({
            "device": "keyboard",
            "controlType": "key",
            "control": key,
            "value": i32::from(pressed),
            "pressed": pressed,
            "repeat": repeat,
        });
        let script = format!("window.__snowglassKeyboardInput?.({event});");
        let _ = self.foreground_webview.evaluate_script(&script);
    }

    fn publish_targets_for(&self, instance_id: &str) {
        let mut targets = self
            .surfaces
            .iter()
            .map(|(&connection, surface)| WebRenderTarget {
                connection: connection.to_string(),
                index: surface.index,
                width: surface.preferred_width,
                height: surface.preferred_height,
            })
            .collect::<Vec<_>>();
        targets.sort_by_key(|target| target.index);
        let Ok(payload) = serde_json::to_value(&targets) else {
            return;
        };
        self.emit_instance_event(instance_id, "surfacesChanged", &payload);
    }

    fn poll_target(&mut self) {
        let Some(target) = self.target.as_mut() else {
            return;
        };
        let finished = match target.try_wait() {
            Ok(Some(())) => {
                self.target = None;
                true
            }
            Ok(None) => false,
            Err(error) => {
                eprintln!("snowglass: target process wait failed: {error}");
                self.target = None;
                true
            }
        };
        if finished {
            self.finish_active_game();
        }
    }
}

impl Drop for ShellState {
    fn drop(&mut self) {
        for (&connection, surface) in &mut self.surfaces {
            self.capture.revoke(connection);
            let _ = disconnect_from(&mut surface.compositor, &mut self.merged, connection);
        }
        if let Some(target) = self.target.take() {
            let _ = target.terminate_tree_and_wait();
        }
    }
}

struct ShellApplication {
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    options: Option<ShellOptions>,
    state: Option<ShellState>,
    error: Option<Box<dyn Error>>,
}

impl ApplicationHandler<UserEvent> for ShellApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() || self.error.is_some() {
            return;
        }
        let options = self
            .options
            .take()
            .expect("shell options are consumed exactly once");
        match ShellState::new(event_loop, self.proxy.clone(), options) {
            Ok(state) => self.state = Some(state),
            Err(error) => {
                self.error = Some(error);
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            // Capture is drained from `about_to_wait`, after this cycle's
            // native window messages have been delivered.
            UserEvent::WakeCapture => state.capture_work_pending = true,
            // Waking the loop is the whole job: `about_to_wait` draws whatever
            // is pending once this cycle's messages have been delivered.
            UserEvent::CompositorTick => state.compositor_clock.clear_tick(),
            UserEvent::FrontendReady => state.deliver_cli_launch(),
            UserEvent::Launch {
                instance_id,
                options,
            } => state.launch_game(instance_id, options),
            UserEvent::CloseInstance(instance_id) => state.close_game(&instance_id),
            UserEvent::GameMessage {
                instance_id,
                name,
                args,
            } => state.game_message(&instance_id, &name, args),
            UserEvent::Layout(layouts) => state.apply_layout(layouts),
            UserEvent::InputPriority(priority) => state.set_input_priority(priority),
            UserEvent::BackgroundMessage(message) => state.forward_background_message(&message),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if window_id != state.window.id() {
            return;
        }
        let response = state.overlay.state.on_window_event(&state.window, &event);
        let consumed = response.consumed;
        if response.repaint {
            state.window.request_redraw();
        }
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => event_loop.exit(),
            WindowEvent::Resized(size) if size.width != 0 && size.height != 0 => {
                let bounds = wry::Rect {
                    position: wry::dpi::PhysicalPosition::new(0, 0).into(),
                    size: wry::dpi::PhysicalSize::new(size.width, size.height).into(),
                };
                if let Some(background) = state.background_webview.as_ref()
                    && let Err(error) = background.set_bounds(bounds)
                {
                    eprintln!("snowglass: resizing background WebView2 failed: {error}");
                }
                if let Err(error) = state.foreground_webview.set_bounds(bounds) {
                    eprintln!("snowglass: resizing foreground WebView2 failed: {error}");
                }
                if let Err(error) = state.overlay.resize(&state.window) {
                    eprintln!("snowglass: resizing egui composition overlay failed: {error}");
                }
                // The shared swapchain and the background both cover the whole
                // window, so the window is what resizes them, not a DOM box.
                if let Some(merged) = state.merged.as_mut() {
                    let render_window = SurfaceWindow::new(&state.window, size.width, size.height);
                    match Renderer::recreate_swapchain(merged, &render_window) {
                        Ok(true) => {
                            for surface in state.surfaces.values_mut() {
                                surface.swapchain_width = size.width;
                                surface.swapchain_height = size.height;
                                surface.needs_redraw = true;
                            }
                        }
                        Ok(false) => {}
                        Err(error) => {
                            eprintln!("snowglass: resizing the shared swapchain failed: {error}");
                        }
                    }
                }
                if let Some(connection) = state.background_connection
                    && let Some(background) = state.surfaces.get_mut(&connection)
                {
                    background.viewport_width = size.width.max(1);
                    background.viewport_height = size.height.max(1);
                    background.needs_redraw = true;
                }
                state.window.request_redraw();
            }
            WindowEvent::Focused(false) => {
                if let Some(connection) = state.pointer_routing.captured_connection() {
                    state.cancel_pointer_connection(connection);
                }
                if let Some(provider) = &state.input_provider {
                    provider.clear_keys();
                    provider.set_pointer(None);
                }
                state.dom_mouse_keys &= DOM_MK_SHIFT | DOM_MK_CONTROL;
                state.foreground_mouse_capture = false;
                state.direct_input.reset_mouse_position();
            }
            WindowEvent::KeyboardInput { event, .. }
                if state.input_priority == InputPriority::Game =>
            {
                if let winit::keyboard::PhysicalKey::Code(code) = event.physical_key {
                    let key = format!("{code:?}");
                    let pressed = event.state == ElementState::Pressed;
                    if let Some(provider) = &state.input_provider
                        && provider.is_key_mapped(&key)
                    {
                        provider.set_key(key, pressed);
                    } else if !consumed {
                        state.publish_keyboard_input(&key, pressed, event.repeat);
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self::set_modifier(
                    &mut state.dom_mouse_keys,
                    DOM_MK_SHIFT,
                    modifiers.state().shift_key(),
                );
                self::set_modifier(
                    &mut state.dom_mouse_keys,
                    DOM_MK_CONTROL,
                    modifiers.state().control_key(),
                );
            }
            WindowEvent::CursorMoved { position, .. }
                if !consumed
                    || state.pointer_routing.captured_connection().is_some()
                    || state.dom_mouse_keys & DOM_MOUSE_BUTTONS != 0
                    || state.foreground_mouse_capture =>
            {
                state.cursor = POINT {
                    x: position.x.round() as i32,
                    y: position.y.round() as i32,
                };
                if state.input_priority == InputPriority::Foreground
                    || state.foreground_mouse_capture
                    || (!consumed
                        && state.pointer_routing.captured_connection().is_none()
                        && state.foreground_hit_test())
                {
                    state.route_foreground_mouse(WM_MOUSEMOVE, None, 0);
                    return;
                }
                state.pointer_routing.set_cursor(position.x, position.y);
                state.route_pointer(PointerPhase::Move, 0, 0, 0);
            }
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } if !consumed
                || state.pointer_routing.captured_connection().is_some()
                || state.dom_mouse_keys & DOM_MOUSE_BUTTONS != 0
                || state.foreground_mouse_capture =>
            {
                if let Some(button) = mouse_button(button) {
                    let foreground_hit = !consumed
                        && state.pointer_routing.captured_connection().is_none()
                        && state.foreground_hit_test();
                    if state.input_priority == InputPriority::Foreground
                        || state.foreground_mouse_capture
                        || foreground_hit
                    {
                        if button_state == ElementState::Pressed && foreground_hit {
                            state.foreground_mouse_capture = true;
                        }
                        let kind = match (button, button_state) {
                            (1, ElementState::Pressed) => WM_LBUTTONDOWN,
                            (1, ElementState::Released) => WM_LBUTTONUP,
                            (2, ElementState::Pressed) => WM_RBUTTONDOWN,
                            (2, ElementState::Released) => WM_RBUTTONUP,
                            (3, ElementState::Pressed) => WM_MBUTTONDOWN,
                            (3, ElementState::Released) => WM_MBUTTONUP,
                            (_, ElementState::Pressed) => WM_XBUTTONDOWN,
                            (_, ElementState::Released) => WM_XBUTTONUP,
                        };
                        state.route_foreground_mouse(
                            kind,
                            Some((button, button_state == ElementState::Pressed)),
                            if button >= 4 {
                                u32::from(button - 3)
                            } else {
                                0
                            },
                        );
                        if button_state == ElementState::Released
                            && state.dom_mouse_keys & DOM_MOUSE_BUTTONS == 0
                        {
                            state.foreground_mouse_capture = false;
                        }
                        return;
                    }
                    state.route_pointer(
                        if button_state == ElementState::Pressed {
                            PointerPhase::ButtonDown
                        } else {
                            PointerPhase::ButtonUp
                        },
                        u32::from(button),
                        0,
                        0,
                    );
                }
            }
            WindowEvent::MouseWheel { delta, .. } if !consumed => {
                let (horizontal, vertical) = match delta {
                    MouseScrollDelta::LineDelta(horizontal, vertical) => {
                        ((horizontal * 120.0) as i32, (vertical * 120.0) as i32)
                    }
                    MouseScrollDelta::PixelDelta(delta) => (delta.x as i32, delta.y as i32),
                };
                if state.input_priority == InputPriority::Foreground
                    || state.foreground_mouse_capture
                    || (state.pointer_routing.captured_connection().is_none()
                        && state.foreground_hit_test())
                {
                    if horizontal != 0 {
                        state.route_foreground_mouse(WM_MOUSEHWHEEL, None, horizontal as u32);
                    }
                    if vertical != 0 {
                        state.route_foreground_mouse(WM_MOUSEWHEEL, None, vertical as u32);
                    }
                    return;
                }
                state.route_pointer(PointerPhase::Wheel, 0, horizontal, vertical);
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = state.overlay.draw(&state.window) {
                    eprintln!("snowglass: drawing egui composition overlay failed: {error}");
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let mut deadline = now + Duration::from_millis(100);
        if let Some(state) = self.state.as_mut() {
            if state.capture_work_pending {
                state.process_capture();
            }
            // A frame the compositor deferred keeps its `latest` slot, so it
            // has to be retried from here. `process_capture` only runs when a
            // producer posts new work, which would hold an already-finished
            // frame back until the guest renders its next one.
            if state.has_pending_surfaces() {
                state.draw_pending_surfaces();
            }
            if let Some(resize_due) = state.resize_pending_surfaces(now) {
                deadline = deadline.min(resize_due);
            }
            state.publish_input_state();
            state.poll_target();
            if state.overlay.repaint_due(now) {
                state.window.request_redraw();
            }
            deadline = deadline.min(state.overlay.next_repaint());
            // Cheap enough to reconcile every turn, which keeps pacing correct
            // no matter which path added or dropped a stream.
            state
                .compositor_clock
                .set_active(!state.surfaces.is_empty());
            // The compositor clock already wakes the loop once per composition
            // frame, which is the earliest a deferred present can be picked up.
            // Only sessions without a usable clock need the timer fallback.
            if state.has_pending_surfaces() && !state.compositor_clock.is_pacing() {
                deadline = deadline.min(now + PENDING_SURFACE_RETRY);
            }
            if state.capture_work_pending {
                deadline = deadline.min(now + PENDING_SURFACE_RETRY);
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
    }
}

/// Arms process-creation hooking so WebView2's children are hooked as they are
/// created rather than after they are already running.
///
/// The DXGI detours only see factories created after they are installed, and
/// WebView2 builds its swapchain during startup, so injecting into a process
/// that already exists is always too late. Loading this before any WebView2
/// process exists is what puts the graphics hooks in ahead of the swapchain.
fn arm_webview_process_hook() -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    let Ok(root) = std::env::current_exe() else {
        return false;
    };
    let Some(artifact_root) = root.parent() else {
        return false;
    };
    let library = artifact_root.join("snowglass_hook_webview2.dll");
    if !library.is_file() || !artifact_root.join("snowglass-injector.exe").is_file() {
        eprintln!(
            "snowglass: the WebView2 process hook needs snowglass_hook_webview2.dll and the injector beside the shell"
        );
        return false;
    }
    // The hook shells out to the injector for each child it catches, so it has
    // to be told where those artifacts live.
    // SAFETY: set while the shell is still single-threaded, before the hook or
    // any WebView2 process can read it.
    unsafe {
        std::env::set_var(snowglass_hook_webview2::ARTIFACT_DIR, artifact_root);
    }
    let wide = library
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: the module stays loaded for the life of the shell, and the export
    // takes no arguments.
    unsafe {
        let Ok(module) = LoadLibraryW(windows::core::PCWSTR(wide.as_ptr())) else {
            eprintln!("snowglass: loading the WebView2 process hook failed");
            return false;
        };
        let Some(install) =
            GetProcAddress(module, windows::core::s!("SnowglassInstallWebView2Hook"))
        else {
            eprintln!("snowglass: the WebView2 process hook has no install export");
            return false;
        };
        let install: unsafe extern "system" fn() -> i32 = std::mem::transmute(install);
        let result = install();
        if result < 0 {
            eprintln!("snowglass: installing the WebView2 process hook failed: {result:#x}");
            return false;
        }
    }
    eprintln!("snowglass: WebView2 child processes will be hooked as they start");
    true
}

/// Whether a producer is one of the WebView2 processes hosting the background
/// page, which is how its stream is told apart from a guest's.
fn is_webview_producer(process_id: u32) -> bool {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    if process_id == 0 {
        return false;
    }
    // SAFETY: the snapshot is closed before returning and the entry is a fully
    // sized live local for the duration of the walk.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return false;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = false;
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32ProcessID == process_id {
                    found = String::from_utf16_lossy(&entry.szExeFile)
                        .trim_end_matches('\0')
                        .eq_ignore_ascii_case("msedgewebview2.exe");
                    break;
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
        found
    }
}

/// Drops a stream from whichever compositor owns it: its own, or the shared one.
fn disconnect_from(
    dedicated: &mut Option<D3d12Compositor>,
    merged: &mut Option<D3d12Compositor>,
    connection: u64,
) -> Result<(), Box<dyn Error>> {
    match (dedicated.as_mut(), merged.as_mut()) {
        (Some(compositor), _) => Renderer::disconnect_stream(compositor, connection),
        (None, Some(compositor)) => Renderer::disconnect_stream(compositor, connection),
        (None, None) => Ok(()),
    }
}

fn unique_session() -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("snowglass-{}-{nonce:x}", std::process::id())
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

fn pointer_axes(x: f64, y: f64) -> [i16; 2] {
    let axis = |value: f64| {
        (value.clamp(0.0, 1.0) * f64::from(u16::MAX) + f64::from(i16::MIN)).round() as i16
    };
    [axis(x), axis(y)]
}

fn shell_pointer_transform(
    transform: PresentationTransform,
    width: u32,
    height: u32,
) -> PresentationTransform {
    if transform.kind == PresentationMappingKind::Unavailable && width != 0 && height != 0 {
        PresentationTransform::axis_aligned(0, 0, width, height)
    } else {
        transform
    }
}

fn set_modifier(keys: &mut u32, flag: u32, pressed: bool) {
    if pressed {
        *keys |= flag;
    } else {
        *keys &= !flag;
    }
}

fn control_event(key: &str, value: i32) -> serde_json::Value {
    let mut fields = key.splitn(3, ':');
    let slot = fields
        .next()
        .and_then(|slot| slot.parse::<usize>().ok())
        .unwrap_or_default();
    let control_type = fields.next().unwrap_or_default();
    let control = fields.next().unwrap_or_default();
    serde_json::json!({
        "slot": slot,
        "controlType": control_type,
        "control": control,
        "value": value,
    })
}

const fn rotated_extent(width: u32, height: u32, quarter_turns: u8) -> (u32, u32) {
    if quarter_turns % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    }
}

/// Holds the process timer resolution at 1ms for the lifetime of the shell.
///
/// `ControlFlow::WaitUntil` bottoms out in `MsgWaitForMultipleObjectsEx`, whose
/// timeout is rounded up to the process timer resolution. At the ~15.6ms
/// default a one-millisecond present retry sleeps through most of a refresh
/// interval, so a deferred frame misses the composition pass it was ready for.
struct TimerResolution(bool);

impl TimerResolution {
    fn raise() -> Self {
        // SAFETY: `timeBeginPeriod` takes a scalar, and a successful call is
        // balanced by `timeEndPeriod` in `Drop`.
        Self(unsafe { timeBeginPeriod(1) } == TIMERR_NOERROR)
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful `timeBeginPeriod(1)` above.
            unsafe { timeEndPeriod(1) };
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = cli::parse()?;
    let _timer_resolution = TimerResolution::raise();
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut application = ShellApplication {
        proxy,
        options: Some(options),
        state: None,
        error: None,
    };
    event_loop.run_app(&mut application)?;
    application.error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_targets_fall_back_to_their_full_presentation_for_pointer_input() {
        assert_eq!(
            shell_pointer_transform(PresentationTransform::UNAVAILABLE, 854, 480),
            PresentationTransform::axis_aligned(0, 0, 854, 480)
        );
    }

    #[test]
    fn shell_targets_preserve_explicit_pointer_transforms() {
        let transform = PresentationTransform::axis_aligned(10, 20, 300, 200);
        assert_eq!(shell_pointer_transform(transform, 854, 480), transform);
    }
}
