#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod cli;
mod composition;
mod game_layer;
mod game_layer_host;
mod input;

use api::{LaunchOptions, parse_input_binding};
use cli::ShellOptions;
use composition::{
    CompositionTree, ForegroundHitRegion, SurfaceLayout, SurfaceWindow, chrome_band_z_index,
    game_view_z_index,
};
use consolespec::BindingKey;
use game_layer::{
    DomKeyboardEvent, DomMouseEvent, EmulatorCommand, EmulatorCommandSink, GameLayerFactory,
    GameViewStyle, InstanceToken,
};
use game_layer_host::GameLayerHost;
use input::InputTransport;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use snowglass_egui::{
    D3d12EguiPainter, DebugOverlayModel, InputAxisDiagnostics, InputDiagnostics,
    InputSlotDiagnostics, draw_debug_overlay,
};
use snowglass_input::{
    hub::{
        BindingOverride, InputOperation, InputTransaction, PortConfiguration, SourceControl,
        SourceSelector, VirtualPort,
    },
    layout::SdlButton,
    mouse::{PointerPhase, layout::Rect},
    provider::sdl3::{ProviderConfig as Sdl3ProviderConfig, Sdl3Provider, SlotConfig},
};
use snowglass_input_dinput::DirectInputForwarder;
use snowglass_platform_windows::{performance_counter, performance_frequency};
use snowglass_protocol::{AdapterLuid, PresentationMappingKind, PresentationTransform};
use snowglass_renderer::{
    CompositingPlan, D3d12Compositor, Extent, PlanLayer, Renderer, SurfaceCapture,
    SurfaceCaptureEvent, SurfaceContent, SurfacePlacement, SurfacePointerSender, cli::ViewerApi,
    input::PointerRoutingState, webrtc,
};
use snowglass_statistics::{FrameMetrics, LiveStatistics, PresentationMetrics, RecorderHandle};
use std::{
    collections::HashMap,
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_MOUSE_EVENT_KIND, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS,
    COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC, ICoreWebView2CompositionController,
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, POINT, WAIT_FAILED, WAIT_OBJECT_0},
        Graphics::DirectComposition::DCompositionWaitForCompositorClock,
        Media::{TIMERR_NOERROR, timeBeginPeriod, timeEndPeriod},
        System::Threading::{CreateEventW, INFINITE, SetEvent},
        UI::{
            Input::KeyboardAndMouse::SetFocus,
            WindowsAndMessaging::{
                WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL,
                WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN,
                WM_XBUTTONUP,
            },
        },
    },
    core::Interface,
};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::Key,
    window::{Window, WindowId},
};
use wry::{WebView, WebViewBuilder, WebViewBuilderExtWindows, WebViewExtWindows};

#[derive(Debug)]
enum UserEvent {
    WakeCapture,
    CompositorTick,
    FrontendReady {
        background: Option<DiscoveredGameLayer>,
        background_error: Option<String>,
    },
    ForegroundHitRegions(Vec<ForegroundHitRegion>),
    GameLayerMessage(serde_json::Value),
    GameLayerProxy(serde_json::Value),
    InputConfigure {
        request_id: u64,
        transaction: InputTransaction,
    },
    EmulatorCommand(EmulatorCommand),
}

#[derive(Debug)]
struct DiscoveredGameLayer {
    html: String,
    base_url: String,
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
/// How many game views the layer document may show at once.
///
/// This fixes how many chrome bands are kept, which is one more than the view
/// count. Overshooting costs a command buffer each and nothing per frame, so it
/// is set well above what a console layout needs rather than derived from the
/// document, which can change under us.
const GAME_LAYER_VIEWS: usize = 8;
const PENDING_SURFACE_RETRY: Duration = Duration::from_millis(1);

/// How long a compositor-clock tick waits for DXGI to return a present slot.
///
/// The clock ticks when DWM begins a composition pass; the slot for the frame
/// that pass retires is released a moment later. Polling at the tick therefore
/// misses it by a hair and defers the present a whole interval, which is the
/// judder this budget exists to close. Expected waits are a fraction of a
/// millisecond — the bound is only here so a stalled swapchain cannot hold the
/// event loop, and the WebView2 host that shares this thread with it.
const PRESENT_PERMIT_WAIT: Duration = Duration::from_millis(3);

/// Where an event-loop turn's wall time goes.
///
/// The shell runs WebView2, Vello, capture intake, and every present on one
/// thread. A turn that runs long does not surface as an obviously slow
/// function: it surfaces as coalesced compositor ticks and a present rate below
/// the display's, which reads identically to a pacing bug. Attributing the time
/// is what separates the two.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TurnPhase {
    Capture,
    Present,
    GameLayer,
    Input,
    Overlay,
}

