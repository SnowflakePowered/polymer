#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod composition;
mod host;
mod input;
mod launch;

use composition::{CompositionTree, SurfaceLayout, SurfaceWindow, WebViewLayer};
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
    CompositingPlan, D3d12Compositor, Extent, Renderer, SurfacePlacement,
    capture::{Capture, CaptureEvent, ConsumerPreference},
    cli::{ViewerApi, ViewerOptions, parse_target_named},
    debug::{
        DebugOverlay, FrameSequenceTracker, InputAxisDiagnostics, InputDiagnostics,
        InputSlotDiagnostics, StatisticsSample, draw_debug_overlay,
    },
    input::PointerRoutingState,
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
        Foundation::POINT,
        System::{Com::IDispatch, Variant::VARIANT},
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
    connection: u64,
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
    needs_redraw: bool,
    resize_due: Option<Instant>,
    compositor: D3d12Compositor,
}

struct EguiOverlay {
    compositor: D3d12Compositor,
    context: egui::Context,
    state: egui_winit::State,
    diagnostics: DebugOverlay,
    sequences: FrameSequenceTracker,
    frames_since_sample: u64,
    sample_started: Option<Instant>,
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
            frames_since_sample: 0,
            sample_started: None,
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

    fn observe_frame(&mut self, connection: u64, frame: snowglass_protocol::FrameReady) {
        let diagnostics = &mut self.diagnostics.frame;
        let observation =
            self.sequences
                .observe(connection, frame.frame_number, frame.timestamp_qpc);
        if let Some(interval_qpc) = observation.interval_qpc {
            diagnostics.frame_interval_ms = interval_qpc * 1000.0 / self.frequency;
        }
        diagnostics.dropped = self.sequences.dropped();
        self.frames_since_sample += 1;
        self.sample_started.get_or_insert_with(Instant::now);
        diagnostics.frame_number = frame.frame_number;
        diagnostics.slot = frame.slot;
        diagnostics.render_width = frame.render_width;
        diagnostics.render_height = frame.render_height;
        diagnostics.capture_x = frame.capture_x;
        diagnostics.capture_y = frame.capture_y;
        diagnostics.capture_width = frame.capture_width;
        diagnostics.capture_height = frame.capture_height;
        diagnostics.latency_ms = performance_counter()
            .unwrap_or(frame.timestamp_qpc)
            .saturating_sub(frame.timestamp_qpc) as f64
            * 1000.0
            / self.frequency;
    }

