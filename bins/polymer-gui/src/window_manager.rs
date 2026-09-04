//! Window, native target, and Windows.UI.Composition ownership for the shell.

#![allow(unsafe_code)]

use polymer_egui::{D3d12EguiPainter, DebugOverlayModel, InputDiagnostics, draw_debug_overlay};
use polymer_platform_windows::{performance_counter, performance_frequency};
use polymer_renderer::{CompositingPlan, D3d12Compositor, Extent, Renderer};
use polymer_statistics::{FrameMetrics, LiveStatistics, PresentationMetrics, RecorderHandle};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    cell::Cell,
    collections::HashMap,
    error::Error,
    ffi::c_void,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use windows::{
    System::DispatcherQueueController,
    UI::Composition::{
        CompositionStretch, Compositor, ContainerVisual, Core::CompositorController,
        Desktop::DesktopWindowTarget, SpriteVisual,
    },
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, WAIT_FAILED, WAIT_OBJECT_0},
        Graphics::DirectComposition::DCompositionWaitForCompositorClock,
        Media::{TIMERR_NOERROR, timeBeginPeriod, timeEndPeriod},
        System::{
            Threading::{CreateEventW, INFINITE, SetEvent},
            WinRT::{
                Composition::{ICompositorDesktopInterop, ICompositorInterop},
                CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
                DispatcherQueueOptions, RO_INIT_MULTITHREADED, RO_INIT_SINGLETHREADED,
                RoInitialize, RoUninitialize,
            },
        },
    },
    core::{IUnknown, Interface},
};
use windows_numerics::Vector2;

/// Initialises the Windows Runtime apartment used by the desktop visual layer.
/// The shell owns one current-thread apartment for its entire event loop.
pub(crate) struct WinRtApartment {
    _dispatcher: DispatcherQueueController,
    _apartment: RoApartment,
}

struct RoApartment;

thread_local! {
    static WORKER_WINRT_INITIALIZED: Cell<bool> = const { Cell::new(false) };
}

pub(crate) struct WorkerWinRtApartment;

impl WorkerWinRtApartment {
    pub(crate) fn initialize_current_thread() {
        // SAFETY: Tokio calls this once on each new worker thread. The matching
        // stop hook below balances every successful initialization.
        let initialized = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
        WORKER_WINRT_INITIALIZED.set(initialized);
        if !initialized {
            tracing::warn!("polymer: could not initialize WinRT on a Tokio worker");
        }
    }

    pub(crate) fn uninitialize_current_thread() {
        if WORKER_WINRT_INITIALIZED.replace(false) {
            // SAFETY: Tokio invokes its stop hook on the same worker thread
            // whose successful initialization set this thread-local flag.
            unsafe { RoUninitialize() };
        }
    }
}

impl WinRtApartment {
    pub(crate) fn initialize() -> windows::core::Result<Self> {
        // SAFETY: this is the first WinRT apartment initialization on the main
        // thread and Drop balances every successful call, including S_FALSE.
        // Winit enables OLE drag/drop on this thread, which requires the STA
        // apartment. WUC objects used here are agile; initializing MTA first
        // makes winit's later OleInitialize fail with RPC_E_CHANGED_MODE.
        unsafe { RoInitialize(RO_INIT_SINGLETHREADED)? };
        let apartment = RoApartment;
        let options = DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        };
        // SAFETY: creates a queue for this already initialized STA thread. The
        // retained controller keeps it alive through winit's entire event loop.
        let dispatcher = unsafe { CreateDispatcherQueueController(options) }?;
        Ok(Self {
            _dispatcher: dispatcher,
            _apartment: apartment,
        })
    }
}

impl Drop for RoApartment {
    fn drop(&mut self) {
        // SAFETY: balances the successful RoInitialize owned by this value.
        unsafe { RoUninitialize() };
    }
}

/// Holds the process timer resolution at 1ms for the lifetime of the event
/// loop so frame-pacing deadlines are not rounded to the legacy 15.6ms tick.
pub(crate) struct TimerResolution(bool);

