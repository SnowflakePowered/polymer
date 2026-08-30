use crate::{
    HookBackend,
    capture::{self, ConsumerPreference, FrameRouter},
    input::{InputTransport, ProtocolInputSender},
    ui::{UiRequestReceiver, UiTransport},
};
use crossfire::{MRx, MTx, mpmc, oneshot::TxOneshot};
use dashmap::{DashMap, DashSet};
use parking_lot::{Condvar, Mutex, RwLock};
use snowglass_input::{
    RawKeyInput,
    gamepad::{GamepadUpdate, GamepadUpdateSink},
    mouse::{PointerInput, PointerPhase, PointerSource},
};
use snowglass_ipc::{Message, StreamReady};
use snowglass_platform_windows::{
    NativeWindow, ProcessId, ProcessWaitHandle, TargetProcess, client_size, process_audio_state,
    process_tree_ids, set_process_audio,
};
use snowglass_protocol::{ExternalImageKind, FrameReady};
use std::{
    collections::HashSet,
    fmt,
    path::PathBuf,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use watchable::{TimeoutError, Watchable, Watcher};

type SurfaceEventSender = MTx<mpmc::List<SurfaceEvent>>;
type SurfaceEventChannel = MRx<mpmc::List<SurfaceEvent>>;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TrackedSurfaceId(pub u32);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackedSurfaceConfig {
    pub id: TrackedSurfaceId,
    pub render_target_index: u32,
    pub window_pattern: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EmulatorLaunchConfig {
    pub session: String,
    pub command: String,
    pub arguments: Vec<String>,
    pub artifact_root: PathBuf,
    pub hooks: Vec<HookBackend>,
    pub input_hooks: Vec<crate::InputHookBackend>,
    pub bluetooth_hid_pipes: Vec<crate::BluetoothHidPipeConfig>,
    pub consumer: ConsumerPreference,
    pub isolate: bool,
    pub tracked_surfaces: Vec<TrackedSurfaceConfig>,
    pub tracking_enabled: bool,
    pub skip_surfaces: u32,
    pub graph_tracking: bool,
    pub force_keyed_mutex: bool,
    pub gamepad_layouts: Vec<String>,
}

impl EmulatorLaunchConfig {
    fn validate(&self) -> Result<(), LaunchError> {
        if self.session.is_empty() || self.session.contains(['/', '\\']) {
            return Err(LaunchError::InvalidConfig(
                "session must be a non-empty socket name".to_owned(),
            ));
        }
        if self.command.trim().is_empty() {
            return Err(LaunchError::InvalidConfig(
                "target command must not be empty".to_owned(),
            ));
        }
        if self.hooks.is_empty() {
            return Err(LaunchError::InvalidConfig(
                "at least one graphics hook is required".to_owned(),
            ));
        }
        if self.bluetooth_hid_pipes.iter().any(|pipe| pipe.port >= 4)
            || self
                .bluetooth_hid_pipes
                .iter()
                .map(|pipe| pipe.port)
                .collect::<HashSet<_>>()
                .len()
                != self.bluetooth_hid_pipes.len()
        {
            return Err(LaunchError::InvalidConfig(
                "Bluetooth HID pipes require unique ports 0..=3".to_owned(),
            ));
        }
        if !self.bluetooth_hid_pipes.is_empty()
            && !self
                .input_hooks
                .contains(&crate::InputHookBackend::BluetoothApiWin32)
        {
            return Err(LaunchError::InvalidConfig(
                "Bluetooth HID pipes require their input hook".to_owned(),
            ));
        }
        let mut ids = HashSet::new();
        if self
            .tracked_surfaces
            .iter()
            .any(|surface| !ids.insert(surface.id))
        {
            return Err(LaunchError::InvalidConfig(
                "tracked surface ids must be unique".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum LaunchError {
    InvalidConfig(String),
    Io(std::io::Error),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => formatter.write_str(message),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LaunchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidConfig(_) => None,
            Self::Io(error) => Some(error),
        }
    }
}

impl From<std::io::Error> for LaunchError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstanceExit {
    Natural,
    Terminated,
    Failed(Arc<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstanceState {
    Starting,
    Running,
    Closing,
    Exited(InstanceExit),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NormalizedVolume(f32);

impl NormalizedVolume {
    pub fn new(value: f32) -> Option<Self> {
        value.is_finite().then_some(Self(value.clamp(0.0, 1.0)))
    }

    pub const fn get(self) -> f32 {
        self.0
    }
}

impl Default for NormalizedVolume {
    fn default() -> Self {
        Self(1.0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioState {
    pub available: bool,
    pub volume: NormalizedVolume,
    pub muted: bool,
    pub active_sessions: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResizeDimensions {
    pub width: u32,
    pub height: u32,
}

impl ResizeDimensions {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PresentedFrameToken {
    generation: u64,
    frame_number: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct SurfaceFrameUpdate {
    pub generation: u64,
    pub frame: FrameReady,
}

impl SurfaceFrameUpdate {
    pub const fn presentation_token(self) -> PresentedFrameToken {
        PresentedFrameToken {
            generation: self.generation,
            frame_number: self.frame.frame_number,
        }
    }
}

pub struct SurfaceFrameReceiver {
    generation: u64,
    receiver: Watcher<Option<FrameReady>>,
}

impl SurfaceFrameReceiver {
    pub fn latest(&self) -> Option<SurfaceFrameUpdate> {
        (*self.receiver.peek()).map(|frame| SurfaceFrameUpdate {
            generation: self.generation,
            frame,
        })
    }

    pub async fn next_frame(&mut self) -> Result<SurfaceFrameUpdate, SurfaceError> {
        self.receiver
            .next_value_async()
            .await
            .map_err(|_| SurfaceError::Unavailable)?
            .map(|frame| SurfaceFrameUpdate {
                generation: self.generation,
                frame,
            })
            .ok_or(SurfaceError::Unavailable)
    }

    pub fn try_next_frame(&mut self) -> Result<Option<SurfaceFrameUpdate>, SurfaceError> {
        self.next_frame_timeout(Duration::ZERO)
    }

    /// Waits for the newest frame for at most `timeout`.
    ///
    /// Native consumers use this from a dedicated presentation thread so a
    /// busy UI event loop cannot hold the producer's finite export slots.
    pub fn next_frame_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<SurfaceFrameUpdate>, SurfaceError> {
        match self.receiver.watch_timeout(timeout) {
            Ok(()) => Ok(self.receiver.get().map(|frame| SurfaceFrameUpdate {
                generation: self.generation,
                frame,
            })),
            Err(TimeoutError::Timeout) => Ok(None),
            Err(TimeoutError::Disconnected) => Err(SurfaceError::Unavailable),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SurfacePointerUpdate {
    pub source: PointerSource,
    pub phase: PointerPhase,
    pub pointer_id: u32,
    pub x: i32,
    pub y: i32,
    pub coordinate_width: u32,
    pub coordinate_height: u32,
    pub button: u32,
    pub buttons: u32,
    pub wheel_horizontal: i32,
    pub wheel_vertical: i32,
    pub timestamp: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HotkeyKey {
    pub virtual_key: u32,
    pub scan_code: u16,
    pub extended: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HotkeyChord {
    pub modifiers: Vec<HotkeyKey>,
    pub key: HotkeyKey,
}

#[derive(Clone, Debug)]
struct SurfaceSnapshot {
    generation: u64,
    protocol_surface_id: u64,
    target_window: i64,
    backend: HookBackend,
    transport_dimensions: ResizeDimensions,
    active: bool,
}

struct SurfaceShared {
    snapshot: RwLock<SurfaceSnapshot>,
    input: InputTransport,
    frames: RwLock<Watchable<Option<FrameReady>>>,
    sequence: AtomicU32,
    started: Instant,
}

#[derive(Clone)]
pub struct TrackedSurfaceHandle {
    id: TrackedSurfaceId,
    shared: Arc<SurfaceShared>,
}

impl TrackedSurfaceHandle {
    pub const fn id(&self) -> TrackedSurfaceId {
        self.id
    }

    pub fn backend(&self) -> HookBackend {
        self.snapshot().backend
    }

    pub fn target_window(&self) -> Option<NativeWindow> {
        let window = self.snapshot().target_window;
        (window != 0).then_some(NativeWindow(window as isize))
    }

    pub fn transport_dimensions(&self) -> ResizeDimensions {
        self.snapshot().transport_dimensions
    }

    pub fn window_dimensions(&self) -> Result<ResizeDimensions, SurfaceError> {
        let window = self.target_window().ok_or(SurfaceError::Unavailable)?;
        let (width, height) = client_size(window).ok_or(SurfaceError::Unavailable)?;
        Ok(ResizeDimensions::new(width, height))
    }

    pub fn subscribe_frames(&self) -> SurfaceFrameReceiver {
        let snapshot = self.snapshot();
        let receiver = self.shared.frames.read().watch();
        receiver.mark_read();
        SurfaceFrameReceiver {
            generation: snapshot.generation,
            receiver,
        }
    }

    pub fn send_pointer_update(
        &self,
        frame: PresentedFrameToken,
        update: SurfacePointerUpdate,
    ) -> Result<(), SurfaceError> {
        let snapshot = self.snapshot();
        self.validate_active_generation(&snapshot, frame.generation)?;
        self.shared
            .input
            .send(Message::PointerInput(PointerInput {
                surface_id: snapshot.protocol_surface_id,
                target_window: snapshot.target_window,
                frame_number: frame.frame_number,
                source: update.source,
                phase: update.phase,
                pointer_id: update.pointer_id,
                x: update.x,
                y: update.y,
                coordinate_width: update.coordinate_width,
                coordinate_height: update.coordinate_height,
                button: update.button,
                buttons: update.buttons,
                wheel_horizontal: update.wheel_horizontal,
                wheel_vertical: update.wheel_vertical,
                timestamp: update.timestamp,
                sequence: self.next_sequence(),
            }))
            .then_some(())
            .ok_or(SurfaceError::Unavailable)
    }

    pub fn send_hotkey(&self, chord: &HotkeyChord) -> Result<(), SurfaceError> {
        let snapshot = self.snapshot();
        self.validate_active_generation(&snapshot, snapshot.generation)?;
        for key in &chord.modifiers {
            self.send_raw_key(&snapshot, *key, true)?;
        }
        let result = self
            .send_raw_key(&snapshot, chord.key, true)
            .and_then(|()| self.send_raw_key(&snapshot, chord.key, false));
        for key in chord.modifiers.iter().rev() {
            let _ = self.send_raw_key(&snapshot, *key, false);
        }
        result
    }

    pub fn resize_window(&self, dimensions: ResizeDimensions) -> Result<(), SurfaceError> {
        if dimensions.width == 0 || dimensions.height == 0 {
            return Err(SurfaceError::InvalidDimensions);
        }
        let snapshot = self.snapshot();
        self.validate_active_generation(&snapshot, snapshot.generation)?;
        self.shared
            .input
            .send(Message::ResizeWindow(snowglass_protocol::ResizeWindow {
                target_window: snapshot.target_window,
                width: dimensions.width,
                height: dimensions.height,
            }))
            .then_some(())
            .ok_or(SurfaceError::Unavailable)
    }

    fn send_raw_key(
        &self,
        snapshot: &SurfaceSnapshot,
        key: HotkeyKey,
        pressed: bool,
    ) -> Result<(), SurfaceError> {
        self.shared
            .input
            .send(Message::RawKeyInput(RawKeyInput {
                target_window: snapshot.target_window,
                virtual_key: key.virtual_key,
                scan_code: key.scan_code,
                extended: key.extended,
                pressed,
                repeat: false,
                timestamp: self.shared.started.elapsed().as_millis() as u32,
                sequence: self.next_sequence(),
            }))
            .then_some(())
            .ok_or(SurfaceError::Unavailable)
    }

    fn snapshot(&self) -> SurfaceSnapshot {
        self.shared.snapshot.read().clone()
    }

    fn validate_active_generation(
        &self,
        snapshot: &SurfaceSnapshot,
        generation: u64,
    ) -> Result<(), SurfaceError> {
        if !snapshot.active || snapshot.target_window == 0 {
            return Err(SurfaceError::Unavailable);
        }
        if snapshot.generation != generation {
            return Err(SurfaceError::StaleGeneration);
        }
        Ok(())
    }

    fn next_sequence(&self) -> u32 {
        self.shared.sequence.fetch_add(1, Ordering::AcqRel) + 1
    }
}

#[derive(Debug)]
pub enum SurfaceError {
    Unavailable,
    StaleGeneration,
    InvalidDimensions,
    Io(std::io::Error),
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("tracked surface is unavailable"),
            Self::StaleGeneration => formatter.write_str("tracked surface generation is stale"),
            Self::InvalidDimensions => formatter.write_str("window dimensions must be non-zero"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SurfaceError {}

pub enum SurfaceEvent {
    Offered(SurfaceOffer),
    Removed {
        id: TrackedSurfaceId,
        connection: u64,
    },
}

#[derive(Clone)]
pub struct SurfaceEventReceiver(SurfaceEventChannel);

impl SurfaceEventReceiver {
    pub fn try_recv(&self) -> Result<SurfaceEvent, crossfire::TryRecvError> {
        self.0.try_recv()
    }

    pub fn recv(&self) -> Result<SurfaceEvent, crossfire::RecvError> {
        self.0.recv()
    }
}

pub struct SurfaceOffer {
    offer: u64,
    connection: u64,
    stream: StreamReady,
    surface: TrackedSurfaceHandle,
    replacing: Option<u64>,
    manager: Weak<SurfaceManager>,
    resolved: bool,
}

impl SurfaceOffer {
    pub const fn id(&self) -> TrackedSurfaceId {
        self.surface.id()
    }

    pub fn backend(&self) -> HookBackend {
        stream_backend(self.stream.image_kind)
    }

    pub const fn connection_id(&self) -> u64 {
        self.connection
    }

    pub const fn replaced_connection_id(&self) -> Option<u64> {
        self.replacing
    }

    pub const fn stream(&self) -> &StreamReady {
        &self.stream
    }

    pub fn surface(&self) -> &TrackedSurfaceHandle {
        &self.surface
    }

    pub fn accept(mut self) -> Result<TrackedSurfaceHandle, SurfaceError> {
        let Some(manager) = self.manager.upgrade() else {
            return Err(SurfaceError::Unavailable);
        };
        manager.resolve_offer(self.offer, true)?;
        self.resolved = true;
        Ok(self.surface.clone())
    }

    pub fn reject(mut self) {
        if let Some(manager) = self.manager.upgrade() {
            let _ = manager.resolve_offer(self.offer, false);
        }
        self.resolved = true;
    }
}

impl Drop for SurfaceOffer {
    fn drop(&mut self) {
        if !self.resolved
            && let Some(manager) = self.manager.upgrade()
        {
            let _ = manager.resolve_offer(self.offer, false);
        }
    }
}

pub struct EmulatorInstance {
    process_id: u32,
    lifecycle: Arc<InstanceLifecycle>,
    events: SurfaceEventReceiver,
    ui_requests: UiRequestReceiver,
}

#[derive(Clone)]
pub struct VirtualGamepadConnectionSender(InputTransport);

impl VirtualGamepadConnectionSender {
    pub fn set_connected(
        &self,
        connection: snowglass_input::gamepad::VirtualGamepadConnection,
    ) -> Result<(), String> {
        self.0
            .set_gamepad_connected(connection)
            .map_err(|error| error.to_string())
    }
}

impl EmulatorInstance {
    pub fn launch(config: EmulatorLaunchConfig) -> Result<Self, LaunchError> {
        Self::launch_with_waker(config, || {})
    }

    pub fn launch_with_waker(
        config: EmulatorLaunchConfig,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, LaunchError> {
        Self::launch_with_input(config, wake, |_| Ok(()))
    }

    pub fn launch_with_input(
        config: EmulatorLaunchConfig,
        wake: impl Fn() + Send + Sync + 'static,
        configure_input: impl FnOnce(VirtualGamepadConnectionSender) -> Result<(), String>,
    ) -> Result<Self, LaunchError> {
        config.validate()?;
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let input = InputTransport::bind(&config.session)?;
        if let Err(error) = configure_input(VirtualGamepadConnectionSender(input.clone())) {
            input.shutdown();
            return Err(LaunchError::InvalidConfig(error));
        }
        let (ui, ui_requests) = match UiTransport::bind(&config.session, wake.clone()) {
            Ok(ui) => ui,
            Err(error) => {
                input.shutdown();
                return Err(error.into());
            }
        };
        let (event_sender, event_receiver) = mpmc::unbounded_blocking();
        let surfaces = Arc::new(SurfaceManager::new(&config, input.clone(), event_sender));
        let offer_manager = Arc::downgrade(&surfaces);
        let disconnect_manager = Arc::downgrade(&surfaces);
        let callbacks = capture::CaptureCallbacks {
            offer: Arc::new(move |connection, stream, accepted| {
                if let Some(manager) = offer_manager.upgrade() {
                    manager.offer(connection, stream, accepted);
                } else {
                    accepted.send(false);
                }
            }),
            disconnected: Arc::new(move |connection| {
                if let Some(manager) = disconnect_manager.upgrade() {
                    manager.disconnected(connection);
                }
            }),
        };
        let router = match capture::bind(&config.session, config.consumer, callbacks, Some(wake)) {
            Ok(router) => router,
            Err(error) => {
                input.shutdown();
                ui.shutdown();
                return Err(error.into());
            }
        };
        surfaces.install_router(router.clone());
        let process = match crate::launch::launch(&config) {
            Ok(process) => process,
            Err(error) => {
                surfaces.shutdown();
                input.shutdown();
                ui.shutdown();
                return Err(error);
            }
        };
        let process_id = process.id();
        let wait_handle = match process.wait_handle() {
            Ok(wait_handle) => wait_handle,
            Err(error) => {
                surfaces.shutdown();
                input.shutdown();
                ui.shutdown();
                let _ = process.terminate_tree_and_wait();
                return Err(error.into());
            }
        };
        let audio = match AudioWorker::spawn(ProcessId(process_id)) {
            Ok(audio) => audio,
            Err(error) => {
                surfaces.shutdown();
                input.shutdown();
                ui.shutdown();
                let _ = process.terminate_tree_and_wait();
                return Err(error.into());
            }
        };
        let state = Watchable::new(InstanceState::Starting);
        let lifecycle = Arc::new(InstanceLifecycle {
            process: Mutex::new(Some(process)),
            input: input.clone(),
            ui,
            state: state.clone(),
            surfaces: surfaces.clone(),
            audio,
            finishing: AtomicBool::new(false),
            monitor: Mutex::new(None),
        });
        state.replace(InstanceState::Running);
        if let Err(error) = lifecycle.start_monitor(wait_handle) {
            lifecycle.finish(InstanceExit::Terminated);
            return Err(error.into());
        }
        Ok(Self {
            process_id,
            lifecycle,
            events: SurfaceEventReceiver(event_receiver),
            ui_requests,
        })
    }

    pub const fn process_id(&self) -> u32 {
        self.process_id
    }

    pub fn state(&self) -> InstanceState {
        self.lifecycle.state.get()
    }

    pub fn virtual_gamepad_connections(&self) -> VirtualGamepadConnectionSender {
        VirtualGamepadConnectionSender(self.lifecycle.input.clone())
    }

    pub fn tracked_surface(&self, id: TrackedSurfaceId) -> Option<TrackedSurfaceHandle> {
        self.lifecycle
            .surfaces
            .active
            .get(&id)
            .map(|surface| surface.value().handle.clone())
    }

    pub fn tracked_surfaces(&self) -> Vec<TrackedSurfaceHandle> {
        let mut surfaces = self
            .lifecycle
            .surfaces
            .active
            .iter()
            .map(|surface| surface.value().handle.clone())
            .collect::<Vec<_>>();
        surfaces.sort_by_key(TrackedSurfaceHandle::id);
        surfaces
    }

    pub fn subscribe_surfaces(&self) -> SurfaceEventReceiver {
        self.events.clone()
    }

    pub fn subscribe_ui_requests(&self) -> UiRequestReceiver {
        self.ui_requests.clone()
    }

    pub fn send_gamepad_update(&self, update: GamepadUpdate) -> Result<(), SurfaceError> {
        self.lifecycle
            .input
            .publish_gamepad(update)
            .map_err(SurfaceError::Io)
    }

    pub fn gamepad_input(&self) -> Arc<dyn GamepadUpdateSink> {
        Arc::new(self.lifecycle.input.clone())
    }

    pub fn subscribe_gamepad_feedback(
        &self,
    ) -> std::sync::mpsc::Receiver<snowglass_input::gamepad::GamepadFeedback> {
        self.lifecycle.input.subscribe_feedback()
    }

    /// Compatibility bridge for input encoders that already produce the
    /// existing Snowglass protocol messages. The instance still owns the IPC
    /// connection and writer lifecycle.
    #[doc(hidden)]
    pub fn protocol_input_sender(&self) -> ProtocolInputSender {
        self.lifecycle.input.sender()
    }

    pub fn audio_state(&self) -> AudioState {
        self.lifecycle.audio.state()
    }

    pub fn set_volume(&self, volume: NormalizedVolume) {
        self.lifecycle.audio.set_volume(volume);
    }

    pub fn set_muted(&self, muted: bool) {
        self.lifecycle.audio.set_muted(muted);
    }

    pub fn close(&mut self) -> InstanceExit {
        self.lifecycle.finish(InstanceExit::Terminated);
        let exit = self.lifecycle.wait_blocking();
        self.lifecycle.join_monitor();
        exit
    }

    pub async fn wait(&mut self) -> InstanceExit {
        let states = self.lifecycle.state.watch();
        states.mark_read();
        loop {
            if let Some(exit) = terminal_exit(&self.lifecycle.state) {
                self.lifecycle.join_monitor();
                return exit;
            }
            if states.next_value_async().await.is_err() {
                let exit = terminal_exit(&self.lifecycle.state).unwrap_or(InstanceExit::Natural);
                self.lifecycle.join_monitor();
                return exit;
            }
        }
    }
}

impl Drop for EmulatorInstance {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn terminal_exit(state: &Watchable<InstanceState>) -> Option<InstanceExit> {
    match state.get() {
        InstanceState::Exited(exit) => Some(exit.clone()),
        _ => None,
    }
}

struct PendingOffer {
    connection: u64,
    surface_id: TrackedSurfaceId,
    stream: StreamReady,
    accepted: TxOneshot<bool>,
    handle: TrackedSurfaceHandle,
    snapshot: SurfaceSnapshot,
    frames: Watchable<Option<FrameReady>>,
    replacing: Option<u64>,
}

struct ActiveSurface {
    connection: u64,
    protocol_surface_id: u64,
    handle: TrackedSurfaceHandle,
}

pub(crate) struct SurfaceManager {
    hooks: Arc<[HookBackend]>,
    tracked_surfaces: Arc<[TrackedSurfaceConfig]>,
    input: InputTransport,
    router: RwLock<Option<FrameRouter>>,
    events: SurfaceEventSender,
    active: DashMap<TrackedSurfaceId, ActiveSurface>,
    pending: DashMap<u64, PendingOffer>,
    topology: Mutex<()>,
    next_offer: AtomicU64,
    remaining_skips: AtomicU32,
    skipped_surfaces: DashSet<(u64, HookBackend)>,
    stopping: AtomicBool,
}

impl SurfaceManager {
    fn new(
        config: &EmulatorLaunchConfig,
        input: InputTransport,
        events: SurfaceEventSender,
    ) -> Self {
        Self {
            hooks: Arc::from(config.hooks.clone()),
            tracked_surfaces: Arc::from(config.tracked_surfaces.clone()),
            input,
            router: RwLock::new(None),
            events,
            active: DashMap::new(),
            pending: DashMap::new(),
            topology: Mutex::new(()),
            next_offer: AtomicU64::new(1),
            remaining_skips: AtomicU32::new(config.skip_surfaces),
            skipped_surfaces: DashSet::new(),
            stopping: AtomicBool::new(false),
        }
    }

    fn install_router(&self, router: FrameRouter) {
        *self.router.write() = Some(router);
    }

    fn router(&self) -> Option<FrameRouter> {
        self.router.read().clone()
    }

    pub(crate) fn offer(
        self: &Arc<Self>,
        connection: u64,
        stream: StreamReady,
        accepted: TxOneshot<bool>,
    ) {
        let _topology = self.topology.lock();
        if self.stopping.load(Ordering::Acquire) {
            accepted.send(false);
            return;
        }
        let Some(router) = self.router() else {
            accepted.send(false);
            return;
        };
        let backend = stream_backend(stream.image_kind);
        let Some(incoming_rank) = self.hooks.iter().position(|hook| *hook == backend) else {
            accepted.send(false);
            return;
        };
        let skip_identity = (stream.surface_id.0, backend);
        let remaining_skips = self.remaining_skips.load(Ordering::Acquire);
        if self.skipped_surfaces.contains(&skip_identity) || remaining_skips != 0 {
            if !self.skipped_surfaces.contains(&skip_identity) {
                self.remaining_skips
                    .store(remaining_skips - 1, Ordering::Release);
                if stream.surface_id.0 != 0 {
                    self.skipped_surfaces.insert(skip_identity);
                }
            }
            accepted.send(false);
            return;
        }
        let existing = self
            .active
            .iter()
            .find(|active| {
                stream.surface_id.0 != 0
                    && active.value().protocol_surface_id == stream.surface_id.0
            })
            .map(|active| {
                (
                    *active.key(),
                    active.value().connection,
                    active.value().handle.backend(),
                )
            });
        let pending = self
            .pending
            .iter()
            .find(|pending| {
                stream.surface_id.0 != 0
                    && pending.value().stream.surface_id.0 == stream.surface_id.0
            })
            .map(|pending| {
                (
                    *pending.key(),
                    pending.value().surface_id,
                    stream_backend(pending.value().stream.image_kind),
                )
            });
        let (surface_id, replacing) = if let Some((offer, id, old_backend)) = pending {
            let old_rank = self
                .hooks
                .iter()
                .position(|hook| *hook == old_backend)
                .unwrap_or(usize::MAX);
            if incoming_rank > old_rank {
                accepted.send(false);
                return;
            }
            self.reject_pending_locked(offer);
            let replacing = self.active.get(&id).map(|active| active.connection);
            (id, replacing)
        } else if let Some((id, old_connection, old_backend)) = existing {
            let old_rank = self
                .hooks
                .iter()
                .position(|hook| *hook == old_backend)
                .unwrap_or(usize::MAX);
            if incoming_rank > old_rank {
                accepted.send(false);
                return;
            }
            (id, Some(old_connection))
        } else {
            let Some(id) = self.unassigned_surface(&stream) else {
                accepted.send(false);
                return;
            };
            (id, None)
        };
        let generation = self
            .active
            .get(&surface_id)
            .map_or(1, |active| active.value().handle.snapshot().generation + 1);
        let frames = Watchable::new(None);
        let snapshot = SurfaceSnapshot {
            generation,
            protocol_surface_id: stream.surface_id.0,
            target_window: stream.source_window,
            backend,
            transport_dimensions: ResizeDimensions::new(stream.width, stream.height),
            active: false,
        };
        let handle = self
            .active
            .get(&surface_id)
            .map(|active| active.value().handle.clone())
            .unwrap_or_else(|| TrackedSurfaceHandle {
                id: surface_id,
                shared: Arc::new(SurfaceShared {
                    snapshot: RwLock::new(snapshot.clone()),
                    input: self.input.clone(),
                    frames: RwLock::new(frames.clone()),
                    sequence: AtomicU32::new(0),
                    started: Instant::now(),
                }),
            });
        router.route(connection, frames.clone());
        let offer_id = self.next_offer.fetch_add(1, Ordering::AcqRel);
        self.pending.insert(
            offer_id,
            PendingOffer {
                connection,
                surface_id,
                stream,
                accepted,
                handle: handle.clone(),
                snapshot,
                frames,
                replacing,
            },
        );
        let offer = SurfaceOffer {
            offer: offer_id,
            connection,
            stream,
            surface: handle.clone(),
            replacing,
            manager: Arc::downgrade(self),
            resolved: false,
        };
        if self.events.send(SurfaceEvent::Offered(offer)).is_err() {
            self.reject_pending_locked(offer_id);
        } else {
            router.wake();
        }
    }

    fn resolve_offer(&self, offer: u64, accept: bool) -> Result<(), SurfaceError> {
        let _topology = self.topology.lock();
        let Some((_, pending)) = self.pending.remove(&offer) else {
            return Err(SurfaceError::Unavailable);
        };
        let Some(router) = self.router() else {
            pending.frames.shutdown();
            pending.accepted.send(false);
            return Err(SurfaceError::Unavailable);
        };
        if !accept || self.stopping.load(Ordering::Acquire) {
            pending.frames.shutdown();
            router.revoke(pending.connection);
            pending.accepted.send(false);
            return if accept {
                Err(SurfaceError::Unavailable)
            } else {
                Ok(())
            };
        }
        if let Some(replaced) = pending.replacing {
            router.revoke(replaced);
        }
        let mut snapshot = pending.snapshot;
        snapshot.active = true;
        *pending.handle.shared.snapshot.write() = snapshot;
        if pending.replacing.is_some() {
            let previous_frames =
                std::mem::replace(&mut *pending.handle.shared.frames.write(), pending.frames);
            previous_frames.shutdown();
        }
        self.active.insert(
            pending.surface_id,
            ActiveSurface {
                connection: pending.connection,
                protocol_surface_id: pending.stream.surface_id.0,
                handle: pending.handle.clone(),
            },
        );
        pending.accepted.send(true);
        Ok(())
    }

    fn reject_pending_locked(&self, offer: u64) {
        let Some((_, pending)) = self.pending.remove(&offer) else {
            return;
        };
        pending.frames.shutdown();
        if let Some(router) = self.router() {
            router.revoke(pending.connection);
        }
        pending.accepted.send(false);
    }

    pub(crate) fn disconnected(&self, connection: u64) {
        let _topology = self.topology.lock();
        let removed = self
            .active
            .iter()
            .find_map(|active| (active.value().connection == connection).then_some(*active.key()));
        let Some(id) = removed else {
            return;
        };
        if let Some((_, active)) = self.active.remove(&id) {
            active.handle.shared.snapshot.write().active = false;
            active.handle.shared.frames.read().shutdown();
        }
        let _ = self.events.send(SurfaceEvent::Removed { id, connection });
        if let Some(router) = self.router() {
            router.wake();
        }
    }

    fn unassigned_surface(&self, stream: &StreamReady) -> Option<TrackedSurfaceId> {
        graph_surface_index(stream.surface_id.0)
            .and_then(|index| {
                self.tracked_surfaces
                    .iter()
                    .find(|surface| surface.render_target_index == index)
                    .map(|surface| surface.id)
            })
            .filter(|id| !self.active.contains_key(id))
            .or_else(|| {
                self.tracked_surfaces
                    .iter()
                    .find(|surface| {
                        !self.active.contains_key(&surface.id)
                            && !self
                                .pending
                                .iter()
                                .any(|pending| pending.value().surface_id == surface.id)
                    })
                    .map(|surface| surface.id)
            })
    }

    fn shutdown(&self) {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        {
            let _topology = self.topology.lock();
            let pending = self
                .pending
                .iter()
                .map(|pending| *pending.key())
                .collect::<Vec<_>>();
            for offer in pending {
                self.reject_pending_locked(offer);
            }
            let active = self
                .active
                .iter()
                .map(|active| *active.key())
                .collect::<Vec<_>>();
            for id in active {
                let Some((_, active)) = self.active.remove(&id) else {
                    continue;
                };
                if let Some(router) = self.router() {
                    router.revoke(active.connection);
                }
                active.handle.shared.snapshot.write().active = false;
                active.handle.shared.frames.read().shutdown();
                let _ = self.events.send(SurfaceEvent::Removed {
                    id,
                    connection: active.connection,
                });
            }
            if let Some(router) = self.router() {
                router.wake();
            }
        }
        if let Some(router) = self.router() {
            router.shutdown();
        }
    }
}

struct AudioControl {
    stopping: bool,
    desired: Option<(NormalizedVolume, bool)>,
}

struct AudioShared {
    process_id: ProcessId,
    state: RwLock<AudioState>,
    control: Mutex<AudioControl>,
    wake: Condvar,
}

struct AudioWorker {
    shared: Arc<AudioShared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl AudioWorker {
    fn spawn(process_id: ProcessId) -> std::io::Result<Self> {
        let shared = Arc::new(AudioShared {
            process_id,
            state: RwLock::new(AudioState::default()),
            control: Mutex::new(AudioControl {
                stopping: false,
                desired: None,
            }),
            wake: Condvar::new(),
        });
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name(format!("snowglass-audio-{}", process_id.0))
            .spawn(move || run_audio(worker_shared))?;
        Ok(Self {
            shared,
            worker: Mutex::new(Some(worker)),
        })
    }

    fn state(&self) -> AudioState {
        *self.shared.state.read()
    }

    fn set_volume(&self, volume: NormalizedVolume) {
        self.shared.state.write().volume = volume;
        let mut control = self.shared.control.lock();
        let muted = control
            .desired
            .map(|(_, muted)| muted)
            .unwrap_or_else(|| self.shared.state.read().muted);
        control.desired = Some((volume, muted));
        self.shared.wake.notify_one();
    }

    fn set_muted(&self, muted: bool) {
        self.shared.state.write().muted = muted;
        let mut control = self.shared.control.lock();
        let volume = control
            .desired
            .map(|(volume, _)| volume)
            .unwrap_or_else(|| self.shared.state.read().volume);
        control.desired = Some((volume, muted));
        self.shared.wake.notify_one();
    }

    fn shutdown(&self) {
        {
            let mut control = self.shared.control.lock();
            if !control.stopping {
                control.stopping = true;
                self.shared.wake.notify_one();
            }
        }
        if let Some(worker) = self.worker.lock().take() {
            let _ = worker.join();
        }
    }
}

impl Drop for AudioWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_audio(shared: Arc<AudioShared>) {
    loop {
        refresh_audio(&shared);
        let mut control = shared.control.lock();
        if control.stopping {
            return;
        }
        shared
            .wake
            .wait_for(&mut control, Duration::from_millis(500));
        if control.stopping {
            return;
        }
    }
}

fn refresh_audio(shared: &AudioShared) {
    let desired = shared.control.lock().desired;
    let processes = process_tree_ids(shared.process_id).unwrap_or_else(|_| vec![shared.process_id]);
    let mut sessions = 0;

    if let Some((volume, muted)) = desired {
        for process in processes {
            sessions += set_process_audio(process.0, volume.get(), muted).unwrap_or_default();
        }
        *shared.state.write() = AudioState {
            available: sessions != 0,
            volume,
            muted,
            active_sessions: sessions,
        };
        return;
    }

    let mut discovered = None;
    for process in processes {
        let Ok(state) = process_audio_state(process.0) else {
            continue;
        };
        sessions += state.sessions;
        if discovered.is_none() && state.available {
            discovered = Some((
                NormalizedVolume::new(state.volume).unwrap_or_default(),
                state.muted,
            ));
        }
    }
    let (volume, muted) = discovered.unwrap_or_else(|| {
        let current = *shared.state.read();
        (current.volume, current.muted)
    });
    *shared.state.write() = AudioState {
        available: sessions != 0,
        volume,
        muted,
        active_sessions: sessions,
    };
}

struct InstanceLifecycle {
    process: Mutex<Option<TargetProcess>>,
    input: InputTransport,
    ui: UiTransport,
    state: Watchable<InstanceState>,
    surfaces: Arc<SurfaceManager>,
    audio: AudioWorker,
    finishing: AtomicBool,
    monitor: Mutex<Option<JoinHandle<()>>>,
}

impl InstanceLifecycle {
    fn start_monitor(self: &Arc<Self>, wait: ProcessWaitHandle) -> std::io::Result<()> {
        let lifecycle = Arc::downgrade(self);
        let worker = thread::Builder::new()
            .name("snowglass-process-wait".to_owned())
            .spawn(move || {
                let exit = wait.wait().map_or_else(
                    |error| InstanceExit::Failed(Arc::from(error.to_string())),
                    |()| {
                        match wait.exit_code() {
                            Ok(code) => tracing::info!(
                                "snowglass: emulator process exited with code 0x{code:08x}"
                            ),
                            Err(error) => tracing::warn!(
                                "snowglass: failed to read emulator process exit code: {error}"
                            ),
                        }
                        InstanceExit::Natural
                    },
                );
                if let Some(lifecycle) = lifecycle.upgrade() {
                    lifecycle.finish(exit);
                }
            })?;
        *self.monitor.lock() = Some(worker);
        Ok(())
    }

    fn finish(&self, exit: InstanceExit) {
        if self.finishing.swap(true, Ordering::AcqRel) {
            return;
        }
        self.state.replace(InstanceState::Closing);
        // End the producer before waiting for capture/audio transports to
        // shut down. Those workers can be blocked in target-owned IPC while
        // the emulator is alive, which previously prevented close() from ever
        // reaching the process termination step.
        if let Some(process) = self.process.lock().take() {
            let _ = process.terminate_tree_and_wait();
        }
        self.surfaces.shutdown();
        self.input.shutdown();
        self.ui.shutdown();
        self.audio.shutdown();
        self.state.replace(InstanceState::Exited(exit));
        self.state.shutdown();
    }

    fn wait_blocking(&self) -> InstanceExit {
        let watcher = self.state.watch();
        watcher.mark_read();
        loop {
            if let Some(exit) = terminal_exit(&self.state) {
                return exit;
            }
            if watcher.next_value().is_err() {
                return terminal_exit(&self.state).unwrap_or(InstanceExit::Natural);
            }
        }
    }

    fn join_monitor(&self) {
        if let Some(worker) = self.monitor.lock().take() {
            let _ = worker.join();
        }
    }
}

impl Drop for InstanceLifecycle {
    fn drop(&mut self) {
        self.finish(InstanceExit::Terminated);
        self.join_monitor();
    }
}

fn graph_surface_index(surface_id: u64) -> Option<u32> {
    const TAG: u64 = 0x5347_0000_0000_0000;
    const MASK: u64 = 0xffff_0000_0000_0000;
    (surface_id & MASK == TAG)
        .then_some(surface_id & !MASK)
        .and_then(|index| u32::try_from(index).ok())
}

fn stream_backend(kind: ExternalImageKind) -> HookBackend {
    match kind {
        ExternalImageKind::GdiD3d12Resource => HookBackend::Gdi,
        ExternalImageKind::D3d11SharedResourceNt | ExternalImageKind::D3d11D3d12Resource => {
            HookBackend::D3d11
        }
        ExternalImageKind::D3d12Resource | ExternalImageKind::D3d12Heap => HookBackend::D3d12,
        ExternalImageKind::OpenGlD3d12Resource => HookBackend::OpenGl,
        ExternalImageKind::VulkanOpaqueWin32 | ExternalImageKind::VulkanD3d12Resource => {
            HookBackend::Vulkan
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> EmulatorLaunchConfig {
        EmulatorLaunchConfig {
            session: "runtime-test".to_owned(),
            command: "emulator.exe".to_owned(),
            arguments: Vec::new(),
            artifact_root: PathBuf::from("."),
            hooks: vec![HookBackend::D3d12],
            input_hooks: Vec::new(),
            bluetooth_hid_pipes: Vec::new(),
            consumer: ConsumerPreference::D3d12,
            isolate: false,
            tracked_surfaces: vec![TrackedSurfaceConfig {
                id: TrackedSurfaceId(7),
                render_target_index: 0,
                window_pattern: None,
            }],
            tracking_enabled: true,
            skip_surfaces: 0,
            graph_tracking: false,
            force_keyed_mutex: false,
            gamepad_layouts: Vec::new(),
        }
    }

    #[test]
    fn validates_required_launch_configuration() {
        assert!(config().validate().is_ok());

        let mut invalid = config();
        invalid.command.clear();
        assert!(matches!(
            invalid.validate(),
            Err(LaunchError::InvalidConfig(_))
        ));

        let mut invalid = config();
        invalid
            .tracked_surfaces
            .push(invalid.tracked_surfaces[0].clone());
        assert!(matches!(
            invalid.validate(),
            Err(LaunchError::InvalidConfig(_))
        ));
    }

    #[test]
    fn decodes_graph_surface_indices() {
        assert_eq!(graph_surface_index(0x5347_0000_0000_0003), Some(3));
        assert_eq!(graph_surface_index(3), None);
        assert_eq!(graph_surface_index(0x5348_0000_0000_0003), None);
    }

    #[test]
    fn maps_every_external_image_kind_to_a_hook() {
        for (kind, expected) in [
            (ExternalImageKind::GdiD3d12Resource, HookBackend::Gdi),
            (ExternalImageKind::D3d11SharedResourceNt, HookBackend::D3d11),
            (ExternalImageKind::D3d11D3d12Resource, HookBackend::D3d11),
            (ExternalImageKind::D3d12Resource, HookBackend::D3d12),
            (ExternalImageKind::D3d12Heap, HookBackend::D3d12),
            (ExternalImageKind::OpenGlD3d12Resource, HookBackend::OpenGl),
            (ExternalImageKind::VulkanOpaqueWin32, HookBackend::Vulkan),
            (ExternalImageKind::VulkanD3d12Resource, HookBackend::Vulkan),
        ] {
            assert_eq!(stream_backend(kind), expected);
        }
    }

    #[test]
    fn volume_is_finite_and_normalized() {
        assert_eq!(NormalizedVolume::new(-1.0).unwrap().get(), 0.0);
        assert_eq!(NormalizedVolume::new(2.0).unwrap().get(), 1.0);
        assert!(NormalizedVolume::new(f32::NAN).is_none());
    }

    #[test]
    fn watchable_notifications_coalesce_without_losing_the_latest_snapshot() {
        let source = Watchable::new(0_u32);
        let receiver = source.watch();
        receiver.mark_read();

        source.replace(1);
        source.replace(2);

        assert!(receiver.watch_timeout(Duration::ZERO).is_ok());
        assert_eq!(receiver.get(), 2);
        assert_eq!(
            receiver.watch_timeout(Duration::ZERO),
            Err(TimeoutError::Timeout)
        );
        source.shutdown();
        assert!(matches!(
            receiver.watch_timeout(Duration::ZERO),
            Err(TimeoutError::Disconnected)
        ));
    }
}