    fn sample(&mut self) {
        let Some(started) = self.sample_started else {
            return;
        };
        if started.elapsed() < Duration::from_millis(500) {
            return;
        }
        let frame = &mut self.diagnostics.frame;
        let stream_count = self.sequences.stream_count().max(1) as f64;
        frame.frames_per_second =
            self.frames_since_sample as f64 / stream_count / started.elapsed().as_secs_f64();
        self.frames_since_sample = 0;
        self.sample_started = Some(Instant::now());
        frame.history.push_back(StatisticsSample {
            fps: frame.frames_per_second as f32,
            latency_ms: frame.latency_ms as f32,
            frame_interval_ms: frame.frame_interval_ms as f32,
        });
        if frame.history.len() > 240 {
            frame.history.pop_front();
        }
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
        let presentation = Renderer::received_presentation(&self.compositor, extent);
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

struct ShellState {
    // Drop WebView2 and its COM host before tearing down their visual tree.
    foreground_webview: WebView,
    background_webview: WebView,
    _gameview_host: IGameViewHost,
    window: Window,
    composition: CompositionTree,
    overlay: EguiOverlay,
    capture: Capture,
    capture_wake_pending: Arc<AtomicBool>,
    capture_dirty: Arc<AtomicBool>,
    capture_work_pending: bool,
    surfaces: HashMap<u64, CapturedSurface>,
    target: Option<TargetProcess>,
    _input: InputTransport,
    input_provider: Option<Sdl3Provider>,
    pointer_routing: PointerRoutingState,
    direct_input: DirectInputForwarder,
    input_priority: InputPriority,
    cursor: POINT,
    dom_mouse_keys: u32,
    last_input_state: String,
    ui_controls: HashMap<String, i32>,
    next_input_publish: Instant,
    options: ViewerOptions,
    skipped_surfaces: u32,
}

impl ShellState {
    fn new(
        event_loop: &ActiveEventLoop,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
        options: ViewerOptions,
    ) -> Result<Self, Box<dyn Error>> {
        if options.api != ViewerApi::D3d12 {
            return Err(
                "snowglass-shell-webview2 requires `--api d3d12` for DirectComposition".into(),
            );
        }
        if options.webrtc.is_some() {
            return Err("WebRTC publishing is not implemented by the WebView2 shell yet".into());
        }

        let window = event_loop.create_window(
            Window::default_attributes()
                .with_title("Snowglass")
                .with_inner_size(LogicalSize::new(1280.0, 720.0)),
        )?;
        let composition = CompositionTree::new(&window)?;
        let background_visual = composition.webview_visual(WebViewLayer::Background)?;
        let background_webview = WebViewBuilder::new()
            .with_transparent(true)
            .with_focused(false)
            .with_composition_visual_target(background_visual)
            .with_composition_input_enabled(false)
            .build(&window)?;
        let foreground_visual = composition.webview_visual(WebViewLayer::Foreground)?;
        let foreground_proxy = proxy.clone();
        let foreground_webview = WebViewBuilder::new()
            .with_focused(false)
            .with_transparent(true)
            .with_environment(background_webview.environment())
            .with_composition_visual_target(foreground_visual)
            .with_composition_input_enabled(false)
            .with_ipc_handler(move |request| {
                let Ok(message) = serde_json::from_str::<serde_json::Value>(request.body()) else {
                    return;
                };
                match message.get("type").and_then(serde_json::Value::as_str) {
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

        let layout_css = serde_json::to_string(options.layout_css.as_deref().unwrap_or(""))?;
        let shell_html = |layer: &str| {
            include_str!("shell.html")
                .replace("__LAYOUT_CSS_JSON__", &layout_css)
                .replace(
                    "__WEBVIEW_LAYER_JSON__",
                    &serde_json::to_string(layer).expect("a static layer name serializes"),
                )
        };
        background_webview.load_html(include_str!("back.html"))?;
        foreground_webview.load_html(&shell_html("foreground"))?;

        let mut overlay = EguiOverlay::new(&window)?;
        composition.set_egui_overlay_content(&overlay.content()?)?;
        overlay.draw(&window)?;

        let session = unique_session();
        let input = InputTransport::bind(&session)?;
        let input_provider = (!options.gamepads.is_empty())
            .then(|| {
                input.start_provider(Sdl3ProviderConfig {
                    slots: options
                        .gamepads
                        .iter()
                        .map(|gamepad| SlotConfig {
                            selector: gamepad.selector.clone(),
                            layout_id: gamepad.layout.clone(),
                            force_mouse: gamepad.force_mouse,
                        })
                        .collect(),
                    stone_database: options.stone_db.clone(),
                    gamecontroller_database: options.gamecontroller_db.clone(),
                    keyboard_mapping_directory: None,
                })
            })
            .transpose()
            .map_err(std::io::Error::other)?;
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
        let target = Some(launch::launch(&options, &session)?);

        composition.commit()?;
        Ok(Self {
            foreground_webview,
            background_webview,
            _gameview_host: gameview_host,
            window,
            composition,
            overlay,
            capture,
            capture_wake_pending,
            capture_dirty,
            capture_work_pending: false,
            surfaces: HashMap::new(),
            target,
            _input: input,
            input_provider,
            pointer_routing: PointerRoutingState::new(Instant::now()),
            direct_input: DirectInputForwarder::new(control_sender),
            input_priority: InputPriority::Game,
            cursor: POINT::default(),
            dom_mouse_keys: 0,
            last_input_state: String::new(),
            ui_controls: HashMap::new(),
            next_input_publish: Instant::now(),
            options,
            skipped_surfaces: 0,
        })
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
                    if self.skipped_surfaces < self.options.skip_surfaces {
                        self.skipped_surfaces += 1;
                        let _ = accepted.send(false);
                        continue;
                    }
                    if self.surfaces.len() >= self.options.surfaces as usize {
                        let _ = accepted.send(false);
                        continue;
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
                    self.overlay.observe_frame(connection, frame);
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
                    Renderer::set_latest(&mut surface.compositor, connection, frame);
                }
                CaptureEvent::Disconnected(connection) => {
                    self.overlay.sequences.remove(connection);
                    self.cancel_pointer_connection(connection);
                    if let Some(mut surface) = self.surfaces.remove(&connection) {
                        let _ = Renderer::disconnect_stream(&mut surface.compositor, connection);
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
        if changed {
            self.publish_targets();
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
        for (&connection, surface) in &mut self.surfaces {
            if (!Renderer::has_pending_frame(&surface.compositor) && !surface.needs_redraw)
                || !surface.compositor.try_begin_frame()
            {
                continue;
            }
            let plan = surface.composition_plan();
            match Renderer::draw_game(&mut surface.compositor, &plan) {
                Ok(_) => surface.needs_redraw = false,
                Err(error) => {
                    eprintln!("snowglass: rendering stream {connection} failed: {error}");
                }
            }
        }
    }

    fn has_pending_surfaces(&self) -> bool {
        self.surfaces
            .values()
            .any(|surface| surface.needs_redraw || Renderer::has_pending_frame(&surface.compositor))
    }

    fn connect_stream(
        &mut self,
        connection: u64,
        stream: &snowglass_ipc::StreamReady,
    ) -> Result<(), Box<dyn Error>> {
        let (index, quarter_turns) = self
            .next_target_configuration()
            .ok_or("the shell already has a surface for every tracked render target")?;
        let (width, height) = rotated_extent(stream.width, stream.height, quarter_turns);
        let render_window = SurfaceWindow::new(&self.window, width.max(1), height.max(1));
        let mut compositor = D3d12Compositor::new_for_composition_stream(
            &render_window,
            stream.adapter_luid,
            connection,
        )?;
        Renderer::import_stream(&mut compositor, connection, stream, quarter_turns)?;
        let content = compositor.composition_content()?;
        self.composition.add_render_target(connection, &content)?;
        self.composition.commit()?;
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
                needs_redraw: false,
                resize_due: None,
                compositor,
            },
        );
        Ok(())
    }

    fn next_target_configuration(&self) -> Option<(u32, u8)> {
        let in_use = |index| self.surfaces.values().any(|surface| surface.index == index);
        if let Some(targets) = &self.options.track_render_targets {
            targets
                .iter()
                .find(|target| !in_use(target.index))
                .map(|target| (target.index, target.quarter_turns))
        } else {
            (0..self.options.surfaces)
                .find(|&index| !in_use(index))
                .map(|index| (index, 0))
        }
    }

    fn apply_layout(&mut self, layouts: Vec<SurfaceLayout>) {
        let resize_due = Instant::now() + SURFACE_RESIZE_SETTLE;
        for item in layouts {
            let Some(surface) = self.surfaces.get_mut(&item.connection) else {
                continue;
            };
            surface.viewport_x = item.x;
            surface.viewport_y = item.y;
            surface.viewport_width = item.width;
            surface.viewport_height = item.height;
            if let Err(error) = self.composition.apply_layout(
                item,
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
            match Renderer::recreate_swapchain(&mut surface.compositor, &render_window) {
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
            if let Err(error) = self.composition.apply_layout(layout, extent.0, extent.1) {
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
        for (&connection, surface) in &self.surfaces {
            for mut received_surface in Renderer::received_surfaces(&surface.compositor) {
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
                .options
                .gamepads
                .iter()
                .enumerate()
                .map(|(provider_slot, config)| {
                    let Some(state) = states.get(provider_slot).and_then(Option::as_ref) else {
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
        if let Err(error) = self.background_webview.evaluate_script(&script) {
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

    fn publish_targets(&self) {
        let mut targets = self
            .surfaces
            .iter()
            .map(|(&connection, surface)| WebRenderTarget {
                connection,
                index: surface.index,
                width: surface.preferred_width,
                height: surface.preferred_height,
            })
            .collect::<Vec<_>>();
        targets.sort_by_key(|target| target.index);
        let Ok(json) = serde_json::to_string(&targets) else {
            return;
        };
        let script = format!("window.__snowglassSetTargets?.({json});");
        if let Err(error) = self.foreground_webview.evaluate_script(&script) {
            eprintln!(
                "snowglass: publishing render targets to foreground WebView2 failed: {error}"
            );
        }
    }

    fn target_finished(&mut self) -> bool {
        let Some(target) = self.target.as_mut() else {
            return true;
        };
        match target.try_wait() {
            Ok(Some(())) => {
                self.target = None;
                true
            }
            Ok(None) => false,
            Err(error) => {
                eprintln!("snowglass: target process wait failed: {error}");
                true
            }
        }
    }
}

impl Drop for ShellState {
    fn drop(&mut self) {
        for (&connection, surface) in &mut self.surfaces {
            self.capture.revoke(connection);
            let _ = Renderer::disconnect_stream(&mut surface.compositor, connection);
        }
        if let Some(target) = self.target.take() {
            let _ = target.terminate_tree_and_wait();
        }
    }
}

struct ShellApplication {
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    options: Option<ViewerOptions>,
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
                if let Err(error) = state.background_webview.set_bounds(bounds) {
                    eprintln!("snowglass: resizing background WebView2 failed: {error}");
                }
                if let Err(error) = state.foreground_webview.set_bounds(bounds) {
                    eprintln!("snowglass: resizing foreground WebView2 failed: {error}");
                }
                if let Err(error) = state.overlay.resize(&state.window) {
                    eprintln!("snowglass: resizing egui composition overlay failed: {error}");
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
                    || state.dom_mouse_keys & DOM_MOUSE_BUTTONS != 0 =>
            {
                state.cursor = POINT {
                    x: position.x.round() as i32,
                    y: position.y.round() as i32,
                };
                if state.input_priority == InputPriority::Foreground {
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
                || state.dom_mouse_keys & DOM_MOUSE_BUTTONS != 0 =>
            {
                if let Some(button) = mouse_button(button) {
                    if state.input_priority == InputPriority::Foreground {
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
                if state.input_priority == InputPriority::Foreground {
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
            if let Some(resize_due) = state.resize_pending_surfaces(now) {
                deadline = deadline.min(resize_due);
            }
            state.publish_input_state();
            if state.target_finished() {
                event_loop.exit();
                return;
            }
            if state.overlay.repaint_due(now) {
                state.window.request_redraw();
            }
            deadline = deadline.min(state.overlay.next_repaint());
            if state.has_pending_surfaces() {
                deadline = deadline.min(now + PENDING_SURFACE_RETRY);
            }
            if state.capture_work_pending {
                deadline = deadline.min(now + PENDING_SURFACE_RETRY);
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
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

const fn rotated_extent(width: u32, height: u32, quarter_turns: u8) -> (u32, u32) {
    if quarter_turns % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = parse_target_named("snowglass-shell-webview2")?;
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