impl TimerResolution {
    pub(crate) fn raise() -> Self {
        // SAFETY: a successful call is balanced in Drop.
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
use winit::platform::windows::WindowAttributesExtWindows;
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{DeviceEvent, DeviceId, ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow},
    keyboard::{Key, PhysicalKey},
    window::{Window, WindowId},
};
pub(crate) use winit::{
    event_loop::{EventLoop, EventLoopProxy},
    platform::pump_events::{EventLoopExtPumpEvents, PumpStatus},
};

#[derive(Debug)]
pub(crate) enum WindowCommand {
    WakeCapture,
    CompositorTick,
    ShellRasterReady,
    RedrawShell,
    SetCursor(Option<cursor_icon::CursorIcon>),
    SetTitle(String),
    SetImeEnabled(bool),
    SetImeCursorArea {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
}

pub(crate) enum LoopDirective {
    Exit,
    WaitUntil(Instant),
}

/// Raw winit integration. The coordinated application receives only neutral
/// shell/device events and an abstract loop directive.
pub(crate) struct EventLoopApplication {
    inner: crate::app::ShellApplication,
    creating_window: bool,
}

impl EventLoopApplication {
    pub fn new(inner: crate::app::ShellApplication) -> Self {
        Self {
            inner,
            creating_window: false,
        }
    }

    pub async fn run_async_tasks(&mut self) {
        self.inner.run_async_tasks().await;
    }