impl TurnPhase {
    const ALL: [Self; 5] = [
        Self::Capture,
        Self::Present,
        Self::GameLayer,
        Self::Input,
        Self::Overlay,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Present => "present",
            Self::GameLayer => "layer",
            Self::Input => "input",
            Self::Overlay => "overlay",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// An open phase measurement, holding the enclosing phase's nested total so it
/// can be restored when this one closes.
#[derive(Clone, Copy, Debug)]
struct PhaseEntry {
    started: Instant,
    outer_nested: Duration,
}

#[derive(Debug, Default)]
struct TurnProfile {
    spans: [Duration; TurnPhase::ALL.len()],
    /// Time already attributed to phases nested inside the one currently open.
    nested: Duration,
    turns: u64,
    started: Option<Instant>,
    report: String,
}

impl TurnProfile {
    /// Opens a phase. Phases nest — draining capture presents, and presenting
    /// repaints chrome — so each reports the time it spent on its own work
    /// rather than the time its callees spent. Otherwise the enclosing phase
    /// always looks like the expensive one and names the wrong thing to cut.
    fn enter(&mut self) -> PhaseEntry {
        PhaseEntry {
            started: Instant::now(),
            outer_nested: std::mem::take(&mut self.nested),
        }
    }

    fn record(&mut self, phase: TurnPhase, entry: PhaseEntry) {
        let total = entry.started.elapsed();
        self.spans[phase.index()] += total.saturating_sub(self.nested);
        self.nested = entry.outer_nested + total;
    }

    /// Closes a turn, refreshing the report once a sampling window elapses.
    ///
    /// Costs are reported as milliseconds spent per second of wall clock, so a
    /// phase reading 400 is eating 40% of the loop and a turn rate below the
    /// refresh rate cannot be fixed by scheduling presents differently.
    fn finish_turn(&mut self, now: Instant) {
        self.turns += 1;
        self.nested = Duration::ZERO;
        let started = *self.started.get_or_insert(now);
        let elapsed = now.saturating_duration_since(started);
        if elapsed < snowglass_statistics::SAMPLE_INTERVAL {
            return;
        }
        let seconds = elapsed.as_secs_f64();
        let mut phases = TurnPhase::ALL;
        // Costliest first: the point of the line is to name what to cut.
        phases.sort_by(|left, right| self.spans[right.index()].cmp(&self.spans[left.index()]));
        let costs = phases
            .iter()
            .map(|phase| {
                format!(
                    "{} {:.0}",
                    phase.label(),
                    self.spans[phase.index()].as_secs_f64() * 1000.0 / seconds
                )
            })
            .collect::<Vec<_>>()
            .join(" · ");
        self.report = format!(
            "Loop {:.1} turns/s · {costs} ms/s",
            self.turns as f64 / seconds
        );
        self.spans = <[Duration; TurnPhase::ALL.len()]>::default();
        self.turns = 0;
        self.started = Some(now);
    }
}
const EGUI_STATISTICS_REFRESH: Duration = Duration::from_millis(100);

/// How long after egui last took the pointer it keeps drawing at the rate it
/// asks for rather than the statistics cadence.
///
/// Long enough to bridge the gaps between events within a drag and to let the
/// animation that settles after one finish; short enough that releasing returns
/// the overlay to its cadence without a visible tail.
const EGUI_INTERACTION_GRACE: Duration = Duration::from_millis(250);
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

#[derive(Clone)]
struct CapturedSurface {
    index: u32,
    preferred_width: u32,
    preferred_height: u32,
    viewport_width: u32,
    viewport_height: u32,
    viewport_x: i32,
    viewport_y: i32,
    quarter_turns: u8,
    pointer_events: bool,
    z_index: i32,
    corner_radius: f32,
    game_view_style: Option<GameViewStyle>,
    opacity: f32,
}

struct ActiveEmulator {
    token: InstanceToken,
    runtime: snowglass_runtime::EmulatorInstance,
    options: LaunchOptions,
    webrtc: Option<webrtc::Publisher>,
    audio: snowglass_runtime::AudioState,
}

struct EguiOverlay {
    painter: D3d12EguiPainter,
    compositor: D3d12Compositor,
    context: egui::Context,
    state: egui_winit::State,
    diagnostics: DebugOverlayModel,
    frames: HashMap<u64, FrameMetrics>,
    statistics: LiveStatistics,
    presentation: PresentationMetrics,
    frequency: f64,
    next_repaint: Instant,
    /// Until when egui's own repaint requests are honoured instead of the
    /// statistics cadence.
    interactive_until: Option<Instant>,
}

impl EguiOverlay {
    fn new(window: &Window, recorder: RecorderHandle) -> Result<Self, Box<dyn Error>> {
        let size = window.inner_size();
        let render_window = SurfaceWindow::new(window, size.width.max(1), size.height.max(1));
        let compositor = D3d12Compositor::new_default_for_transparent_composition(&render_window)?;
        let painter = D3d12EguiPainter::new(
            compositor.d3d12_device().clone(),
            compositor.target_format(),
            compositor.frames_in_flight(),
        )?;
        let context = egui::Context::default();
        let state = egui_winit::State::new(
            context.clone(),
            egui::ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        let mut diagnostics = DebugOverlayModel::default();
        diagnostics.backend = "Waiting".to_owned();
        diagnostics.consumer = "D3D12/DXGI".to_owned();
        Ok(Self {
            painter,
            compositor,
            context,
            state,
            diagnostics,
            frames: HashMap::new(),
            statistics: LiveStatistics::new(recorder),
            presentation: PresentationMetrics::new("d3d12"),
            frequency: performance_frequency().unwrap_or(1) as f64,
            next_repaint: Instant::now(),
            interactive_until: None,
        })
    }

    fn content(&self) -> Result<windows::core::IUnknown, windows::core::Error> {
        self.compositor.composition_content()
    }

    fn set_backend(&mut self, backend: String) {
        self.diagnostics.backend = backend;
    }

    fn add_surface(&mut self, connection: u64, surface: u64, target: u32) {
        self.frames
            .insert(connection, FrameMetrics::new(connection, surface, target));
    }

    fn observe_frame(&mut self, connection: u64, frame: snowglass_protocol::FrameReady) {
        let latency_ticks = performance_counter()
            .unwrap_or(frame.timestamp_qpc)
            .saturating_sub(frame.timestamp_qpc);
        if let Some(metrics) = self.frames.get_mut(&connection) {
            metrics.observe(
                frame.frame_number,
                frame.timestamp_qpc,
                self.frequency,
                Duration::from_secs_f64(latency_ticks as f64 / self.frequency.max(1.0)),
            );
        }
        self.diagnostics.frame_number = frame.frame_number;
        self.diagnostics.slot = frame.slot;
        self.diagnostics.render_width = frame.render_width;
        self.diagnostics.render_height = frame.render_height;
        self.diagnostics.capture_x = frame.capture_x;
        self.diagnostics.capture_y = frame.capture_y;
        self.diagnostics.capture_width = frame.capture_width;
        self.diagnostics.capture_height = frame.capture_height;
    }

    fn sample(&mut self) {
        self.diagnostics.statistics = self.statistics.sample().clone();
    }

    fn record_present(&mut self, carried_new_frame: bool) {
        self.presentation.record_present(carried_new_frame);
    }

    fn record_clock(&mut self) {
        self.presentation.record_clock();
    }

    fn set_note(&mut self, note: &str) {
        let notes = &mut self.diagnostics.notes;
        if note.is_empty() {
            notes.clear();
        } else if notes.first().is_none_or(|first| first != note) {
            notes.clear();
            notes.push(note.to_owned());
        }
    }

    fn remove_target(&mut self, connection: u64) {
        self.frames.remove(&connection);
        self.statistics.remove_connection(connection);
        self.diagnostics.statistics = self.statistics.snapshot().clone();
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

    /// Notes that egui has the pointer, so it repaints at the rate it asks for
    /// until the interaction settles.
    fn note_interaction(&mut self, now: Instant) {
        self.interactive_until = Some(now + EGUI_INTERACTION_GRACE);
        // An interaction starting between scheduled repaints must not wait out
        // the cadence it was released from.
        self.next_repaint = self.next_repaint.min(now);
    }

    fn interactive(&self, now: Instant) -> bool {
        self.interactive_until.is_some_and(|until| now < until)
    }

    fn draw(&mut self, window: &Window) -> Result<bool, Box<dyn Error>> {
        // Idle, everything this overlay reports is sampled on the statistics
        // interval, so repainting faster only spends the loop it is there to
        // measure — egui asks for a repaint on the next frame whenever anything
        // animates, and honouring that unconditionally ran it at display rate.
        //
        // Under the pointer that trade inverts: a drag has to track the cursor,
        // and a diagnostics overlay that lags its own input is unusable. While
        // egui holds the pointer it draws as often as it asks.
        //
        // The gate lives here rather than only where the loop schedules
        // repaints because `request_redraw` reaches this directly, so anywhere
        // else leaves the cost scaling with how many events the window gets.
        let now = Instant::now();
        let interactive = self.interactive(now);
        if !interactive && !self.repaint_due(now) {
            return Ok(false);
        }
        self.next_repaint = now + EGUI_STATISTICS_REFRESH;
        if !self.compositor.try_begin_frame() {
            // Mid-interaction the permit frees within a refresh, and waiting a
            // full cadence for it would drop the drag to 10 Hz.
            if interactive {
                self.next_repaint = now + PENDING_SURFACE_RETRY;
            }
            return Ok(false);
        }
        self.sample();
        let input = self.state.take_egui_input(window);
        let diagnostics = &self.diagnostics;
        let mut output = self.context.run(input, |context| {
            draw_debug_overlay(context, diagnostics);
        });
        self.state
            .handle_platform_output(window, std::mem::take(&mut output.platform_output));
        let primitives = self
            .context
            .tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
        if interactive {
            // What egui asked for, still floored by the statistics cadence so
            // an idle interaction window does not stop refreshing the numbers.
            let requested = output
                .viewport_output
                .get(&egui::ViewportId::ROOT)
                .map_or(Duration::ZERO, |viewport| viewport.repaint_delay);
            self.next_repaint = now + requested.min(EGUI_STATISTICS_REFRESH);
        }
        let plan = CompositingPlan::new([0.0, 0.0, 0.0, 0.0], Vec::new());
        let Self {
            painter,
            compositor,
            ..
        } = self;
        compositor.present_with(&plan, None, |pass| {
            painter.paint(pass, &output, &primitives)
        })?;
        // A drag in progress, or a pointer resting over an egui window, keeps
        // the interaction alive without needing a fresh event every frame.
        if self.context.is_using_pointer() || self.context.wants_pointer_input() {
            self.note_interaction(now);
        }
        Ok(true)
    }

    fn repaint_due(&self, now: Instant) -> bool {
        self.next_repaint <= now
    }

    fn next_repaint(&self) -> Instant {
        self.next_repaint
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
    worker: Option<std::thread::JoinHandle<()>>,
    stop_event: HANDLE,
    active: Arc<AtomicBool>,
    healthy: Arc<AtomicBool>,
    tick_pending: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
}

impl CompositorClock {
    fn spawn(proxy: winit::event_loop::EventLoopProxy<UserEvent>) -> std::io::Result<Self> {
        let stop_event =
            unsafe { CreateEventW(None, true, false, None) }.map_err(std::io::Error::other)?;
        let active = Arc::new(AtomicBool::new(false));
        let healthy = Arc::new(AtomicBool::new(true));
        let tick_pending = Arc::new(AtomicBool::new(false));
        let stopping = Arc::new(AtomicBool::new(false));
        let thread_active = Arc::clone(&active);
        let thread_healthy = Arc::clone(&healthy);
        let thread_tick_pending = Arc::clone(&tick_pending);
        let thread_stopping = Arc::clone(&stopping);
        let thread_stop_event = stop_event.0 as usize;
        let handle = match std::thread::Builder::new()
            .name("snowglass-compositor-clock".to_owned())
            .spawn(move || {
                let thread_stop_event = HANDLE(thread_stop_event as *mut _);
                loop {
                    // A shell with no streams on screen has nothing to pace.
                    // Park instead of waking once per refresh for no reason;
                    // `park` may return spuriously, so re-check the flag.
                    while !thread_active.load(Ordering::Acquire) {
                        if thread_stopping.load(Ordering::Acquire) {
                            return;
                        }
                        std::thread::park();
                    }
                    if thread_stopping.load(Ordering::Acquire) {
                        return;
                    }
                    // SAFETY: the call reads the process-wide compositor clock
                    // and is passed no handles to borrow.
                    let status = unsafe {
                        DCompositionWaitForCompositorClock(Some(&[thread_stop_event]), INFINITE)
                    };
                    if status == WAIT_FAILED.0 {
                        // Fall back to timer pacing rather than spinning on a
                        // clock this session cannot wait on.
                        thread_healthy.store(false, Ordering::Release);
                        return;
                    }
                    if thread_stopping.load(Ordering::Acquire) {
                        return;
                    }

                    if status != WAIT_OBJECT_0.0 + 1 {
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
            }) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = unsafe { CloseHandle(stop_event) };
                return Err(error);
            }
        };
        Ok(Self {
            worker: Some(handle),
            stop_event,
            active,
            healthy,
            tick_pending,
            stopping,
        })
    }

    /// Paces the event loop only while streams are on screen.
    fn set_active(&self, active: bool) {
        if self.active.swap(active, Ordering::AcqRel) != active
            && active
            && let Some(worker) = &self.worker
        {
            worker.thread().unpark();
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

impl Drop for CompositorClock {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        self.active.store(true, Ordering::Release);
        let _ = unsafe { SetEvent(self.stop_event) };
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
        let _ = unsafe { CloseHandle(self.stop_event) };
    }
}

struct StreamState {
    capture: SurfaceCapture,
    pointer: SurfacePointerSender,
    wake_pending: Arc<AtomicBool>,
    dirty: Arc<AtomicBool>,
    work_pending: bool,
    surfaces: HashMap<u64, CapturedSurface>,
}

struct ShellInputState {
    transport: InputTransport,
    mouse_buttons: u8,
    pointer_routing: PointerRoutingState,
    direct_input: DirectInputForwarder,
    priority: InputPriority,
    cursor: POINT,
    dom_mouse_keys: u32,
    dom_alt_key: bool,
    foreground_hit_regions: Vec<ForegroundHitRegion>,
    foreground_mouse_capture: bool,
    last_published_state: String,
    ui_controls: HashMap<String, i32>,
    next_publish: Instant,
    guide_chord_held: bool,
}

impl ShellInputState {
    fn set_mouse_button(&mut self, flag: u8, pressed: bool) -> u8 {
        let previous = self.mouse_buttons;
        if pressed {
            self.mouse_buttons |= flag;
        } else {
            self.mouse_buttons &= !flag;
        }
        previous
    }
}

struct GameSessionState {
    session: String,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    active: Option<ActiveEmulator>,
    pending_cli_launch: Option<LaunchOptions>,
}

struct PresentationState {
    composition: CompositionTree,
    overlay: EguiOverlay,
    clock: CompositorClock,
    profile: TurnProfile,
    /// One window-spanning opaque swapchain shared by every stream.
    merged: Option<D3d12Compositor>,
    merged_dirty: bool,
    game_layer_chrome_bands: Vec<usize>,
    game_layer_chrome_dirty: bool,
    game_layer_refresh_pending: bool,
}

struct ShellState {
    // Drop WebView2 and its COM host before tearing down their visual tree.
    foreground_webview: WebView,
    window: Window,
    game_layer: GameLayerHost,
    presentation: PresentationState,
    streams: StreamState,
    game: GameSessionState,
    input: ShellInputState,
}

impl ShellState {
    fn new(
        event_loop: &ActiveEventLoop,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
        options: ShellOptions,
        statistics_recorder: RecorderHandle,
    ) -> Result<Self, Box<dyn Error>> {
        let ShellOptions {
            viewer: options,
            foreground_url,
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
        let mut composition = CompositionTree::new(&window)?;
        let size = window.inner_size();
        let render_window = SurfaceWindow::new(&window, size.width.max(1), size.height.max(1));
        // Prepare Vello before building WebView2. Initializing its D3D12 state
        // after Chromium has started WARP faults inside d3d10warp.dll on this
        // system. The factory chooses the initial adapter, and the merged
        // compositor must use the same device so it can import Vello's shared
        // game-layer textures before a captured stream supplies an adapter.
        let game_layer_factory = GameLayerFactory::new()?;
        let merged = D3d12Compositor::new_for_shared_composition(
            &render_window,
            game_layer_factory.adapter_luid(),
        )?;
        composition.set_merged_content(&merged.composition_content()?)?;
        composition.commit()?;
        let session = unique_session();
        // The foreground page opts into a composed game layer with
        // `<link rel="background">`. Its initialization bridge discovers and
        // fetches that document before announcing that the frontend is ready;
        // document discovery itself remains deferred to the page.
        let game_layer = GameLayerHost::new(game_layer_factory);
        let foreground_visual = composition.foreground_visual()?;
        let foreground_proxy = proxy.clone();
        let bridge_script = include_str!(concat!(env!("OUT_DIR"), "/bridge.js"));
        let foreground_builder = WebViewBuilder::new()
            .with_focused(false)
            .with_transparent(true)
            .with_initialization_script(bridge_script);
        let foreground_webview = foreground_builder
            .with_composition_visual_target(foreground_visual)
            .with_composition_input_enabled(false)
            .with_ipc_handler(move |request| {
                let Ok(message) = serde_json::from_str::<serde_json::Value>(request.body()) else {
                    return;
                };
                match message.get("type").and_then(serde_json::Value::as_str) {
                    Some("frontendReady") => {
                        let background = message.get("background").and_then(|background| {
                            Some(DiscoveredGameLayer {
                                html: background.get("html")?.as_str()?.to_owned(),
                                base_url: background.get("url")?.as_str()?.to_owned(),
                            })
                        });
                        let background_error = message
                            .get("backgroundError")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned);
                        let _ = foreground_proxy.send_event(UserEvent::FrontendReady {
                            background,
                            background_error,
                        });
                    }
                    Some("foregroundHitRegions") => {
                        let Some(regions) = message.get("regions") else {
                            return;
                        };
                        match serde_json::from_value::<Vec<ForegroundHitRegion>>(regions.clone()) {
                            Ok(regions) => {
                                let _ = foreground_proxy
                                    .send_event(UserEvent::ForegroundHitRegions(regions));
                            }
                            Err(error) => {
                                tracing::error!(
                                    "snowglass: invalid WebView2 foreground hit regions: {error}"
                                );
                            }
                        }
                    }
                    Some("gameLayerMessage") => {
                        let message = message
                            .get("message")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let _ = foreground_proxy.send_event(UserEvent::GameLayerMessage(message));
                    }
                    Some("gameLayerProxy") => {
                        let Some(request) = message.get("request").cloned() else {
                            return;
                        };
                        let _ = foreground_proxy.send_event(UserEvent::GameLayerProxy(request));
                    }
                    Some("inputConfigure") => {
                        let Some(request_id) =
                            message.get("id").and_then(serde_json::Value::as_u64)
                        else {
                            return;
                        };
                        let Some(transaction) = message.get("transaction").cloned() else {
                            return;
                        };
                        match serde_json::from_value::<InputTransaction>(transaction) {
                            Ok(transaction) => {
                                let _ = foreground_proxy.send_event(UserEvent::InputConfigure {
                                    request_id,
                                    transaction,
                                });
                            }
                            Err(error) => {
                                tracing::error!(
                                    "snowglass: invalid input configuration transaction: {error}"
                                );
                            }
                        }
                    }
                    _ => {}
                }
            })
            .build(&window)?;

        // A page driven by `requestAnimationFrame`, which is what Chromium
        // throttles when it decides a window is not being seen. Used to check
        // that a hidden, captured background keeps producing frames.
        foreground_webview.load_url(&foreground_url)?;

        let mut overlay = EguiOverlay::new(&window, statistics_recorder)?;
        composition.set_egui_overlay_content(&overlay.content()?)?;
        overlay.draw(&window)?;

        let input = InputTransport::runtime().map_err(std::io::Error::other)?;
        let control_sender = input.control_sender();
        let capture_wake_pending = Arc::new(AtomicBool::new(false));
        let capture_dirty = Arc::new(AtomicBool::new(false));
        let capture = SurfaceCapture::default();
        let pointer = capture.pointer_sender();
        let compositor_clock = CompositorClock::spawn(proxy.clone())?;
        // Merging composites every stream into one opaque window-covering
        // swapchain. A game layer supplies the chrome that swapchain draws
        // around the streams, which is what lets it stay opaque and so stay
        // eligible for a hardware overlay plane.

        composition.commit()?;
        Ok(Self {
            foreground_webview,
            window,
            game_layer,
            presentation: PresentationState {
                composition,
                overlay,
                clock: compositor_clock,
                profile: TurnProfile::default(),
                merged: Some(merged),
                merged_dirty: true,
                game_layer_chrome_bands: Vec::new(),
                game_layer_chrome_dirty: false,
                game_layer_refresh_pending: false,
            },
            streams: StreamState {
                capture,
                pointer,
                wake_pending: capture_wake_pending,
                dirty: capture_dirty,
                work_pending: false,
                surfaces: HashMap::new(),
            },
            game: GameSessionState {
                session,
                proxy,
                active: None,
                pending_cli_launch,
            },
            input: ShellInputState {
                transport: input,
                mouse_buttons: 0,
                pointer_routing: PointerRoutingState::new(Instant::now()),
                direct_input: DirectInputForwarder::new(control_sender),
                priority: InputPriority::Foreground,
                cursor: POINT::default(),
                dom_mouse_keys: 0,
                dom_alt_key: false,
                foreground_hit_regions: Vec::new(),
                foreground_mouse_capture: false,
                last_published_state: String::new(),
                ui_controls: HashMap::new(),
                next_publish: Instant::now(),
                guide_chord_held: false,
            },
        })
    }

    fn deliver_cli_launch(&mut self) {
        let Some(options) = self.game.pending_cli_launch.take() else {
            return;
        };
        let Ok(options) = serde_json::to_value(&options) else {
            return;
        };
        if !self.game_layer.post_message(&serde_json::json!({
            "type": "snowglass-cli-launch",
            "options": options,
        })) {
            tracing::error!("snowglass: CLI launch requires a linked game layer");
            return;
        }
        self.flush_game_layer_bridge();
    }

    fn flush_game_layer_bridge(&mut self) {
        self.game_layer.flush(&self.foreground_webview);
        if self
            .game_layer
            .active()
            .is_some_and(|layer| layer.needs_paint())
        {
            self.refresh_game_layer();
        }
    }

    fn apply_emulator_command(&mut self, command: EmulatorCommand) {
        match command {
            EmulatorCommand::Launch { token, options } => self.launch_game(token, options),
            EmulatorCommand::Close(token) => self.close_game(token),
            EmulatorCommand::SetVolume { token, volume } => self.set_game_volume(token, volume),
            EmulatorCommand::SetMuted { token, muted } => self.set_game_muted(token, muted),
            EmulatorCommand::Message { token, name, args } => self.game_message(token, &name, args),
            EmulatorCommand::Input { token, active } if self.is_active(token) => {
                self.set_input_priority(if active {
                    InputPriority::Game
                } else {
                    InputPriority::Foreground
                });
            }
            EmulatorCommand::Input { .. } => {}
            EmulatorCommand::ConfigureInput {
                request_id,
                transaction,
            } => self.configure_input(u64::from(request_id), transaction, true),
        }
    }

    fn is_active(&self, token: InstanceToken) -> bool {
        self.game
            .active
            .as_ref()
            .is_some_and(|active| active.token == token)
    }

    fn input_provider(&self) -> Option<&Sdl3Provider> {
        self.input.transport.provider()
    }

    fn configure_input(
        &mut self,
        request_id: u64,
        transaction: InputTransaction,
        background: bool,
    ) {
        let active = self.game.active.is_some();
        let topology_change = transaction.operations.iter().any(|operation| {
            matches!(
                operation,
                InputOperation::ReplacePorts { .. }
                    | InputOperation::ConfigurePort { .. }
                    | InputOperation::SetSource { .. }
                    | InputOperation::SetLayout { .. }
                    | InputOperation::RemovePort { .. }
            )
        });
        if active && topology_change {
            self.send_input_configuration_result(
                request_id,
                Err(
                    "changing controller topology while an emulator is attached is not supported by the current SDL hook"
                        .to_owned(),
                ),
                background,
            );
            return;
        }
        let current_configuration = self.input.transport.hub().snapshot();
        if active
            && let Some(binding) = transaction.operations.iter().find_map(|operation| {
                let InputOperation::SetOverride { port, binding, .. } = operation else {
                    return None;
                };
                let source = current_configuration
                    .ports
                    .iter()
                    .find(|configured| configured.port == *port)
                    .and_then(|configured| configured.source.as_ref());
                (!binding_supported_by_attached_provider(binding, source)).then_some(binding)
            })
        {
            self.send_input_configuration_result(
                request_id,
                Err(format!(
                    "binding {binding:?} cannot yet be projected by the SDL data plane"
                )),
                background,
            );
            return;
        }

        match self.input.transport.hub().apply(transaction) {
            Ok(configuration) => {
                if topology_change
                    && let Err(error) = self
                        .input
                        .transport
                        .restart_from_configuration(&configuration)
                {
                    self.send_input_configuration_result(request_id, Err(error), background);
                    return;
                }
                if let Some(provider) = self.input_provider() {
                    for port in &configuration.ports {
                        // Overrides are fanned out per virtual pad; a port that
                        // presents none has nothing to fan out to.
                        let Some(slot) = port.slot.map(usize::from) else {
                            continue;
                        };
                        provider.reset_bindings(slot);
                        for (target, binding) in &port.overrides {
                            if let BindingOverride::Axis {
                                source:
                                    source @ (SourceControl::MouseDelta { .. }
                                    | SourceControl::MousePosition { .. }),
                                transform,
                            } = binding
                            {
                                provider.set_mouse_axis_binding(
                                    slot,
                                    target.clone(),
                                    source.clone(),
                                    *transform,
                                );
                            } else if matches!(port.source, Some(SourceSelector::KeyboardMouse)) {
                                let key = match binding {
                                    BindingOverride::Button {
                                        source: SourceControl::Key { code },
                                    }
                                    | BindingOverride::Axis {
                                        source: SourceControl::Key { code },
                                        ..
                                    } => Some(code.clone()),
                                    BindingOverride::Disabled => None,
                                    _ => continue,
                                };
                                provider.set_key_binding(slot, target.clone(), key);
                            } else if let Some(binding) = binding.sdl_binding() {
                                provider.set_binding(slot, target.clone(), binding);
                            }
                        }
                    }
                }
                self.publish_input_configuration();
                self.send_input_configuration_result(request_id, Ok(configuration), background);
            }
            Err(error) => {
                self.send_input_configuration_result(
                    request_id,
                    Err(error.to_string()),
                    background,
                );
            }
        }
    }

    fn publish_input_configuration(&mut self) {
        let configuration = self.input.transport.hub().snapshot();
        let layouts = self.input.transport.hub().layouts();
        let configuration_value =
            serde_json::to_value(&configuration).unwrap_or(serde_json::Value::Null);
        let layouts_value = serde_json::to_value(&layouts).unwrap_or(serde_json::Value::Null);
        self.game_layer
            .set_input_configuration(&configuration_value, &layouts_value);
        let Ok(configuration) = serde_json::to_string(&configuration) else {
            return;
        };
        let Ok(layouts) = serde_json::to_string(&layouts) else {
            return;
        };
        let script =
            format!("window.__snowglassSetInputConfiguration?.({configuration}, {layouts});");
        if let Err(error) = self.foreground_webview.evaluate_script(&script) {
            tracing::error!("snowglass: publishing input configuration failed: {error}");
        }
    }

    fn send_input_configuration_result(
        &mut self,
        request_id: u64,
        result: Result<snowglass_input::hub::InputConfiguration, String>,
        background: bool,
    ) {
        let result = match result {
            Ok(configuration) => serde_json::json!({
                "ok": true,
                "configuration": configuration,
            }),
            Err(error) => serde_json::json!({
                "ok": false,
                "error": error,
            }),
        };
        if background {
            self.game_layer
                .input_configuration_result(request_id, &result);
            return;
        }
        let script =
            format!("window.__snowglassInputConfigurationResult?.({request_id}, {result});");
        if let Err(error) = self.foreground_webview.evaluate_script(&script) {
            tracing::error!("snowglass: returning input configuration result failed: {error}");
        }
    }

    fn frontend_ready(
        &mut self,
        background: Option<DiscoveredGameLayer>,
        background_error: Option<String>,
    ) {
        if let Some(error) = background_error {
            tracing::error!("snowglass: loading the linked game layer failed: {error}");
        }
        if let Some(background) = background {
            let size = self.window.inner_size();
            let command_proxy = self.game.proxy.clone();
            let activated = self.game_layer.activate(|factory| {
                let emulator_commands = EmulatorCommandSink::new(move |command| {
                    if command_proxy
                        .send_event(UserEvent::EmulatorCommand(command))
                        .is_err()
                    {
                        tracing::error!("snowglass: emulator command event loop is unavailable");
                    }
                });
                factory.create(
                    &background.html,
                    background.base_url.clone(),
                    GAME_LAYER_VIEWS,
                    size.width,
                    size.height,
                    emulator_commands,
                )
            });
            if !activated {
                tracing::error!(
                    "snowglass: ignoring a second game layer discovered at {}",
                    background.base_url
                );
                self.deliver_cli_launch();
                return;
            }
            self.refresh_game_layer();
            self.game_layer.flush(&self.foreground_webview);
            if let Err(error) = self
                .foreground_webview
                .evaluate_script("window.__snowglassGameLayerReady?.();")
            {
                tracing::error!("snowglass: could not announce game layer readiness: {error}");
            }
            tracing::error!(
                "snowglass: using game layer discovered at {}",
                background.base_url
            );
        } else {
            // A page without the link does not use composed chrome and should
            // not retain the renderer that was prepared solely for it.
            self.game_layer.disable();
        }
        self.publish_input_configuration();
        self.deliver_cli_launch();
    }

    fn launch_game(&mut self, token: InstanceToken, options: LaunchOptions) {
        if self.game.active.is_some() {
            self.emit_instance_event(
                token,
                "error",
                &serde_json::json!({ "message": "another emulator instance is active" }),
            );
            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            return;
        }
        let invalid_background_attachment = self.game_layer.active().and_then(|layer| {
            options
                .surfaces
                .iter()
                .find(|surface| !layer.validates_attachment(surface.attach, surface.index))
                .map(|surface| surface.index)
        });
        if let Some(index) = invalid_background_attachment {
            self.emit_instance_event(
                token,
                "error",
                &serde_json::json!({
                    "message": format!(
                        "surface {index} does not attach to its background tracked-surface"
                    )
                }),
            );
            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            return;
        }
        self.emit_instance_event(token, "start", &serde_json::Value::Null);
        let mut gamepad_bindings = Vec::new();
        let mut hub_ports = Vec::new();
        // Next free index per group, for mappings that leave the port implicit.
        let mut group_indices: HashMap<String, u8> = HashMap::new();
        for mapping in &options.gamepad_mapping {
            let Ok(slot) = u8::try_from(mapping.slot) else {
                self.emit_instance_event(
                    token,
                    "error",
                    &serde_json::json!({ "message": "virtual pad slot exceeds 255" }),
                );
                self.emit_instance_event(token, "closed", &serde_json::Value::Null);
                return;
            };
            let mut overrides = HashMap::new();
            for (target, source) in &mapping.mapping {
                // The descriptor arrives as JSON, so the key is a string here
                // and nowhere past here: a target the grammar does not admit is
                // reported like any other malformed launch option.
                let target = match target.parse::<BindingKey>() {
                    Ok(target) => target,
                    Err(error) => {
                        self.emit_instance_event(
                            token,
                            "error",
                            &serde_json::json!({ "message": error.to_string() }),
                        );
                        self.emit_instance_event(token, "closed", &serde_json::Value::Null);
                        return;
                    }
                };
                match parse_input_binding(source) {
                    Ok(binding) => {
                        overrides.insert(target, hub_binding(&binding));
                        gamepad_bindings.push((mapping.slot, target, binding));
                    }
                    Err(error) => {
                        self.emit_instance_event(
                            token,
                            "error",
                            &serde_json::json!({ "message": error }),
                        );
                        self.emit_instance_event(token, "closed", &serde_json::Value::Null);
                        return;
                    }
                }
            }
            // A descriptor may name the port outright; when it does not, the
            // machine decides, because no machinespec puts one layout in two
            // groups. Indices run in slot order within each group.
            let port = match mapping.port.clone() {
                Some(port) => port,
                None => {
                    let resolved = consolespec::MachineSpec::try_from(options.machine.as_str())
                        .map_err(|error| error.to_string())
                        .and_then(|machine| {
                            machine.group_for(&mapping.layout).map(|group| {
                                let index =
                                    group_indices.entry(group.name().to_owned()).or_insert(0u8);
                                let port = VirtualPort::new(group.name(), *index);
                                *index += 1;
                                port
                            })
                        });
                    match resolved {
                        Ok(port) => port,
                        Err(error) => {
                            self.emit_instance_event(
                                token,
                                "error",
                                &serde_json::json!({ "message": error }),
                            );
                            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
                            return;
                        }
                    }
                }
            };
            hub_ports.push(PortConfiguration {
                port,
                layout: mapping.layout.clone(),
                slot: Some(slot),
                source: Some(hub_source(mapping.selector())),
                overrides,
            });
        }
        let hub = self.input.transport.hub();
        if let Err(error) = hub.apply(InputTransaction {
            expected_revision: hub.snapshot().revision,
            operations: vec![InputOperation::ReplacePorts {
                machine: options.machine.clone(),
                ports: hub_ports,
            }],
        }) {
            self.emit_instance_event(
                token,
                "error",
                &serde_json::json!({ "message": error.to_string() }),
            );
            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            return;
        }
        self.publish_input_configuration();
        let mut slots = options
            .gamepad_mapping
            .iter()
            .map(|mapping| mapping.slot)
            .max()
            .map(|slot| {
                vec![
                    SlotConfig {
                        selector: snowglass_input::provider::sdl3::DeviceSelector::Index(
                            usize::MAX
                        ),
                        layout_id: "XBOX_CONTROLLER".to_owned(),
                        force_mouse: false,
                    };
                    slot + 1
                ]
            })
            .unwrap_or_default();
        for mapping in &options.gamepad_mapping {
            slots[mapping.slot] = SlotConfig {
                selector: mapping.selector(),
                layout_id: mapping.layout.clone(),
                force_mouse: mapping.mouse,
            };
        }
        let provider_config = Sdl3ProviderConfig {
            slots,
            gamecontroller_database: None,
            keyboard_mapping_directory: None,
        };
        if let Err(error) = self.input.transport.restart_provider(provider_config) {
            self.emit_instance_event(token, "error", &serde_json::json!({ "message": error }));
            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            return;
        }
        if let Some(provider) = self.input_provider() {
            for (slot, binding_target, binding) in gamepad_bindings {
                provider.set_binding(slot, binding_target, binding);
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
                        token,
                        "error",
                        &serde_json::json!({ "message": error }),
                    );
                    self.emit_instance_event(token, "closed", &serde_json::Value::Null);
                    return;
                }
            };
            match webrtc::Publisher::start(bind, &webrtc_targets) {
                Ok(publisher) => Some(publisher),
                Err(error) => {
                    self.emit_instance_event(
                        token,
                        "error",
                        &serde_json::json!({ "message": error.to_string() }),
                    );
                    self.emit_instance_event(token, "closed", &serde_json::Value::Null);
                    return;
                }
            }
        };
        let artifact_root = match std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        {
            Some(path) => path,
            None => {
                self.emit_instance_event(
                    token,
                    "error",
                    &serde_json::json!({ "message": "shell executable has no parent directory" }),
                );
                self.emit_instance_event(token, "closed", &serde_json::Value::Null);
                return;
            }
        };
        let mut gamepad_layouts = options.gamepad_mapping.iter().collect::<Vec<_>>();
        gamepad_layouts.sort_by_key(|mapping| mapping.slot);
        let runtime_config = snowglass_runtime::EmulatorConfig {
            session: self.game.session.clone(),
            command: options.target(),
            artifact_root,
            hooks: options
                .hook
                .iter()
                .copied()
                .map(snowglass_renderer::cli::HookBackend::from)
                .collect(),
            consumer: snowglass_runtime::ConsumerPreference::D3d12,
            isolate: options.isolated,
            tracked_surfaces: options
                .surfaces
                .iter()
                .map(|surface| snowglass_runtime::TrackedSurfaceConfig {
                    id: snowglass_runtime::TrackedSurfaceId(surface.index),
                    render_target_index: surface.index,
                    window_pattern: surface.selector.clone(),
                })
                .collect(),
            tracking_enabled: !options.surfaces.is_empty(),
            skip_surfaces: 0,
            graph_tracking: false,
            force_keyed_mutex: false,
            gamepad_layouts: gamepad_layouts
                .into_iter()
                .map(|mapping| mapping.layout.clone())
                .collect(),
        };
        let wake_proxy = self.game.proxy.clone();
        let capture_dirty = self.streams.dirty.clone();
        let capture_wake_pending = self.streams.wake_pending.clone();
        match snowglass_runtime::EmulatorInstance::spawn_with_waker(runtime_config, move || {
            capture_dirty.store(true, Ordering::Release);
            if !capture_wake_pending.swap(true, Ordering::AcqRel)
                && wake_proxy.send_event(UserEvent::WakeCapture).is_err()
            {
                capture_wake_pending.store(false, Ordering::Release);
            }
        }) {
            Ok(target) => {
                self.streams.capture.attach(&target);
                self.input.transport.attach(&target);
                let audio = target.audio_state();
                self.game.active = Some(ActiveEmulator {
                    token,
                    runtime: target,
                    options,
                    webrtc: publisher,
                    audio,
                });
                self.set_input_priority(InputPriority::Game);
                self.emit_instance_event(token, "ready", &serde_json::Value::Null);
                self.publish_audio_state(true);
            }
            Err(error) => {
                self.emit_instance_event(
                    token,
                    "error",
                    &serde_json::json!({ "message": error.to_string() }),
                );
                self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            }
        }
    }

    fn close_game(&mut self, token: InstanceToken) {
        let Some(active) = self
            .game
            .active
            .as_mut()
            .filter(|active| active.token == token)
        else {
            return;
        };
        active.runtime.close();
        self.streams.capture.detach();
        self.input.transport.detach();
        self.finish_active_game();
    }

    fn finish_active_game(&mut self) {
        if let Some(provider) = self.input_provider() {
            provider.clear_keys();
            provider.set_pointer(None);
            provider.set_enabled(false);
        }
        let Some(active) = self.game.active.take() else {
            return;
        };
        let connections = self.streams.surfaces.keys().copied().collect::<Vec<_>>();
        for connection in connections {
            self.cancel_pointer_connection(connection);
            self.presentation.overlay.remove_target(connection);
            if let Some(publisher) = &active.webrtc {
                publisher.remove_stream(connection);
            }
            if self.streams.surfaces.remove(&connection).is_some() {
                self.streams.capture.revoke(connection);
                if let Some(merged) = self.presentation.merged.as_mut() {
                    let _ = Renderer::disconnect_stream(merged, connection);
                }
                self.presentation.merged_dirty = true;
            }
        }
        let _ = self.presentation.composition.commit();
        self.set_input_priority(InputPriority::Foreground);
        self.input.foreground_hit_regions.clear();
        self.publish_targets_for(active.token);
        self.emit_instance_event(active.token, "closed", &serde_json::Value::Null);
    }

    fn game_message(&mut self, token: InstanceToken, name: &str, args: serde_json::Value) {
        if self.is_active(token) {
            self.emit_instance_event(
                token,
                "message",
                &serde_json::json!({ "name": name, "args": args }),
            );
        }
    }

    fn set_game_volume(&mut self, token: InstanceToken, volume: f64) {
        let Some(volume) = snowglass_runtime::NormalizedVolume::new(volume as f32) else {
            return;
        };
        let Some(active) = self
            .game
            .active
            .as_ref()
            .filter(|active| active.token == token)
        else {
            return;
        };
        active.runtime.set_volume(volume);
        self.publish_audio_state(false);
    }

    fn set_game_muted(&mut self, token: InstanceToken, muted: bool) {
        let Some(active) = self
            .game
            .active
            .as_ref()
            .filter(|active| active.token == token)
        else {
            return;
        };
        active.runtime.set_muted(muted);
        self.publish_audio_state(false);
    }

    fn publish_audio_state(&mut self, force: bool) {
        let (token, audio) = {
            let Some(active) = self.game.active.as_mut() else {
                return;
            };
            let audio = active.runtime.audio_state();
            if !force && active.audio == audio {
                return;
            }
            active.audio = audio;
            (active.token, audio)
        };
        self.emit_instance_event(
            token,
            "audioChanged",
            &serde_json::json!({
                "available": audio.available,
                "volume": audio.volume.get(),
                "muted": audio.muted,
            }),
        );
    }

    fn emit_instance_event(
        &mut self,
        token: InstanceToken,
        event_name: &str,
        payload: &serde_json::Value,
    ) {
        let event = serde_json::json!({
            "instanceId": token,
            "eventName": event_name,
            "payload": payload,
        });
        let mut background_changed = false;
        if let Some(layer) = self.game_layer.active_mut() {
            layer.dispatch_emulator_event(&event);
            background_changed = layer.needs_paint();
        }
        if background_changed {
            self.refresh_game_layer();
        }
        self.game_layer.flush(&self.foreground_webview);
    }

    fn process_capture(&mut self) {
        let entered = self.presentation.profile.enter();
        self.process_capture_inner();
        self.presentation
            .profile
            .record(TurnPhase::Capture, entered);
    }

    fn process_capture_inner(&mut self) {
        // Leave the notification latched while draining. Producers can mark
        // `capture_dirty`, but cannot flood the event loop with one user event
        // per captured frame.
        self.streams.dirty.store(false, Ordering::Release);
        let mut changed = false;
        let mut processed = 0;
        while processed < MAX_CAPTURE_EVENTS_PER_TURN {
            let Ok(event) = self.streams.capture.try_recv() else {
                break;
            };
            processed += 1;
            match event {
                SurfaceCaptureEvent::Offered(offer) => {
                    let connection = offer.connection_id();
                    let surface_id = offer.stream().surface_id.0;
                    let Some(active) = &self.game.active else {
                        self.streams.capture.reject(offer);
                        continue;
                    };
                    let replacement = offer.replaced_connection_id();
                    if replacement.is_none()
                        && self.streams.surfaces.len() >= active.options.surfaces.len()
                    {
                        self.streams.capture.reject(offer);
                        continue;
                    }
                    let connected = if let Some(previous) = replacement {
                        self.replace_stream(previous, &offer)
                    } else {
                        self.connect_stream(&offer)
                    };
                    match connected {
                        Ok(()) => match self.streams.capture.accept(offer) {
                            Ok(_) => {
                                if let Some(surface) = self.streams.surfaces.get(&connection) {
                                    self.presentation.overlay.add_surface(
                                        connection,
                                        surface_id,
                                        surface.index,
                                    );
                                }
                                changed = true;
                            }
                            Err(error) => {
                                self.streams.surfaces.remove(&connection);
                                if let Some(merged) = self.presentation.merged.as_mut() {
                                    let _ = Renderer::disconnect_stream(merged, connection);
                                }
                                tracing::error!(
                                    "snowglass: capture stream {connection} disappeared while it was being imported: {error}"
                                );
                            }
                        },
                        Err(error) => {
                            tracing::error!(
                                "snowglass: rejected capture stream {connection}: {error}"
                            );
                            self.streams.capture.reject(offer);
                        }
                    }
                }
                SurfaceCaptureEvent::Frame { connection, update } => {
                    let frame = update.frame;
                    let Some(surface) = self.streams.surfaces.get_mut(&connection) else {
                        continue;
                    };
                    self.presentation.overlay.observe_frame(connection, frame);
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
                    if let Some(merged) = self.presentation.merged.as_mut() {
                        Renderer::set_latest(merged, connection, frame);
                    }
                }
                SurfaceCaptureEvent::Removed { connection, .. } => {
                    self.presentation.overlay.remove_target(connection);
                    self.cancel_pointer_connection(connection);
                    if let Some(publisher) = self
                        .game
                        .active
                        .as_ref()
                        .and_then(|active| active.webrtc.as_ref())
                    {
                        publisher.remove_stream(connection);
                    }
                    if self.streams.surfaces.remove(&connection).is_some() {
                        if let Some(merged) = self.presentation.merged.as_mut() {
                            let _ = Renderer::disconnect_stream(merged, connection);
                        }
                        self.presentation.merged_dirty = true;
                        changed = true;
                    }
                }
            }
        }
        if changed && let Some(token) = self.game.active.as_ref().map(|active| active.token) {
            self.publish_targets_for(token);
        }
        self.draw_unpaced_surfaces();
        self.streams.wake_pending.store(false, Ordering::Release);

        // A producer that ran while the notification was latched could not
        // enqueue another WakeCapture. Carry that work into the next event-loop
        // cycle instead. Resetting the latch before reading `capture_dirty`
        // closes the race: a later producer will enqueue a fresh notification.
        let arrived_while_draining = self.streams.dirty.swap(false, Ordering::AcqRel);
        self.streams.work_pending =
            processed == MAX_CAPTURE_EVENTS_PER_TURN || arrived_while_draining;
    }

    /// Presents pending content only while nothing else is pacing the shell.
    ///
    /// Once the compositor clock is running it owns every present, so that each
    /// one lands at the start of a composition interval rather than wherever
    /// the guest happened to finish a frame. Presenting on arrival puts it at
    /// an arbitrary phase against DWM's sampling deadline, and a guest and a
    /// display at nominally the same rate drift that phase slowly across the
    /// deadline — a stretch of doubled and dropped frames every time it
    /// crosses.
    fn draw_unpaced_surfaces(&mut self) {
        if !self.presentation.clock.is_pacing() {
            self.draw_pending_surfaces();
        }
    }

    fn draw_pending_surfaces(&mut self) {
        self.present_pending_surfaces(Duration::ZERO);
    }

    /// Presents every surface holding new content, waiting up to `permit_wait`
    /// for DXGI to hand back a present slot.
    fn present_pending_surfaces(&mut self, permit_wait: Duration) {
        let entered = self.presentation.profile.enter();
        self.present_pending_surfaces_inner(permit_wait);
        self.presentation
            .profile
            .record(TurnPhase::Present, entered);
    }

    fn present_pending_surfaces_inner(&mut self, permit_wait: Duration) {
        self.draw_merged(permit_wait);
    }

    fn has_pending_surfaces(&self) -> bool {
        // Re-presenting an unchanged shared buffer burns GPU time and can
        // occupy the one-frame DXGI queue when a real guest frame arrives.
        // DirectComposition retains the last buffer, so only new content or
        // chrome needs another present.
        self.presentation.merged.as_ref().is_some_and(|merged| {
            Renderer::has_pending_frame(merged)
                || self.presentation.merged_dirty
                || self.presentation.game_layer_chrome_dirty
        })
    }

    fn connect_stream(
        &mut self,
        offer: &snowglass_runtime::SurfaceOffer,
    ) -> Result<(), Box<dyn Error>> {
        let connection = offer.connection_id();
        let stream = offer.stream();
        let (index, quarter_turns) = self
            .next_target_configuration()
            .ok_or("the shell already has a surface for every tracked render target")?;
        let (width, height) = rotated_extent(stream.width, stream.height, quarter_turns);
        self.ensure_merged_adapter(stream.adapter_luid)?;
        // Every game surface is rendered into one window-sized swapchain.
        // DirectComposition owns only that merged visual, which remains below
        // WebView2 in the composition tree.
        let merged = self
            .presentation
            .merged
            .as_mut()
            .expect("the shared compositor exists by now");
        Renderer::import_surface_offer(merged, offer, quarter_turns)?;
        self.presentation
            .overlay
            .set_backend(format!("{:?}", stream.image_kind));
        self.streams.surfaces.insert(
            connection,
            CapturedSurface {
                index,
                preferred_width: width,
                preferred_height: height,
                viewport_width: width.max(1),
                viewport_height: height.max(1),
                viewport_x: 0,
                viewport_y: 0,
                quarter_turns,
                pointer_events: true,
                z_index: index as i32,
                corner_radius: 0.0,
                game_view_style: None,
                opacity: 1.0,
            },
        );
        self.presentation.merged_dirty = true;
        // The layer already knows where this view goes; it just had nothing to
        // put there until now.
        self.refresh_game_layer();
        Ok(())
    }

    fn replace_stream(
        &mut self,
        previous: u64,
        offer: &snowglass_runtime::SurfaceOffer,
    ) -> Result<(), Box<dyn Error>> {
        let connection = offer.connection_id();
        let stream = offer.stream();
        let surface = self
            .streams
            .surfaces
            .get(&previous)
            .cloned()
            .ok_or("the replaced tracked surface is no longer connected")?;
        self.ensure_merged_adapter(stream.adapter_luid)?;
        let merged = self
            .presentation
            .merged
            .as_mut()
            .expect("the shared compositor exists by now");
        Renderer::import_surface_offer(merged, offer, surface.quarter_turns)?;
        self.streams.surfaces.insert(connection, surface);
        self.streams.surfaces.remove(&previous);
        let _ = Renderer::disconnect_stream(merged, previous);
        self.presentation.overlay.remove_target(previous);
        self.cancel_pointer_connection(previous);
        if let Some(publisher) = self
            .game
            .active
            .as_ref()
            .and_then(|active| active.webrtc.as_ref())
        {
            publisher.remove_stream(previous);
        }
        self.presentation
            .overlay
            .set_backend(format!("{:?}", stream.image_kind));
        self.presentation.merged_dirty = true;
        self.refresh_game_layer();
        Ok(())
    }

    fn ensure_merged_adapter(&mut self, desired: AdapterLuid) -> Result<(), Box<dyn Error>> {
        if self
            .presentation
            .merged
            .as_ref()
            .is_some_and(|merged| merged.adapter_luid() == desired)
        {
            return Ok(());
        }
        if !self.streams.surfaces.is_empty() {
            return Err(
                "captured streams from multiple adapters cannot share one swapchain".into(),
            );
        }

        let size = self.window.inner_size();
        let render_window = SurfaceWindow::new(&self.window, size.width.max(1), size.height.max(1));
        let replacement = D3d12Compositor::new_for_shared_composition(&render_window, desired)?;
        self.game_layer.switch_adapter(desired)?;
        let content = replacement.composition_content()?;
        self.presentation.composition.set_merged_content(&content)?;
        self.presentation.composition.commit()?;
        self.presentation.merged = Some(replacement);
        self.presentation.merged_dirty = true;
        if let Some(layer) = self.game_layer.active_mut() {
            layer.invalidate();
        }
        Ok(())
    }

    /// Placements for the shared swapchain, ordered so a higher `z_index` is
    /// painted later and therefore on top.
    fn merged_plan(&self) -> CompositingPlan {
        let Some(merged) = self.presentation.merged.as_ref() else {
            return CompositingPlan::new([0.0, 0.0, 0.0, 1.0], Vec::new());
        };
        let mut ordered = Renderer::received_surfaces(merged)
            .into_iter()
            .filter_map(|received| {
                let surface = self.streams.surfaces.get(&received.connection)?;
                let rectangle = Rect::new(
                    surface.viewport_x,
                    surface.viewport_y,
                    surface.viewport_width,
                    surface.viewport_height,
                );
                let mut placement = SurfacePlacement::new(
                    received.connection,
                    received.surface_id,
                    rectangle,
                    surface.corner_radius,
                );
                placement.content = fit_surface_content(
                    rectangle,
                    received.image_extent,
                    surface.game_view_style.as_ref(),
                    surface.opacity,
                );
                Some((surface.z_index, PlanLayer::Surface(placement)))
            })
            .collect::<Vec<_>>();
        // The game layer's chrome is composited into this same swapchain rather
        // than sitting in visuals behind it, because the swapchain is opaque and
        // covers the window: anything left outside it would be hidden. Sharing
        // the z-order is what keeps a bezel above the view it overlaps.
        let window = self.window.inner_size();
        ordered.extend(
            self.presentation
                .game_layer_chrome_bands
                .iter()
                .map(|&band| {
                    (
                        chrome_band_z_index(band),
                        PlanLayer::Chrome {
                            id: band as u64,
                            rectangle: snowglass_input::mouse::layout::Rect::new(
                                0,
                                0,
                                window.width,
                                window.height,
                            ),
                        },
                    )
                })
                .collect::<Vec<_>>(),
        );
        ordered.sort_by_key(|(z_index, _)| *z_index);
        CompositingPlan::layered(
            [0.0, 0.0, 0.0, 1.0],
            ordered.into_iter().map(|(_, layer)| layer).collect(),
        )
    }

    /// Draws every stream and the game layer's chrome into the shared
    /// swapchain when either has changed.
    ///
    /// Every stream keeps its newest frame independently. A draw samples all of
    /// those newest frames, so one stream delivering does not make the others
    /// wait for another producer frame, while an unchanged composition remains
    /// resident in DirectComposition without redundant presents.
    fn draw_merged(&mut self, permit_wait: Duration) {
        let plan = self.merged_plan();
        let Some(merged) = self.presentation.merged.as_mut() else {
            return;
        };
        let carried_new_frame = Renderer::has_pending_frame(merged);
        if !merged.begin_frame_within(permit_wait) {
            return;
        }
        match merged.present_with_webrtc(&plan, None) {
            Ok(_) => {
                self.presentation.merged_dirty = false;
                // The draw is what consumed the pending chrome upload.
                self.presentation.game_layer_chrome_dirty = false;
                self.presentation.overlay.record_present(carried_new_frame);
            }
            Err(error) => tracing::error!("snowglass: rendering merged surfaces failed: {error}"),
        }
    }

    fn next_target_configuration(&self) -> Option<(u32, u8)> {
        let in_use = |index| {
            self.streams
                .surfaces
                .values()
                .any(|surface| surface.index == index)
        };
        self.game
            .active
            .as_ref()?
            .options
            .surfaces
            .iter()
            .find(|target| !in_use(target.index))
            .map(|target| (target.index, target.quarter_turns()))
    }

    /// Re-lays out the game layer and moves the compositor to match.
    ///
    /// The chrome is uploaded band by band and the game views are placed
    /// between them, so a document whose bezel sits over a fullscreen view
    /// composites in the order the document painted it.
    ///
    /// Before a stream connects, chrome can be shown in temporary
    /// DirectComposition surfaces. Once the opaque merged swapchain exists,
    /// chrome goes inside it or it would be hidden.
    fn refresh_game_layer(&mut self) {
        let entered = self.presentation.profile.enter();
        self.refresh_game_layer_inner();
        self.presentation
            .profile
            .record(TurnPhase::GameLayer, entered);
    }

    fn refresh_game_layer_inner(&mut self) {
        self.presentation.game_layer_refresh_pending = false;
        let Some(merged) = self.presentation.merged.as_mut() else {
            return;
        };
        let mut bands = 0;
        let (painted, placements) = {
            let Some(layer) = self.game_layer.active_mut() else {
                return;
            };
            let painted = layer.paint(|band, chrome| {
                bands = band + 1;
                merged.set_chrome_layer(band as u64, chrome.handle, chrome.width, chrome.height);
            });
            (painted, layer.placements().to_vec())
        };
        if painted {
            self.presentation.game_layer_chrome_bands = (0..bands).collect();
            self.presentation.game_layer_chrome_dirty = true;
            for band in bands..=GAME_LAYER_VIEWS {
                merged.drop_chrome_layer(band as u64);
            }
        }

        // Placing happens even when nothing was repainted: a view that
        // connected since the last paint still has to be moved into the box
        // that has been waiting for it.
        //
        // A placement names a render target by its tracked index; the shell
        // knows it by the connection that publishes it.
        let surfaces = &self.streams.surfaces;
        let mut layouts = placements
            .iter()
            .filter_map(|placement| {
                let connection = surfaces.iter().find_map(|(&connection, surface)| {
                    (u64::from(surface.index) == placement.source_id).then_some(connection)
                })?;
                let rect = placement.rect;
                Some(SurfaceLayout {
                    connection,
                    x: rect.x0.round() as i32,
                    y: rect.y0.round() as i32,
                    width: rect.width().round().max(0.0) as u32,
                    height: rect.height().round().max(0.0) as u32,
                    z_index: game_view_z_index(placement.band),
                    corner_radius: placement.corner_radius,
                    game_view_style: placement.style.clone(),
                    opacity: placement.opacity,
                    pointer_events: true,
                })
            })
            .collect::<Vec<_>>();
        // A tracked surface outside the viewport is culled by the painter and
        // therefore has no placement. The merged plan retains its previous
        // rectangle until explicitly updated, so absence means hidden rather
        // than "leave it where it was".
        if let Some(active) = &self.game.active {
            for requested in &active.options.surfaces {
                let Some(connection) = surfaces.iter().find_map(|(&connection, surface)| {
                    (surface.index == requested.index).then_some(connection)
                }) else {
                    continue;
                };
                if layouts.iter().any(|layout| layout.connection == connection) {
                    continue;
                }
                layouts.push(SurfaceLayout {
                    connection,
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                    z_index: game_view_z_index(0),
                    corner_radius: 0.0,
                    game_view_style: None,
                    opacity: 1.0,
                    pointer_events: false,
                });
            }
        }
        // Deliberately not `apply_layout`: these are already compositor-owned
        // placements rather than a foreground layout update.
        self.place_surfaces(layouts);
    }

    fn place_surfaces(&mut self, layouts: Vec<SurfaceLayout>) {
        for item in layouts {
            let Some(surface) = self.streams.surfaces.get_mut(&item.connection) else {
                continue;
            };
            let changed = surface.viewport_x != item.x
                || surface.viewport_y != item.y
                || surface.viewport_width != item.width
                || surface.viewport_height != item.height
                || surface.pointer_events != item.pointer_events
                || surface.z_index != item.z_index
                || surface.corner_radius != item.corner_radius
                || surface.game_view_style != item.game_view_style
                || surface.opacity != item.opacity;
            if changed {
                surface.viewport_x = item.x;
                surface.viewport_y = item.y;
                surface.viewport_width = item.width;
                surface.viewport_height = item.height;
                surface.pointer_events = item.pointer_events;
                surface.z_index = item.z_index;
                surface.corner_radius = item.corner_radius;
                surface.game_view_style = item.game_view_style;
                surface.opacity = item.opacity;
                self.presentation.merged_dirty = true;
            }
        }
    }

    /// Dispatches a regular DOM mouse event to the Deno-backed game layer.
    /// Script claims the pointer from the guest by cancelling the event.
    fn game_layer_mouse(
        &mut self,
        kind: &str,
        x: f32,
        y: f32,
        button: u16,
        pressed: Option<bool>,
    ) -> bool {
        let flag = match button {
            0 => 1,
            1 => 4,
            2 => 2,
            3 => 8,
            4 => 16,
            _ => 0,
        };
        let previous_buttons = pressed
            .map(|pressed| self.input.set_mouse_button(flag, pressed))
            .unwrap_or(self.input.mouse_buttons);
        let buttons = self.input.mouse_buttons;
        let Some(layer) = self.game_layer.active_mut() else {
            self.input.mouse_buttons = previous_buttons;
            return false;
        };
        let prevented = layer.mouse_event(DomMouseEvent {
            kind,
            x,
            y,
            button: button.min(u16::from(u8::MAX)) as u8,
            buttons,
            ctrl_key: self.input.dom_mouse_keys & DOM_MK_CONTROL != 0,
            shift_key: self.input.dom_mouse_keys & DOM_MK_SHIFT != 0,
        });
        let needs_paint = layer.needs_paint();
        if !prevented {
            self.input.mouse_buttons = previous_buttons;
        }
        if needs_paint {
            if self.presentation.clock.is_pacing() {
                self.presentation.game_layer_refresh_pending = true;
            } else {
                self.refresh_game_layer();
            }
        }
        self.flush_game_layer_bridge();
        prevented
    }

    fn game_layer_keyboard(&mut self, kind: &str, key: String, code: String, repeat: bool) -> bool {
        let Some(layer) = self.game_layer.active_mut() else {
            return false;
        };
        let prevented = layer.keyboard_event(DomKeyboardEvent {
            kind,
            key,
            code,
            repeat,
            alt_key: self.input.dom_alt_key,
            ctrl_key: self.input.dom_mouse_keys & DOM_MK_CONTROL != 0,
            shift_key: self.input.dom_mouse_keys & DOM_MK_SHIFT != 0,
        });
        if layer.needs_paint() {
            self.refresh_game_layer();
        }
        self.flush_game_layer_bridge();
        prevented
    }

    fn refresh_pending_game_layer(&mut self) {
        if std::mem::take(&mut self.presentation.game_layer_refresh_pending) {
            self.refresh_game_layer();
        }
    }

    fn foreground_hit_test(&self) -> bool {
        let x = i64::from(self.input.cursor.x);
        let y = i64::from(self.input.cursor.y);
        self.input.foreground_hit_regions.iter().any(|region| {
            let left = i64::from(region.x);
            let top = i64::from(region.y);
            x >= left
                && x < left + i64::from(region.width)
                && y >= top
                && y < top + i64::from(region.height)
        })
    }

    fn pointer_presentation(&self) -> (snowglass_renderer::Presentation, CompositingPlan) {
        let mut received = Vec::new();
        let mut placements = Vec::new();
        let mut surfaces = self
            .streams
            .surfaces
            .iter()
            .filter(|(_, surface)| surface.pointer_events)
            .collect::<Vec<_>>();
        surfaces.sort_by_key(|(_, surface)| std::cmp::Reverse(surface.z_index));
        for (&connection, surface) in surfaces {
            let received_surfaces = match self.presentation.merged.as_ref() {
                Some(merged) => Renderer::received_surfaces(merged)
                    .into_iter()
                    .filter(|received| received.connection == connection)
                    .collect(),
                None => Vec::new(),
            };
            for mut received_surface in received_surfaces {
                received_surface.frame.presentation_transform = shell_pointer_transform(
                    received_surface.frame.presentation_transform,
                    received_surface.frame.presentation_width,
                    received_surface.frame.presentation_height,
                );
                let rectangle = Rect::new(
                    surface.viewport_x,
                    surface.viewport_y,
                    surface.viewport_width,
                    surface.viewport_height,
                );
                let mut placement = SurfacePlacement::new(
                    connection,
                    received_surface.surface_id,
                    rectangle,
                    surface.corner_radius,
                );
                placement.content = fit_surface_content(
                    rectangle,
                    received_surface.image_extent,
                    surface.game_view_style.as_ref(),
                    surface.opacity,
                );
                placements.push(placement);
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
        if !self.input.pointer_routing.send(
            phase,
            button,
            horizontal,
            vertical,
            &presentation,
            &plan,
            &self.streams.pointer,
        ) {
            if phase == PointerPhase::Move {
                self.input.direct_input.reset_mouse_position();
                if let Some(provider) = self.input_provider() {
                    provider.set_pointer(None);
                }
            }
            return;
        }
        let Some(sent) = self.input.pointer_routing.last_sent() else {
            return;
        };
        match phase {
            PointerPhase::Move => {
                self.input.direct_input.mouse_position(
                    sent.surface_id,
                    sent.x,
                    sent.y,
                    sent.coordinate_width,
                    sent.coordinate_height,
                );
                if let Some(provider) = self.input_provider() {
                    provider.set_pointer(Some(pointer_axes(sent.normalized_x, sent.normalized_y)));
                }
            }
            PointerPhase::ButtonDown | PointerPhase::ButtonUp => self
                .input
                .direct_input
                .mouse_button(button as u16, phase == PointerPhase::ButtonDown),
            PointerPhase::Wheel => self.input.direct_input.mouse_wheel(vertical),
            PointerPhase::Cancel => {}
        }
    }

    fn set_input_priority(&mut self, priority: InputPriority) {
        let priority = if priority == InputPriority::Game && self.game.active.is_none() {
            InputPriority::Foreground
        } else {
            priority
        };
        if self.input.priority == priority {
            // A foreground pointer interaction moves focus into WebView2
            // without changing the logical priority. Re-activating game input
            // must still return keyboard focus to the shell HWND.
            if priority == InputPriority::Game {
                self.focus_game_input();
            }
            return;
        }
        if let Some(connection) = self.input.pointer_routing.captured_connection() {
            self.cancel_pointer_connection(connection);
        }
        self.input.direct_input.reset_mouse_position();
        if let Some(provider) = self.input_provider() {
            provider.clear_keys();
            provider.set_pointer(None);
            provider.set_enabled(priority == InputPriority::Game);
        }
        self.input.priority = priority;
        match priority {
            InputPriority::Foreground => {
                // SAFETY: the controller is live while the WebView is retained.
                let _ = unsafe {
                    self.foreground_webview
                        .controller()
                        .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
                };
            }
            InputPriority::Game => self.focus_game_input(),
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

    fn focus_game_input(&self) {
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

    fn cancel_pointer_connection(&mut self, connection: u64) {
        let (presentation, plan) = self.pointer_presentation();
        if let Some(buttons) = self.input.pointer_routing.cancel(
            connection,
            &presentation,
            &plan,
            &self.streams.pointer,
        ) {
            self.input.direct_input.release_mouse_buttons(buttons);
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
                self.input.dom_mouse_keys |= flag;
            } else {
                self.input.dom_mouse_keys &= !flag;
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
                COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS(self.input.dom_mouse_keys as i32),
                mouse_data,
                self.input.cursor,
            )
        };
    }

    fn publish_input_state(&mut self) {
        if Instant::now() < self.input.next_publish {
            return;
        }
        self.input.next_publish = Instant::now() + Duration::from_millis(16);
        let states = self
            .input_provider()
            .map(Sdl3Provider::states)
            .unwrap_or_default();
        let physical_devices = self.input.transport.devices();
        let guide_pressed = physical_devices
            .iter()
            .any(|device| device.buttons.contains(&(SdlButton::Guide, true)));
        if guide_pressed
            && !self.input.guide_chord_held
            && self.input.priority == InputPriority::Game
        {
            self.set_input_priority(InputPriority::Foreground);
        }
        self.input.guide_chord_held = guide_pressed;
        self.presentation.overlay.diagnostics.input = InputDiagnostics {
            active: self.input.priority == InputPriority::Game && self.game.active.is_some(),
            slots: self
                .game
                .active
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
        let foreground_priority = self.input.priority == InputPriority::Foreground;
        for device in physical_devices {
            let assigned = states
                .iter()
                .enumerate()
                .filter_map(|(slot, state)| {
                    state
                        .as_ref()
                        .is_some_and(|state| state.device_id == device.device_id)
                        .then_some(slot)
                })
                .collect::<Vec<_>>();
            let mapped_states = assigned
                .iter()
                .filter_map(|&slot| states.get(slot).and_then(Option::as_ref))
                .collect::<Vec<_>>();
            let primary = mapped_states.first().copied();
            let buttons = device
                .buttons
                .iter()
                .map(|&(button, pressed)| {
                    let name = format!("{button:?}");
                    let global = button == SdlButton::Guide;
                    let consumed = global
                        || mapped_states
                            .iter()
                            .any(|state| state.mapped_buttons.contains(&button));
                    if foreground_priority || !consumed {
                        if !global {
                            next_controls.insert(
                                format!("{}:button:{name}", device.device_id),
                                i32::from(pressed),
                            );
                        }
                    }
                    serde_json::json!({
                        "button": name,
                        "pressed": pressed,
                        "mapped": consumed,
                    })
                })
                .collect::<Vec<_>>();
            let axes = device
                .axes
                .iter()
                .map(|&(axis, value)| {
                    let name = format!("{axis:?}");
                    let consumed = mapped_states
                        .iter()
                        .any(|state| state.mapped_axes.contains(&axis));
                    if foreground_priority || !consumed {
                        next_controls.insert(
                            format!("{}:axis:{name}", device.device_id),
                            i32::from(value),
                        );
                    }
                    serde_json::json!({
                        "axis": name,
                        "value": value,
                        "mapped": consumed,
                    })
                })
                .collect::<Vec<_>>();
            devices.push(serde_json::json!({
                "index": device.enumeration_index,
                "slot": device.enumeration_index,
                "deviceId": device.device_id,
                "name": device.name,
                "guid": device.guid,
                "vendor": device.vendor,
                "product": device.product,
                "assignedPorts": assigned,
                "layout": primary.map(|state| state.layout_id.as_str()),
                "buttons": buttons,
                "axes": axes,
                "mapped": {
                    "buttons": primary.map_or(0, |state| state.mapped.buttons),
                    "leftStick": primary.map_or([0; 2], |state| state.mapped.left_stick),
                    "rightStick": primary.map_or([0; 2], |state| state.mapped.right_stick),
                    "leftTrigger": primary.map_or(0, |state| state.mapped.left_trigger),
                    "rightTrigger": primary.map_or(0, |state| state.mapped.right_trigger),
                    "pointer": primary.map_or([0; 2], |state| state.mapped.pointer),
                    "accelerometer": primary.map_or([0.0; 3], |state| state.mapped.accelerometer),
                    "gyroscope": primary.map_or([0.0; 3], |state| state.mapped.gyroscope),
                }
            }));
        }
        for (slot, state) in states.iter().enumerate().filter_map(|(slot, state)| {
            state
                .as_ref()
                .filter(|state| state.device_id == "keyboard-mouse")
                .map(|state| (slot, state))
        }) {
            devices.push(serde_json::json!({
                "index": devices.len(),
                "slot": devices.len(),
                "deviceId": state.device_id,
                "name": state.name,
                "guid": state.guid,
                "vendor": state.vendor,
                "product": state.product,
                "assignedPorts": [slot],
                "layout": state.layout_id,
                "buttons": state.buttons.iter().map(|(button, pressed)| serde_json::json!({
                    "button": format!("{button:?}"),
                    "pressed": pressed,
                    "mapped": true,
                })).collect::<Vec<_>>(),
                "axes": state.axes.iter().map(|(axis, value)| serde_json::json!({
                    "axis": format!("{axis:?}"),
                    "value": value,
                    "mapped": true,
                })).collect::<Vec<_>>(),
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
            if self.input.ui_controls.get(key).copied().unwrap_or_default() != value {
                routed.push(control_event(key, value));
            }
        }
        for (key, &value) in &self.input.ui_controls {
            if value != 0 && !next_controls.contains_key(key) {
                routed.push(control_event(key, 0));
            }
        }
        self.input.ui_controls = next_controls;
        let controllers = serde_json::Value::Array(devices.clone());
        self.game_layer.set_input_state(
            &controllers,
            if foreground_priority {
                "foreground"
            } else {
                "game"
            },
        );
        let Ok(json) = serde_json::to_string(&devices) else {
            return;
        };
        let state_changed = json != self.input.last_published_state;
        if !state_changed && routed.is_empty() {
            return;
        }
        self.presentation.overlay.next_repaint = Instant::now();
        if state_changed {
            self.input.last_published_state.clone_from(&json);
        }
        let routed = serde_json::to_string(&routed).unwrap_or_else(|_| "[]".to_owned());
        let script = format!("window.__snowglassSetInputState?.({json}, {routed});");
        let _ = self.foreground_webview.evaluate_script(&script);
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

    fn publish_targets_for(&mut self, token: InstanceToken) {
        let mut targets = self
            .streams
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
        self.emit_instance_event(token, "surfacesChanged", &payload);
    }

    fn poll_target(&mut self) {
        let Some(active) = self.game.active.as_ref() else {
            return;
        };
        let finished = matches!(
            active.runtime.state(),
            snowglass_runtime::InstanceState::Exited(_)
        );
        if finished {
            self.streams.capture.detach();
            self.input.transport.detach();
            self.finish_active_game();
        } else {
            self.publish_audio_state(false);
        }
    }
}

impl Drop for ShellState {
    fn drop(&mut self) {
        for &connection in self.streams.surfaces.keys() {
            self.streams.capture.revoke(connection);
            if let Some(merged) = self.presentation.merged.as_mut() {
                let _ = Renderer::disconnect_stream(merged, connection);
            }
        }
        if let Some(active) = self.game.active.as_mut() {
            active.runtime.close();
        }
    }
}

struct ShellApplication {
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    options: Option<ShellOptions>,
    statistics_recorder: Option<RecorderHandle>,
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
        let statistics_recorder = self
            .statistics_recorder
            .take()
            .expect("statistics recorder is consumed exactly once");
        match ShellState::new(event_loop, self.proxy.clone(), options, statistics_recorder) {
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
            // Drain promptly rather than waiting for `about_to_wait`. A busy
            // window can keep delivering compositor, WebView, and redraw
            // events for most of a frame; postponing the drain holds the
            // producer's three export slots and eventually makes it skip
            // captures. `process_capture` is bounded, and carries any overflow
            // into `about_to_wait`.
            UserEvent::WakeCapture => state.process_capture(),
            UserEvent::CompositorTick => {
                state.presentation.clock.clear_tick();
                state.presentation.overlay.record_clock();
                // Mouse input can run several times faster than the display.
                // Paint its latest position once for this scanout rather than
                // blocking on a Vello repaint for every intermediate event.
                state.refresh_pending_game_layer();
                // Present here, in phase with DWM, rather than when a guest
                // frame happened to arrive. The chrome refreshed just above
                // rides the same present.
                state.present_pending_surfaces(PRESENT_PERMIT_WAIT);
            }
            UserEvent::FrontendReady {
                background,
                background_error,
            } => state.frontend_ready(background, background_error),
            UserEvent::ForegroundHitRegions(regions) => {
                state.input.foreground_hit_regions = regions
                    .into_iter()
                    .filter(|region| region.width != 0 && region.height != 0)
                    .collect();
            }
            UserEvent::GameLayerMessage(message) => {
                state.game_layer.post_message(&message);
                state.flush_game_layer_bridge();
            }
            UserEvent::GameLayerProxy(request) => {
                state
                    .game_layer
                    .proxy_request(&request, &state.foreground_webview);
                state.flush_game_layer_bridge();
            }
            UserEvent::InputConfigure {
                request_id,
                transaction,
            } => state.configure_input(request_id, transaction, false),
            UserEvent::EmulatorCommand(command) => state.apply_emulator_command(command),
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.input.priority != InputPriority::Game {
            return;
        }
        if let DeviceEvent::MouseMotion { delta } = event
            && let Some(provider) = state.input_provider()
        {
            provider.add_mouse_delta([delta.0, delta.1]);
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
        let response = state
            .presentation
            .overlay
            .state
            .on_window_event(&state.window, &event);
        let mut consumed = response.consumed;
        if consumed {
            // egui took this event, so it is being interacted with and gets to
            // set its own repaint rate until the interaction settles.
            state.presentation.overlay.note_interaction(Instant::now());
        }
        if response.repaint {
            state.window.request_redraw();
        }
        if !consumed
            && let WindowEvent::KeyboardInput { event, .. } = &event
            && let winit::keyboard::PhysicalKey::Code(code) = event.physical_key
        {
            let code = format!("{code:?}");
            let key = match &event.logical_key {
                Key::Named(key) => format!("{key:?}"),
                Key::Character(key) => key.to_string(),
                Key::Dead(_) => "Dead".into(),
                Key::Unidentified(_) => "Unidentified".into(),
            };
            consumed = state.game_layer_keyboard(
                if event.state == ElementState::Pressed {
                    "keydown"
                } else {
                    "keyup"
                },
                key,
                code,
                event.repeat,
            );
        }
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => event_loop.exit(),
            WindowEvent::Resized(size) if size.width != 0 && size.height != 0 => {
                let bounds = wry::Rect {
                    position: wry::dpi::PhysicalPosition::new(0, 0).into(),
                    size: wry::dpi::PhysicalSize::new(size.width, size.height).into(),
                };
                if let Err(error) = state.foreground_webview.set_bounds(bounds) {
                    tracing::error!("snowglass: resizing foreground WebView2 failed: {error}");
                }
                if let Err(error) = state.presentation.overlay.resize(&state.window) {
                    tracing::error!("snowglass: resizing egui composition overlay failed: {error}");
                }
                if let Some(layer) = state.game_layer.active_mut() {
                    layer.resize(size.width, size.height);
                    state.refresh_game_layer();
                }
                // The shared swapchain and the background both cover the whole
                // window, so the window is what resizes them, not a DOM box.
                if let Some(merged) = state.presentation.merged.as_mut() {
                    let render_window = SurfaceWindow::new(&state.window, size.width, size.height);
                    match Renderer::recreate_swapchain(merged, &render_window) {
                        Ok(true) => state.presentation.merged_dirty = true,
                        Ok(false) => {}
                        Err(error) => {
                            tracing::error!(
                                "snowglass: resizing the shared swapchain failed: {error}"
                            );
                        }
                    }
                }
                state.window.request_redraw();
            }
            WindowEvent::Focused(false) => {
                if let Some(connection) = state.input.pointer_routing.captured_connection() {
                    state.cancel_pointer_connection(connection);
                }
                for (button, flag) in [(0, 1), (1, 4), (2, 2), (3, 8), (4, 16)] {
                    if state.input.mouse_buttons & flag != 0 {
                        state.game_layer_mouse(
                            "mouseup",
                            state.input.cursor.x as f32,
                            state.input.cursor.y as f32,
                            button,
                            Some(false),
                        );
                    }
                }
                state.input.mouse_buttons = 0;
                state.input.dom_alt_key = false;
                if let Some(provider) = state.input_provider() {
                    provider.clear_keys();
                    provider.set_pointer(None);
                }
                state.input.dom_mouse_keys &= DOM_MK_SHIFT | DOM_MK_CONTROL;
                state.input.foreground_mouse_capture = false;
                state.input.direct_input.reset_mouse_position();
            }
            WindowEvent::KeyboardInput { event, .. }
                if state.input.priority == InputPriority::Game =>
            {
                if let winit::keyboard::PhysicalKey::Code(code) = event.physical_key {
                    let key = format!("{code:?}");
                    let pressed = event.state == ElementState::Pressed;
                    if consumed {
                        // A background DOM shortcut claimed the event.
                    } else if let Some(provider) = state.input_provider()
                        && provider.is_key_mapped(&key)
                    {
                        provider.set_key(key, pressed);
                    } else {
                        state.publish_keyboard_input(&key, pressed, event.repeat);
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                state.input.dom_alt_key = modifiers.state().alt_key();
                self::set_modifier(
                    &mut state.input.dom_mouse_keys,
                    DOM_MK_SHIFT,
                    modifiers.state().shift_key(),
                );
                self::set_modifier(
                    &mut state.input.dom_mouse_keys,
                    DOM_MK_CONTROL,
                    modifiers.state().control_key(),
                );
            }
            WindowEvent::CursorMoved { position, .. }
                if !consumed
                    || state.input.pointer_routing.captured_connection().is_some()
                    || state.input.dom_mouse_keys & DOM_MOUSE_BUTTONS != 0
                    || state.input.foreground_mouse_capture =>
            {
                state.input.cursor = POINT {
                    x: position.x.round() as i32,
                    y: position.y.round() as i32,
                };
                if state.input.priority == InputPriority::Foreground
                    || state.input.foreground_mouse_capture
                    || (!consumed
                        && state.input.pointer_routing.captured_connection().is_none()
                        && state.input.mouse_buttons == 0
                        && state.foreground_hit_test())
                {
                    state.route_foreground_mouse(WM_MOUSEMOVE, None, 0);
                    return;
                }
                // WebView2 still needs the move when the pointer leaves a
                // foreground region so CSS :hover and mouseleave state clear.
                // A move outside a published region does not claim the event,
                // so it continues down to Blitz.
                state.route_foreground_mouse(WM_MOUSEMOVE, None, 0);
                // Blitz owns the background DOM and its tracked-surface
                // placements. It only sees events that neither egui nor the
                // foreground WebView claimed.
                if state.game_layer_mouse(
                    "mousemove",
                    position.x as f32,
                    position.y as f32,
                    0,
                    None,
                ) {
                    return;
                }
                state
                    .input
                    .pointer_routing
                    .set_cursor(position.x, position.y);
                state.route_pointer(PointerPhase::Move, 0, 0, 0);
            }
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } if !consumed
                || state.input.pointer_routing.captured_connection().is_some()
                || state.input.dom_mouse_keys & DOM_MOUSE_BUTTONS != 0
                || state.input.foreground_mouse_capture =>
            {
                if let Some(button) = mouse_button(button) {
                    let dom_button = match button {
                        1 => 0,
                        2 => 2,
                        3 => 1,
                        value => value.saturating_sub(1),
                    };
                    let foreground_hit = !consumed
                        && state.input.pointer_routing.captured_connection().is_none()
                        && state.input.mouse_buttons == 0
                        && state.foreground_hit_test();
                    if state.input.priority == InputPriority::Foreground
                        || state.input.foreground_mouse_capture
                        || foreground_hit
                    {
                        if button_state == ElementState::Pressed && foreground_hit {
                            state.input.foreground_mouse_capture = true;
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
                            && state.input.dom_mouse_keys & DOM_MOUSE_BUTTONS == 0
                        {
                            state.input.foreground_mouse_capture = false;
                        }
                        return;
                    }
                    let game_layer_kind = if button_state == ElementState::Pressed {
                        "mousedown"
                    } else {
                        "mouseup"
                    };
                    if state.game_layer_mouse(
                        game_layer_kind,
                        state.input.cursor.x as f32,
                        state.input.cursor.y as f32,
                        dom_button,
                        Some(button_state == ElementState::Pressed),
                    ) {
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
                if state.input.priority == InputPriority::Foreground
                    || state.input.foreground_mouse_capture
                    || (state.input.pointer_routing.captured_connection().is_none()
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
                let entered = state.presentation.profile.enter();
                if let Err(error) = state.presentation.overlay.draw(&state.window) {
                    tracing::error!("snowglass: drawing egui composition overlay failed: {error}");
                }
                state
                    .presentation
                    .profile
                    .record(TurnPhase::Overlay, entered);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let mut deadline = now + Duration::from_millis(100);
        if let Some(state) = self.state.as_mut() {
            if state.streams.work_pending {
                state.process_capture();
            }
            // A frame the compositor deferred keeps its `latest` slot, so it
            // has to be retried from here. `process_capture` only runs when a
            // producer posts new work, which would hold an already-finished
            // frame back until the guest renders its next one.
            //
            // While the compositor clock is pacing, its tick is that retry, and
            // it lands in phase with DWM instead of at an arbitrary point in
            // the interval. A clock that cannot be waited on at all reports
            // itself unhealthy, which is what turns this fallback back on.
            if state.has_pending_surfaces() && !state.presentation.clock.is_pacing() {
                state.draw_pending_surfaces();
                deadline = deadline.min(now + PENDING_SURFACE_RETRY);
            }
            let entered = state.presentation.profile.enter();
            state.publish_input_state();
            state.presentation.profile.record(TurnPhase::Input, entered);
            state.poll_target();
            if state.game_layer.poll_runtime() {
                state.flush_game_layer_bridge();
                // Deno owns browser timers, fetch, WebSockets, promises, and
                // inspector traffic. Keep that event loop moving even when
                // the native window has no other reason to wake.
                deadline = deadline.min(now + Duration::from_millis(16));
            }
            if state.presentation.overlay.repaint_due(now) {
                state.window.request_redraw();
            }
            deadline = deadline.min(state.presentation.overlay.next_repaint());
            // Cheap enough to reconcile every turn, which keeps pacing correct
            // no matter which path added or dropped a stream.
            state
                .presentation
                .clock
                .set_active(!state.streams.surfaces.is_empty());
            if state.streams.work_pending {
                deadline = deadline.min(now + PENDING_SURFACE_RETRY);
            }
            state.presentation.profile.finish_turn(Instant::now());
            let report = std::mem::take(&mut state.presentation.profile.report);
            state.presentation.overlay.set_note(&report);
            state.presentation.profile.report = report;
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

/// Resolves CSS replaced-element sizing without changing the layout rectangle
/// used for guest resizing and input. Overflow is converted into a source crop
/// so the compositor never draws outside the tracked surface's box.
fn fit_surface_content(
    rectangle: Rect,
    source: Extent,
    style: Option<&GameViewStyle>,
    opacity: f32,
) -> SurfaceContent {
    use style::{
        properties::generated::longhands::object_fit::computed_value::T as ObjectFit,
        values::computed::CSSPixelLength,
    };

    let mut content = SurfaceContent::fill(rectangle);
    content.opacity = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if rectangle.is_empty() || source.is_empty() {
        return content;
    }

    let container_width = rectangle.width as f32;
    let container_height = rectangle.height as f32;
    let source_width = source.width as f32;
    let source_height = source.height as f32;
    let Some(style) = style else {
        return content;
    };

    let contain_scale = (container_width / source_width).min(container_height / source_height);
    let cover_scale = (container_width / source_width).max(container_height / source_height);
    let (object_width, object_height) = match style.object_fit {
        ObjectFit::Fill => (container_width, container_height),
        ObjectFit::Contain => (source_width * contain_scale, source_height * contain_scale),
        ObjectFit::Cover => (source_width * cover_scale, source_height * cover_scale),
        ObjectFit::None => (source_width, source_height),
        ObjectFit::ScaleDown
            if source_width <= container_width && source_height <= container_height =>
        {
            (source_width, source_height)
        }
        ObjectFit::ScaleDown => (source_width * contain_scale, source_height * contain_scale),
    };

    let x_offset = style
        .object_position
        .horizontal
        .resolve(CSSPixelLength::new(container_width - object_width))
        .px();
    let y_offset = style
        .object_position
        .vertical
        .resolve(CSSPixelLength::new(container_height - object_height))
        .px();
    let container_left = rectangle.x as f32;
    let container_top = rectangle.y as f32;
    let container_right = container_left + container_width;
    let container_bottom = container_top + container_height;
    let object_left = container_left + x_offset;
    let object_top = container_top + y_offset;
    let object_right = object_left + object_width;
    let object_bottom = object_top + object_height;
    let draw_left = object_left.max(container_left).min(container_right);
    let draw_top = object_top.max(container_top).min(container_bottom);
    let draw_right = object_right.min(container_right).max(container_left);
    let draw_bottom = object_bottom.min(container_bottom).max(container_top);

    content.destination = [draw_left, draw_top, draw_right, draw_bottom];
    if draw_right > draw_left && draw_bottom > draw_top {
        content.source = [
            ((draw_left - object_left) / object_width).clamp(0.0, 1.0),
            ((draw_top - object_top) / object_height).clamp(0.0, 1.0),
            ((draw_right - object_left) / object_width).clamp(0.0, 1.0),
            ((draw_bottom - object_top) / object_height).clamp(0.0, 1.0),
        ];
    }
    content
}

const fn rotated_extent(width: u32, height: u32, quarter_turns: u8) -> (u32, u32) {
    if quarter_turns % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    }
}

fn hub_source(selector: snowglass_input::provider::sdl3::DeviceSelector) -> SourceSelector {
    use snowglass_input::provider::sdl3::DeviceSelector;
    match selector {
        DeviceSelector::Keyboard => SourceSelector::KeyboardMouse,
        DeviceSelector::Index(index) => SourceSelector::SdlIndex { index },
        DeviceSelector::Name(name) => SourceSelector::SdlName { name },
        DeviceSelector::Guid(guid) => SourceSelector::SdlGuid {
            guid,
            duplicate_ordinal: 0,
        },
        DeviceSelector::GuidOrdinal {
            guid,
            duplicate_ordinal,
        } => SourceSelector::SdlGuid {
            guid,
            duplicate_ordinal,
        },
        DeviceSelector::DeviceId(device_id) => SourceSelector::Device { device_id },
    }
}

fn hub_binding(binding: &snowglass_input::layout::InputBinding) -> BindingOverride {
    use snowglass_input::layout::InputBinding;
    match binding {
        InputBinding::Button { button } => BindingOverride::Button {
            source: SourceControl::GamepadButton { button: *button },
        },
        InputBinding::Axis { axis, invert } => BindingOverride::Axis {
            source: SourceControl::GamepadAxis { axis: *axis },
            transform: snowglass_input::hub::AxisTransform {
                invert: *invert,
                ..Default::default()
            },
        },
        InputBinding::Disabled { .. } => BindingOverride::Disabled,
    }
}

fn binding_supported_by_attached_provider(
    binding: &BindingOverride,
    source: Option<&SourceSelector>,
) -> bool {
    if matches!(
        binding,
        BindingOverride::Axis {
            source: SourceControl::MouseDelta { .. } | SourceControl::MousePosition { .. },
            ..
        }
    ) {
        return true;
    }
    if matches!(source, Some(SourceSelector::KeyboardMouse)) {
        return matches!(
            binding,
            BindingOverride::Button {
                source: SourceControl::Key { .. }
            } | BindingOverride::Axis {
                source: SourceControl::Key { .. },
                ..
            } | BindingOverride::Disabled
        );
    }
    binding.sdl_binding().is_some()
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
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    let statistics_recorder = snowglass_statistics::install()?;
    let options = cli::parse()?;
    let _timer_resolution = TimerResolution::raise();
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut application = ShellApplication {
        proxy,
        options: Some(options),
        statistics_recorder: Some(statistics_recorder),
        state: None,
        error: None,
    };
    event_loop.run_app(&mut application)?;
    application.error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use style::{
        properties::generated::longhands::object_fit::computed_value::T as ObjectFit,
        values::computed::{LengthPercentage, Percentage, position::Position},
    };

    fn game_view_style(object_fit: ObjectFit, x: f32, y: f32) -> GameViewStyle {
        GameViewStyle {
            object_fit,
            object_position: Position::new(
                LengthPercentage::new_percent(Percentage(x)),
                LengthPercentage::new_percent(Percentage(y)),
            ),
        }
    }

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

    #[test]
    fn cover_crops_the_source_without_changing_the_layout_box() {
        let rectangle = Rect::new(10, 20, 200, 200);
        let style = game_view_style(ObjectFit::Cover, 0.5, 0.5);
        let content = fit_surface_content(rectangle, Extent::new(400, 200), Some(&style), 0.35);

        assert_eq!(content.destination, [10.0, 20.0, 210.0, 220.0]);
        assert_eq!(content.source, [0.25, 0.0, 0.75, 1.0]);
        assert!((content.opacity - 0.35).abs() < f32::EPSILON);
        let placement = SurfacePlacement {
            connection: 1,
            surface_id: 2,
            rectangle,
            content,
            corner_radius: 0.0,
        };
        assert_eq!(
            CompositingPlan::new([0.0; 4], vec![placement]).rectangle(1, 2),
            Some(rectangle)
        );
    }

    #[test]
    fn contain_and_object_position_move_only_the_draw_quad() {
        let style = game_view_style(ObjectFit::Contain, 1.0, 1.0);
        let content = fit_surface_content(
            Rect::new(10, 20, 200, 200),
            Extent::new(400, 200),
            Some(&style),
            1.0,
        );

        assert_eq!(content.destination, [10.0, 120.0, 210.0, 220.0]);
        assert_eq!(content.source, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn none_is_clipped_at_the_object_position() {
        let style = game_view_style(ObjectFit::None, 1.0, 1.0);
        let content = fit_surface_content(
            Rect::new(0, 0, 200, 100),
            Extent::new(400, 200),
            Some(&style),
            1.0,
        );

        assert_eq!(content.destination, [0.0, 0.0, 200.0, 100.0]);
        assert_eq!(content.source, [0.5, 0.5, 1.0, 1.0]);
    }

    #[test]
    fn attached_provider_accepts_mouse_axis_but_not_mouse_button_expressions() {
        let mouse_axis = BindingOverride::Axis {
            source: SourceControl::MouseDelta {
                axis: snowglass_input::hub::PointerAxis::X,
            },
            transform: Default::default(),
        };
        assert!(binding_supported_by_attached_provider(
            &mouse_axis,
            Some(&SourceSelector::SdlIndex { index: 0 })
        ));

        let mouse_button = BindingOverride::Button {
            source: SourceControl::MouseButton { button: 1 },
        };
        assert!(!binding_supported_by_attached_provider(
            &mouse_button,
            Some(&SourceSelector::KeyboardMouse)
        ));
    }
}