    pub async fn into_result(self) -> Result<(), Box<dyn Error>> {
        self.inner.into_result().await
    }
}

impl ApplicationHandler<crate::app::UserEvent> for EventLoopApplication {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Keep native-window creation in the resume callback. The coordinator
        // consumes it after this event-pump turn returns and starts the UI.
        if self.creating_window || !self.inner.wants_native_window() {
            return;
        }
        self.creating_window = true;
        match event_loop.create_window(
            Window::default_attributes()
                .with_title("Polymer")
                .with_inner_size(LogicalSize::new(1280.0, 720.0)),
        ) {
            Ok(window) => self.inner.accept_native_window(window),
            Err(error) => {
                self.inner.fail(error);
                event_loop.exit();
            }
        }
        self.creating_window = false;
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: crate::app::UserEvent) {
        if let crate::app::UserEvent::InspectorWindowRequested { address } = event {
            if self.inner.inspector_window_id().is_some() {
                return;
            }
            let owner = self
                .inner
                .window_manager_mut()
                .and_then(|window| window.native_handle().ok())
                .map(|hwnd| hwnd.0 as isize);
            let mut attributes = Window::default_attributes()
                .with_title("Polymer Inspector")
                .with_inner_size(LogicalSize::new(1280.0, 800.0))
                .with_visible(false);
            if let Some(owner) = owner {
                attributes = attributes.with_owner_window(owner);
            }
            match event_loop.create_window(attributes) {
                Ok(window) => self.inner.accept_inspector_window(window, address),
                Err(error) => {
                    tracing::error!(%error, "polymer: creating inspector window failed")
                }
            }
            return;
        }
        self.inner.handle_user_event(event);
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        self.inner
            .handle_device_event(translate_device_event(event));
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.inner.inspector_window_id() == Some(window_id) {
            self.inner.handle_inspector_window_event(&event);
            return;
        }
        let Some(manager) = self.inner.window_manager_mut() else {
            return;
        };
        if window_id != manager.id() {
            return;
        }
        let overlay_consumed = manager.handle_window_event(&event);
        let event = translate_window_event(event);
        if self.inner.handle_window_event(event, overlay_consumed) {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match self.inner.about_to_wait() {
            LoopDirective::Exit => event_loop.exit(),
            LoopDirective::WaitUntil(deadline) => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ShellWindowEvent {
    CloseRequested,
    DroppedFile(PathBuf),
    FileHover(bool),
    Moved,
    Resized {
        width: u32,
        height: u32,
    },
    ScaleFactorChanged,
    FocusLost,
    Keyboard {
        physical_key: Option<String>,
        logical_key: String,
        pressed: bool,
        repeat: bool,
    },
    Modifiers {
        alt: bool,
        shift: bool,
        control: bool,
    },
    CursorMoved {
        x: f64,
        y: f64,
    },
    CursorLeft,
    MouseInput {
        button: Option<u16>,
        pressed: bool,
    },
    MouseWheel {
        horizontal: i32,
        vertical: i32,
    },
    Ime(ShellImeEvent),
    RedrawRequested,
    Other,
}

#[derive(Clone, Debug)]
pub(crate) enum ShellImeEvent {
    Enabled,
    Preedit(String, Option<(usize, usize)>),
    Commit(String),
    Disabled,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ShellDeviceEvent {
    MouseMotion { delta_x: f64, delta_y: f64 },
    Other,
}

pub(crate) fn translate_device_event(event: DeviceEvent) -> ShellDeviceEvent {
    match event {
        DeviceEvent::MouseMotion { delta } => ShellDeviceEvent::MouseMotion {
            delta_x: delta.0,
            delta_y: delta.1,
        },
        _ => ShellDeviceEvent::Other,
    }
}

pub(crate) fn translate_window_event(event: WindowEvent) -> ShellWindowEvent {
    match event {
        WindowEvent::CloseRequested | WindowEvent::Destroyed => ShellWindowEvent::CloseRequested,
        WindowEvent::DroppedFile(path) => ShellWindowEvent::DroppedFile(path),
        WindowEvent::HoveredFile(_) => ShellWindowEvent::FileHover(true),
        WindowEvent::HoveredFileCancelled => ShellWindowEvent::FileHover(false),
        WindowEvent::Moved(_) => ShellWindowEvent::Moved,
        WindowEvent::Resized(size) => ShellWindowEvent::Resized {
            width: size.width,
            height: size.height,
        },
        WindowEvent::ScaleFactorChanged { .. } => ShellWindowEvent::ScaleFactorChanged,
        WindowEvent::Focused(false) => ShellWindowEvent::FocusLost,
        WindowEvent::Focused(true) => ShellWindowEvent::Other,
        WindowEvent::KeyboardInput { event, .. } => {
            let physical_key = match event.physical_key {
                PhysicalKey::Code(code) => Some(format!("{code:?}")),
                PhysicalKey::Unidentified(_) => None,
            };
            let logical_key = match event.logical_key {
                Key::Named(key) => format!("{key:?}"),
                Key::Character(key) => key.to_string(),
                Key::Dead(_) => "Dead".into(),
                Key::Unidentified(_) => "Unidentified".into(),
            };
            ShellWindowEvent::Keyboard {
                physical_key,
                logical_key,
                pressed: event.state == ElementState::Pressed,
                repeat: event.repeat,
            }
        }
        WindowEvent::ModifiersChanged(modifiers) => ShellWindowEvent::Modifiers {
            alt: modifiers.state().alt_key(),
            shift: modifiers.state().shift_key(),
            control: modifiers.state().control_key(),
        },
        WindowEvent::CursorMoved { position, .. } => ShellWindowEvent::CursorMoved {
            x: position.x,
            y: position.y,
        },
        WindowEvent::CursorLeft { .. } => ShellWindowEvent::CursorLeft,
        WindowEvent::MouseInput { state, button, .. } => ShellWindowEvent::MouseInput {
            button: neutral_mouse_button(button),
            pressed: state == ElementState::Pressed,
        },
        WindowEvent::MouseWheel { delta, .. } => {
            let (horizontal, vertical) = match delta {
                MouseScrollDelta::LineDelta(horizontal, vertical) => {
                    ((horizontal * 120.0) as i32, (vertical * 120.0) as i32)
                }
                MouseScrollDelta::PixelDelta(delta) => (delta.x as i32, delta.y as i32),
            };
            ShellWindowEvent::MouseWheel {
                horizontal,
                vertical,
            }
        }
        WindowEvent::Ime(event) => ShellWindowEvent::Ime(match event {
            Ime::Enabled => ShellImeEvent::Enabled,
            Ime::Preedit(value, range) => ShellImeEvent::Preedit(value, range),
            Ime::Commit(value) => ShellImeEvent::Commit(value),
            Ime::Disabled => ShellImeEvent::Disabled,
        }),
        WindowEvent::RedrawRequested => ShellWindowEvent::RedrawRequested,
        _ => ShellWindowEvent::Other,
    }
}

fn neutral_mouse_button(button: MouseButton) -> Option<u16> {
    match button {
        MouseButton::Left => Some(1),
        MouseButton::Right => Some(2),
        MouseButton::Middle => Some(3),
        MouseButton::Back => Some(4),
        MouseButton::Forward => Some(5),
        MouseButton::Other(button) => Some(button.saturating_add(4)),
    }
}

/// Wakes the event loop once per DirectComposition frame while captured
/// surfaces are visible.
pub(crate) struct CompositorClock {
    worker: Option<std::thread::JoinHandle<()>>,
    stop_event: HANDLE,
    active: Arc<AtomicBool>,
    healthy: Arc<AtomicBool>,
    tick_pending: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
}

impl CompositorClock {
    pub(crate) fn spawn(proxy: EventLoopProxy<crate::app::UserEvent>) -> std::io::Result<Self> {
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
            .name("polymer-compositor-clock".to_owned())
            .spawn(move || {
                let thread_stop_event = HANDLE(thread_stop_event as *mut _);
                loop {
                    while !thread_active.load(Ordering::Acquire) {
                        if thread_stopping.load(Ordering::Acquire) {
                            return;
                        }
                        std::thread::park();
                    }
                    if thread_stopping.load(Ordering::Acquire) {
                        return;
                    }
                    let status = unsafe {
                        DCompositionWaitForCompositorClock(Some(&[thread_stop_event]), INFINITE)
                    };
                    if status == WAIT_FAILED.0 {
                        thread_healthy.store(false, Ordering::Release);
                        return;
                    }
                    if thread_stopping.load(Ordering::Acquire) {
                        return;
                    }
                    if status != WAIT_OBJECT_0.0 + 1 {
                        continue;
                    }
                    if !thread_tick_pending.swap(true, Ordering::AcqRel)
                        && proxy
                            .send_event(crate::app::UserEvent::Window(
                                WindowCommand::CompositorTick,
                            ))
                            .is_err()
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

    pub(crate) fn set_active(&self, active: bool) {
        if self.active.swap(active, Ordering::AcqRel) != active
            && active
            && let Some(worker) = &self.worker
        {
            worker.thread().unpark();
        }
    }

    pub(crate) fn is_pacing(&self) -> bool {
        self.healthy.load(Ordering::Acquire) && self.active.load(Ordering::Acquire)
    }

    pub(crate) fn clear_tick(&self) {
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

const EGUI_STATISTICS_REFRESH: Duration = Duration::from_millis(100);
const EGUI_INTERACTION_GRACE: Duration = Duration::from_millis(250);
const PENDING_PRESENT_RETRY: Duration = Duration::from_millis(1);

/// Window-owned egui composition overlay and diagnostics renderer.
pub(crate) struct EguiOverlay {
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
    interactive_until: Option<Instant>,
}

impl EguiOverlay {
    pub(crate) fn new(window: &Window, recorder: RecorderHandle) -> Result<Self, Box<dyn Error>> {
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

    pub(crate) fn content(&self) -> Result<windows::core::IUnknown, windows::core::Error> {
        self.compositor.composition_content()
    }

    pub(crate) fn set_backend(&mut self, backend: String) {
        self.diagnostics.backend = backend;
    }

    pub(crate) fn set_input_diagnostics(&mut self, diagnostics: InputDiagnostics) {
        self.diagnostics.input = diagnostics;
    }

    pub(crate) fn add_surface(&mut self, connection: u64, surface: u64, target: u32) {
        self.frames
            .insert(connection, FrameMetrics::new(connection, surface, target));
    }

    pub(crate) fn observe_frame_at(
        &mut self,
        connection: u64,
        frame: polymer_protocol::FrameReady,
        observed_qpc: Option<u64>,
    ) {
        let latency_ticks = observed_qpc
            .or_else(|| performance_counter().ok())
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

    pub(crate) fn record_present(&mut self, carried_new_frame: bool) {
        self.presentation.record_present(carried_new_frame);
    }

    pub(crate) fn record_clock(&mut self) {
        self.presentation.record_clock();
    }

    pub(crate) fn set_note(&mut self, note: &str) {
        let notes = &mut self.diagnostics.notes;
        if note.is_empty() {
            notes.clear();
        } else if notes.first().is_none_or(|first| first != note) {
            notes.clear();
            notes.push(note.to_owned());
        }
    }

    pub(crate) fn remove_target(&mut self, connection: u64) {
        self.frames.remove(&connection);
        self.statistics.remove_connection(connection);
        self.diagnostics.statistics = self.statistics.snapshot().clone();
    }

    pub(crate) fn resize(&mut self, window: &Window) -> Result<(), Box<dyn Error>> {
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let render_window = SurfaceWindow::new(window, size.width, size.height);
        Renderer::recreate_swapchain(&mut self.compositor, &render_window)?;
        Ok(())
    }

    pub(crate) fn on_window_event(
        &mut self,
        window: &Window,
        event: &WindowEvent,
    ) -> egui_winit::EventResponse {
        self.state.on_window_event(window, event)
    }

    pub(crate) fn note_interaction(&mut self, now: Instant) {
        self.interactive_until = Some(now + EGUI_INTERACTION_GRACE);
        self.next_repaint = self.next_repaint.min(now);
    }

    fn interactive(&self, now: Instant) -> bool {
        self.interactive_until.is_some_and(|until| now < until)
    }

    pub(crate) fn draw(&mut self, window: &Window) -> Result<bool, Box<dyn Error>> {
        let now = Instant::now();
        let interactive = self.interactive(now);
        if !interactive && !self.repaint_due(now) {
            return Ok(false);
        }
        self.next_repaint = now + EGUI_STATISTICS_REFRESH;
        if !self.compositor.try_begin_frame() {
            if interactive {
                self.next_repaint = now + PENDING_PRESENT_RETRY;
            }
            return Ok(false);
        }
        self.diagnostics.statistics = self.statistics.sample().clone();
        let input = self.state.take_egui_input(window);
        let diagnostics = &self.diagnostics;
        let mut output = self
            .context
            .run(input, |context| draw_debug_overlay(context, diagnostics));
        self.state
            .handle_platform_output(window, std::mem::take(&mut output.platform_output));
        let primitives = self
            .context
            .tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
        if interactive {
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
        if self.context.is_using_pointer() || self.context.wants_pointer_input() {
            self.note_interaction(now);
        }
        Ok(true)
    }

    pub(crate) fn repaint_due(&self, now: Instant) -> bool {
        self.next_repaint <= now
    }

    pub(crate) fn next_repaint(&self) -> Instant {
        self.next_repaint
    }

    pub(crate) fn request_immediate_repaint(&mut self) {
        self.next_repaint = Instant::now();
    }
}

/// Sole owner of the native shell window and all window-attached composition,
/// overlay, and pacing resources.
pub(crate) struct WindowManager {
    window: Window,
    composition: CompositionTree,
    overlay: EguiOverlay,
    clock: CompositorClock,
}

impl WindowManager {
    pub fn new(
        window: Window,
        recorder: RecorderHandle,
        proxy: EventLoopProxy<crate::app::UserEvent>,
    ) -> Result<Self, Box<dyn Error>> {
        let composition = CompositionTree::new(&window)?;
        let mut overlay = EguiOverlay::new(&window, recorder)?;
        composition.set_egui_overlay_content(&overlay.content()?)?;
        overlay.draw(&window)?;
        composition.commit()?;
        Ok(Self {
            window,
            composition,
            overlay,
            clock: CompositorClock::spawn(proxy)?,
        })
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub(crate) fn native_handle(&self) -> Result<HWND, Box<dyn Error>> {
        Ok(WindowTarget::new(&self.window)?.0)
    }

    pub fn size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.window.inner_size()
    }

    pub fn scale_factor(&self) -> f32 {
        self.window.scale_factor() as f32
    }

    pub fn surface(&self, width: u32, height: u32) -> SurfaceWindow<'_> {
        SurfaceWindow::new(&self.window, width, height)
    }

    pub fn compositor(&self) -> Compositor {
        self.composition.compositor.clone()
    }

    pub fn request_redraw(&self) {
        self.window.request_redraw();
    }

    pub fn set_cursor(&self, icon: Option<cursor_icon::CursorIcon>) {
        self.window.set_cursor(icon.unwrap_or_default());
        self.window.set_cursor_visible(icon.is_some());
    }

    pub fn set_title(&self, title: &str) {
        self.window.set_title(title);
    }

    pub fn set_ime_enabled(&self, enabled: bool) {
        self.window.set_ime_allowed(enabled);
    }

    pub fn set_ime_cursor_area(&self, x: f32, y: f32, width: f32, height: f32) {
        self.window.set_ime_cursor_area(
            winit::dpi::LogicalPosition::new(x, y),
            winit::dpi::LogicalSize::new(width, height),
        );
    }

    pub fn set_shell_root(&self, root: &ContainerVisual) -> windows::core::Result<()> {
        self.composition.set_shell_root(root)
    }

    pub fn commit(&self) -> windows::core::Result<()> {
        self.composition.commit()
    }

    pub fn focus_host(&self) {
        self.composition.focus_host();
    }

    pub fn handle_window_event(&mut self, event: &WindowEvent) -> bool {
        let response = self.overlay.on_window_event(&self.window, event);
        if response.repaint {
            self.window.request_redraw();
        }
        response.consumed
    }

    pub fn note_overlay_interaction(&mut self, now: Instant) {
        self.overlay.note_interaction(now);
    }

    pub fn resize_overlay(&mut self) -> Result<(), Box<dyn Error>> {
        self.overlay.resize(&self.window)
    }

    pub fn draw_overlay(&mut self) -> Result<bool, Box<dyn Error>> {
        self.overlay.draw(&self.window)
    }

    pub fn overlay_repaint_due(&self, now: Instant) -> bool {
        self.overlay.repaint_due(now)
    }

    pub fn next_overlay_repaint(&self) -> Instant {
        self.overlay.next_repaint()
    }

    pub fn request_overlay_repaint(&mut self) {
        self.overlay.request_immediate_repaint();
    }

    pub fn set_overlay_note(&mut self, note: &str) {
        self.overlay.set_note(note);
    }

    pub fn set_input_diagnostics(&mut self, diagnostics: InputDiagnostics) {
        self.overlay.set_input_diagnostics(diagnostics);
    }

    pub fn set_backend(&mut self, backend: String) {
        self.overlay.set_backend(backend);
    }

    pub fn add_surface(&mut self, connection: u64, surface: u64, target: u32) {
        self.overlay.add_surface(connection, surface, target);
    }

    pub fn observe_frame_at(
        &mut self,
        connection: u64,
        frame: polymer_protocol::FrameReady,
        observed_qpc: Option<u64>,
    ) {
        self.overlay
            .observe_frame_at(connection, frame, observed_qpc);
    }

    pub fn remove_target(&mut self, connection: u64) {
        self.overlay.remove_target(connection);
    }

    pub fn record_present(&mut self, carried_new_frame: bool) {
        self.overlay.record_present(carried_new_frame);
    }

    pub fn record_clock(&mut self) {
        self.overlay.record_clock();
    }

    pub fn set_clock_active(&self, active: bool) {
        self.clock.set_active(active);
    }

    pub fn is_pacing(&self) -> bool {
        self.clock.is_pacing()
    }

    pub fn clear_clock_tick(&self) {
        self.clock.clear_tick();
    }
}

struct CompositionTree {
    host: WindowTarget,
    controller: CompositorController,
    compositor: Compositor,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    overlay: SpriteVisual,
}

/// Opaque native host-window target. Native handle extraction and operations
/// stay in this platform adapter instead of leaking into DOM/input code.
struct WindowTarget(HWND);

impl WindowTarget {
    fn new(window: &Window) -> Result<Self, Box<dyn Error>> {
        match window.window_handle()?.as_raw() {
            RawWindowHandle::Win32(handle) => Ok(Self(HWND(handle.hwnd.get() as *mut c_void))),
            _ => Err("Polymer composition requires a Win32 window".into()),
        }
    }

    fn focus(&self) {
        // SAFETY: CompositionTree cannot outlive the winit window used to
        // create it, and the target stores only that live host handle.
        let _ = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.0)) };
    }
}

impl CompositionTree {
    fn new(window: &Window) -> Result<Self, Box<dyn Error>> {
        let host = WindowTarget::new(window)?;

        let controller = CompositorController::new()?;
        let compositor = controller.Compositor()?;
        let desktop: ICompositorDesktopInterop = compositor.cast()?;
        // SAFETY: the desktop target retains the live HWND for the lifetime of
        // this tree, and WindowTarget cannot outlive its winit Window.
        let target = unsafe { desktop.CreateDesktopWindowTarget(host.0, true)? };
        let root = compositor.CreateContainerVisual()?;
        root.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        target.SetRoot(&root)?;

        let overlay = compositor.CreateSpriteVisual()?;
        overlay.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        root.Children()?.InsertAtTop(&overlay)?;
        controller.Commit()?;

        Ok(Self {
            host,
            controller,
            compositor,
            _target: target,
            root,
            overlay,
        })
    }

    pub fn focus_host(&self) {
        self.host.focus();
    }

    pub fn set_egui_overlay_content(&self, content: &IUnknown) -> Result<(), windows::core::Error> {
        let brush = self.surface_brush(content)?;
        self.overlay.SetBrush(&brush)
    }

    pub fn set_shell_root(&self, shell: &ContainerVisual) -> windows::core::Result<()> {
        let children = self.root.Children()?;
        children.RemoveAll()?;
        children.InsertAtTop(shell)?;
        children.InsertAtTop(&self.overlay)
    }

    fn surface_brush(
        &self,
        swapchain: &IUnknown,
    ) -> windows::core::Result<windows::UI::Composition::CompositionSurfaceBrush> {
        let interop: ICompositorInterop = self.compositor.cast()?;
        // SAFETY: the supplied object is an IDXGISwapChain created for
        // composition and retained by its renderer for at least as long as the
        // resulting surface brush remains installed.
        let surface = unsafe { interop.CreateCompositionSurfaceForSwapChain(swapchain)? };
        let brush = self.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::Fill)?;
        Ok(brush)
    }

    pub fn commit(&self) -> windows::core::Result<()> {
        self.controller.Commit()
    }
}

pub struct SurfaceWindow<'a, W: ?Sized = Window> {
    window: &'a W,
    extent: Extent,
}

impl<'a, W: ?Sized> SurfaceWindow<'a, W> {
    pub const fn new(window: &'a W, width: u32, height: u32) -> Self {
        Self {
            window,
            extent: Extent::new(width, height),
        }
    }
}

impl<W: raw_window_handle::HasWindowHandle + ?Sized> raw_window_handle::HasWindowHandle
    for SurfaceWindow<'_, W>
{
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        self.window.window_handle()
    }
}

impl<W: raw_window_handle::HasDisplayHandle + ?Sized> raw_window_handle::HasDisplayHandle
    for SurfaceWindow<'_, W>
{
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        self.window.display_handle()
    }
}

impl<W: raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle + ?Sized>
    polymer_renderer::RenderWindow for SurfaceWindow<'_, W>
{
    fn render_extent(&self) -> Extent {
        self.extent
    }
}
