use crate::{
    HookBackend,
    capture::{self, CaptureEvent as TransportCaptureEvent, ConsumerPreference, FrameRouter},
    input::{InputTransport, ProtocolInputSender},
    ui::{UiRequestReceiver, UiTransport},
};
use crossfire::{
    MRx, MTx, Rx, mpmc, mpsc,
    oneshot::{self, TxOneshot},
};
use dashmap::DashMap;
use parking_lot::RwLock;
use snowglass_input::{
    RawKeyInput,
    gamepad::{GamepadUpdate, LatestGamepadMailbox},
    mouse::{PointerInput, PointerPhase, PointerSource},
};
use snowglass_ipc::{Message, StreamReady};
use snowglass_platform_windows::{
    NativeWindow, ProcessId, TargetProcess, client_size, process_audio_state, process_tree_ids,
    set_process_audio,
};
use snowglass_protocol::{ExternalImageKind, FrameReady};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use watchable::{TimeoutError, Watchable, Watcher};

pub(crate) type ActorSender = MTx<mpsc::List<ActorMessage>>;
type ActorReceiver = Rx<mpsc::List<ActorMessage>>;
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
pub struct EmulatorConfig {
    pub session: String,
    pub command: Vec<String>,
    pub artifact_root: PathBuf,
    pub hooks: Vec<HookBackend>,
    pub consumer: ConsumerPreference,
    pub isolate: bool,
    pub tracked_surfaces: Vec<TrackedSurfaceConfig>,
    pub tracking_enabled: bool,
    pub skip_surfaces: u32,
    pub graph_tracking: bool,
    pub force_keyed_mutex: bool,
    pub gamepad_layouts: Vec<String>,
    pub stone_database: Option<PathBuf>,
}

impl EmulatorConfig {
    fn validate(&self) -> Result<(), LaunchError> {
        if self.session.is_empty() || self.session.contains(['/', '\\']) {
            return Err(LaunchError::InvalidConfig(
                "session must be a non-empty socket name".to_owned(),
            ));
        }
        if self.command.is_empty() {
            return Err(LaunchError::InvalidConfig(
                "target command must not be empty".to_owned(),
            ));
        }
        if self.hooks.is_empty() {
            return Err(LaunchError::InvalidConfig(
                "at least one graphics hook is required".to_owned(),
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

pub struct InstanceStateReceiver {
    receiver: Watcher<InstanceState>,
}

impl InstanceStateReceiver {
    pub fn latest(&self) -> InstanceState {
        self.receiver.peek().clone()
    }

    pub async fn next_state(&mut self) -> Result<InstanceState, watchable::Disconnected> {
        self.receiver.next_value_async().await
    }

    pub fn try_next_state(&mut self) -> Result<Option<InstanceState>, watchable::Disconnected> {
        match self.receiver.watch_timeout(Duration::ZERO) {
            Ok(()) => Ok(Some(self.receiver.get())),
            Err(TimeoutError::Timeout) => Ok(None),
            Err(TimeoutError::Disconnected) => Err(watchable::Disconnected),
        }
    }
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
        match self.receiver.watch_timeout(Duration::ZERO) {
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
    ReconfigureOffered {
        surface: TrackedSurfaceHandle,
        offer: SurfaceOffer,
    },
    Removed {
        id: TrackedSurfaceId,
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
    actor: ActorSender,
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

    pub const fn stream(&self) -> &StreamReady {
        &self.stream
    }

    pub fn surface(&self) -> &TrackedSurfaceHandle {
        &self.surface
    }

    pub fn accept(mut self) -> Result<TrackedSurfaceHandle, SurfaceError> {
        let (accepted, result) = oneshot::oneshot();
        self.actor
            .send(ActorMessage::AcceptOffer {
                offer: self.offer,
                accepted,
            })
            .map_err(|_| SurfaceError::Unavailable)?;
        if !result.recv().unwrap_or(false) {
            return Err(SurfaceError::Unavailable);
        }
        self.resolved = true;
        Ok(self.surface.clone())
    }

    pub fn reject(mut self) {
        let _ = self.actor.send(ActorMessage::RejectOffer(self.offer));
        self.resolved = true;
    }
}

impl Drop for SurfaceOffer {
    fn drop(&mut self) {
        if !self.resolved {
            let _ = self.actor.send(ActorMessage::RejectOffer(self.offer));
        }
    }
}

pub struct EmulatorInstance {
    process_id: u32,
    hooks: Arc<[HookBackend]>,
    input: InputTransport,
    actor: ActorSender,
    actor_thread: Option<JoinHandle<()>>,
    state: Watchable<InstanceState>,
    surfaces: Arc<DashMap<TrackedSurfaceId, TrackedSurfaceHandle>>,
    events: SurfaceEventReceiver,
    ui_requests: UiRequestReceiver,
    audio: Arc<RwLock<AudioState>>,
    close_on_drop: bool,
}

impl EmulatorInstance {
    pub fn spawn(config: EmulatorConfig) -> Result<Self, LaunchError> {
        Self::spawn_with_waker(config, || {})
    }

    pub fn spawn_with_waker(
        config: EmulatorConfig,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, LaunchError> {
        config.validate()?;
        let hooks: Arc<[HookBackend]> = Arc::from(config.hooks.clone());
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let input = InputTransport::bind(&config.session)?;
        let (ui, ui_requests) = match UiTransport::bind(&config.session, wake.clone()) {
            Ok(ui) => ui,
            Err(error) => {
                input.shutdown();
                return Err(error.into());
            }
        };
        let (actor, messages) = mpsc::unbounded_blocking();
        let router =
            match capture::bind(&config.session, config.consumer, actor.clone(), Some(wake)) {
                Ok(router) => router,
                Err(error) => {
                    input.shutdown();
                    ui.shutdown();
                    return Err(error.into());
                }
            };
        let process = match crate::launch::launch(&config) {
            Ok(process) => process,
            Err(error) => {
                input.shutdown();
                router.shutdown();
                ui.shutdown();
                return Err(error.into());
            }
        };
        let process_id = process.id();
        let state = Watchable::new(InstanceState::Starting);
        let (event_sender, event_receiver) = mpmc::unbounded_blocking();
        let surfaces = Arc::new(DashMap::new());
        let audio = Arc::new(RwLock::new(AudioState::default()));
        let actor_state = ActorState::new(
            config,
            process,
            input.clone(),
            router,
            ui,
            actor.clone(),
            state.clone(),
            event_sender,
            surfaces.clone(),
            audio.clone(),
        );
        let actor_thread = thread::Builder::new()
            .name(format!("snowglass-emulator-{process_id}"))
            .spawn(move || actor_state.run(messages))
            .map_err(LaunchError::Io)?;
        Ok(Self {
            process_id,
            hooks,
            input,
            actor,
            actor_thread: Some(actor_thread),
            state,
            surfaces,
            events: SurfaceEventReceiver(event_receiver),
            ui_requests,
            audio,
            close_on_drop: true,
        })
    }

    pub const fn process_id(&self) -> u32 {
        self.process_id
    }

    pub fn state(&self) -> InstanceState {
        self.state.get()
    }

    pub fn subscribe_state(&self) -> InstanceStateReceiver {
        let receiver = self.state.watch();
        receiver.mark_read();
        InstanceStateReceiver { receiver }
    }

    pub fn configured_hooks(&self) -> Arc<[HookBackend]> {
        self.hooks.clone()
    }

    pub fn active_backends(&self) -> Vec<HookBackend> {
        let mut backends = self
            .surfaces
            .iter()
            .map(|surface| surface.value().backend())
            .collect::<Vec<_>>();
        backends.sort_by_key(|backend| backend.class());
        backends.dedup();
        backends
    }

    pub fn tracked_surface(&self, id: TrackedSurfaceId) -> Option<TrackedSurfaceHandle> {
        self.surfaces
            .get(&id)
            .map(|surface| surface.value().clone())
    }

    pub fn tracked_surfaces(&self) -> Vec<TrackedSurfaceHandle> {
        let mut surfaces = self
            .surfaces
            .iter()
            .map(|surface| surface.value().clone())
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
        self.input.publish_gamepad(update).map_err(SurfaceError::Io)
    }

    pub fn gamepad_mailbox(&self) -> Arc<LatestGamepadMailbox> {
        self.input.gamepad_mailbox()
    }

    /// Compatibility bridge for input encoders that already produce the
    /// existing Snowglass protocol messages. The instance still owns the IPC
    /// connection and writer lifecycle.
    #[doc(hidden)]
    pub fn protocol_input_sender(&self) -> ProtocolInputSender {
        self.input.sender()
    }

    pub fn audio_state(&self) -> AudioState {
        *self.audio.read()
    }

    pub fn set_volume(&self, volume: NormalizedVolume) {
        self.audio.write().volume = volume;
        let _ = self.actor.send(ActorMessage::SetVolume(volume));
    }

    pub fn set_muted(&self, muted: bool) {
        self.audio.write().muted = muted;
        let _ = self.actor.send(ActorMessage::SetMuted(muted));
    }

    pub async fn close(&mut self) -> InstanceExit {
        self.close_on_drop = false;
        let (reply, result) = oneshot::oneshot();
        if self.actor.send(ActorMessage::Close(reply)).is_err() {
            self.join_actor();
            return terminal_exit(&self.state).unwrap_or(InstanceExit::Natural);
        }
        let exit = result
            .await
            .unwrap_or_else(|_| terminal_exit(&self.state).unwrap_or(InstanceExit::Natural));
        self.join_actor();
        exit
    }

    pub fn close_blocking(&mut self) -> InstanceExit {
        self.close_on_drop = false;
        let (reply, result) = oneshot::oneshot();
        if self.actor.send(ActorMessage::Close(reply)).is_err() {
            self.join_actor();
            return terminal_exit(&self.state).unwrap_or(InstanceExit::Natural);
        }
        let exit = result
            .recv()
            .unwrap_or_else(|_| terminal_exit(&self.state).unwrap_or(InstanceExit::Natural));
        self.join_actor();
        exit
    }

    pub async fn wait(&mut self) -> InstanceExit {
        let mut states = self.subscribe_state();
        loop {
            if let Some(exit) = terminal_exit(&self.state) {
                self.close_on_drop = false;
                self.join_actor();
                return exit;
            }
            if states.next_state().await.is_err() {
                return InstanceExit::Natural;
            }
        }
    }

    fn join_actor(&mut self) {
        if let Some(actor) = self.actor_thread.take() {
            let _ = actor.join();
        }
    }
}

impl Drop for EmulatorInstance {
    fn drop(&mut self) {
        if self.close_on_drop {
            let _ = self.close_blocking();
        } else {
            self.join_actor();
        }
    }
}

fn terminal_exit(state: &Watchable<InstanceState>) -> Option<InstanceExit> {
    match state.get() {
        InstanceState::Exited(exit) => Some(exit.clone()),
        _ => None,
    }
}

pub(crate) enum ActorMessage {
    Capture(TransportCaptureEvent),
    AcceptOffer {
        offer: u64,
        accepted: TxOneshot<bool>,
    },
    RejectOffer(u64),
    SetVolume(NormalizedVolume),
    SetMuted(bool),
    Close(TxOneshot<InstanceExit>),
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

struct ActorState {
    config: EmulatorConfig,
    process: Option<TargetProcess>,
    process_id: ProcessId,
    input: InputTransport,
    router: FrameRouter,
    ui: UiTransport,
    actor: ActorSender,
    state: Watchable<InstanceState>,
    events: SurfaceEventSender,
    public_surfaces: Arc<DashMap<TrackedSurfaceId, TrackedSurfaceHandle>>,
    audio: Arc<RwLock<AudioState>>,
    audio_override: Option<(NormalizedVolume, bool)>,
    next_audio_refresh: Instant,
    active: HashMap<TrackedSurfaceId, ActiveSurface>,
    pending: HashMap<u64, PendingOffer>,
    next_offer: u64,
    closing_reply: Option<TxOneshot<InstanceExit>>,
    remaining_skips: u32,
    skipped_surfaces: HashSet<(u64, HookBackend)>,
}

impl ActorState {
    #[allow(clippy::too_many_arguments)]
    fn new(
        config: EmulatorConfig,
        process: TargetProcess,
        input: InputTransport,
        router: FrameRouter,
        ui: UiTransport,
        actor: ActorSender,
        state: Watchable<InstanceState>,
        events: SurfaceEventSender,
        public_surfaces: Arc<DashMap<TrackedSurfaceId, TrackedSurfaceHandle>>,
        audio: Arc<RwLock<AudioState>>,
    ) -> Self {
        let remaining_skips = config.skip_surfaces;
        let process_id = ProcessId(process.id());
        Self {
            config,
            process: Some(process),
            process_id,
            input,
            router,
            ui,
            actor,
            state,
            events,
            public_surfaces,
            audio,
            audio_override: None,
            next_audio_refresh: Instant::now(),
            active: HashMap::new(),
            pending: HashMap::new(),
            next_offer: 1,
            closing_reply: None,
            remaining_skips,
            skipped_surfaces: HashSet::new(),
        }
    }

    fn run(mut self, messages: ActorReceiver) {
        self.state.replace(InstanceState::Running);
        loop {
            match messages.recv_timeout(Duration::from_millis(8)) {
                Ok(message) => self.handle(message),
                Err(crossfire::RecvTimeoutError::Disconnected) => self.begin_close(None),
                Err(crossfire::RecvTimeoutError::Timeout) => {}
            }
            if self.poll_process() {
                return;
            }
            self.poll_audio();
            if matches!(self.state.get(), InstanceState::Closing) {
                self.finish_close();
                return;
            }
        }
    }

    fn handle(&mut self, message: ActorMessage) {
        match message {
            ActorMessage::Capture(event) => self.capture(event),
            ActorMessage::AcceptOffer { offer, accepted } => {
                let result = self.resolve_offer(offer, true);
                accepted.send(result);
            }
            ActorMessage::RejectOffer(offer) => {
                self.resolve_offer(offer, false);
            }
            ActorMessage::SetVolume(volume) => {
                let muted = self
                    .audio_override
                    .map(|(_, muted)| muted)
                    .unwrap_or_else(|| self.audio.read().muted);
                self.audio_override = Some((volume, muted));
                self.next_audio_refresh = Instant::now();
            }
            ActorMessage::SetMuted(muted) => {
                let volume = self
                    .audio_override
                    .map(|(volume, _)| volume)
                    .unwrap_or_else(|| self.audio.read().volume);
                self.audio_override = Some((volume, muted));
                self.next_audio_refresh = Instant::now();
            }
            ActorMessage::Close(reply) => self.begin_close(Some(reply)),
        }
    }

    fn capture(&mut self, event: TransportCaptureEvent) {
        match event {
            TransportCaptureEvent::Stream {
                connection,
                stream,
                accepted,
            } => self.offer(connection, stream, accepted),
            TransportCaptureEvent::Disconnected { connection } => self.disconnected(connection),
        }
    }

    fn offer(&mut self, connection: u64, stream: StreamReady, accepted: TxOneshot<bool>) {
        let backend = stream_backend(stream.image_kind);
        let Some(incoming_rank) = self.config.hooks.iter().position(|hook| *hook == backend) else {
            accepted.send(false);
            return;
        };
        let skip_identity = (stream.surface_id.0, backend);
        if self.skipped_surfaces.contains(&skip_identity) || self.remaining_skips != 0 {
            if !self.skipped_surfaces.contains(&skip_identity) {
                self.remaining_skips -= 1;
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
            .find(|(_, active)| {
                stream.surface_id.0 != 0 && active.protocol_surface_id == stream.surface_id.0
            })
            .map(|(&id, active)| (id, active.connection, active.handle.backend()));
        let pending = self
            .pending
            .iter()
            .find(|(_, pending)| {
                stream.surface_id.0 != 0 && pending.stream.surface_id.0 == stream.surface_id.0
            })
            .map(|(&offer, pending)| {
                (
                    offer,
                    pending.surface_id,
                    stream_backend(pending.stream.image_kind),
                )
            });
        let (surface_id, replacing) = if let Some((offer, id, old_backend)) = pending {
            let old_rank = self
                .config
                .hooks
                .iter()
                .position(|hook| *hook == old_backend)
                .unwrap_or(usize::MAX);
            if incoming_rank > old_rank {
                accepted.send(false);
                return;
            }
            self.resolve_offer(offer, false);
            (id, None)
        } else if let Some((id, old_connection, old_backend)) = existing {
            let old_rank = self
                .config
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
            .map_or(1, |active| active.handle.snapshot().generation + 1);
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
            .map(|active| active.handle.clone())
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
        self.router.route(connection, frames.clone());
        let offer_id = self.next_offer;
        self.next_offer += 1;
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
            actor: self.actor.clone(),
            resolved: false,
        };
        let event = if replacing.is_some() {
            SurfaceEvent::ReconfigureOffered {
                surface: handle,
                offer,
            }
        } else {
            SurfaceEvent::Offered(offer)
        };
        if self.events.send(event).is_err() {
            self.resolve_offer(offer_id, false);
        } else {
            self.router.wake();
        }
    }

    fn resolve_offer(&mut self, offer: u64, accept: bool) -> bool {
        let Some(pending) = self.pending.remove(&offer) else {
            return false;
        };
        if !accept {
            pending.frames.shutdown();
            self.router.revoke(pending.connection);
            pending.accepted.send(false);
            return true;
        }
        if let Some(replaced) = pending.replacing {
            self.router.revoke(replaced);
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
        self.public_surfaces
            .insert(pending.surface_id, pending.handle);
        pending.accepted.send(true);
        true
    }

    fn disconnected(&mut self, connection: u64) {
        let removed = self
            .active
            .iter()
            .find_map(|(&id, active)| (active.connection == connection).then_some(id));
        let Some(id) = removed else {
            return;
        };
        if let Some(active) = self.active.remove(&id) {
            active.handle.shared.snapshot.write().active = false;
            active.handle.shared.frames.read().shutdown();
        }
        self.public_surfaces.remove(&id);
        let _ = self.events.send(SurfaceEvent::Removed { id });
        self.router.wake();
    }

    fn unassigned_surface(&self, stream: &StreamReady) -> Option<TrackedSurfaceId> {
        graph_surface_index(stream.surface_id.0)
            .and_then(|index| {
                self.config
                    .tracked_surfaces
                    .iter()
                    .find(|surface| surface.render_target_index == index)
                    .map(|surface| surface.id)
            })
            .filter(|id| !self.active.contains_key(id))
            .or_else(|| {
                self.config
                    .tracked_surfaces
                    .iter()
                    .find(|surface| {
                        !self.active.contains_key(&surface.id)
                            && !self
                                .pending
                                .values()
                                .any(|pending| pending.surface_id == surface.id)
                    })
                    .map(|surface| surface.id)
            })
    }

    fn begin_close(&mut self, reply: Option<TxOneshot<InstanceExit>>) {
        if let Some(reply) = reply {
            self.closing_reply = Some(reply);
        }
        self.state.replace(InstanceState::Closing);
    }

    fn poll_audio(&mut self) {
        let now = Instant::now();
        if now < self.next_audio_refresh {
            return;
        }
        self.next_audio_refresh = now + Duration::from_millis(500);
        let processes = process_tree_ids(self.process_id).unwrap_or_else(|_| vec![self.process_id]);
        let mut sessions = 0;

        if let Some((volume, muted)) = self.audio_override {
            for process in processes {
                sessions += set_process_audio(process.0, volume.get(), muted).unwrap_or_default();
            }
            *self.audio.write() = AudioState {
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
            let current = *self.audio.read();
            (current.volume, current.muted)
        });
        *self.audio.write() = AudioState {
            available: sessions != 0,
            volume,
            muted,
            active_sessions: sessions,
        };
    }

    fn poll_process(&mut self) -> bool {
        let Some(process) = self.process.as_mut() else {
            return true;
        };
        match process.try_wait() {
            Ok(Some(())) => {
                self.finish(InstanceExit::Natural);
                true
            }
            Ok(None) => false,
            Err(error) => {
                self.finish(InstanceExit::Failed(Arc::from(error.to_string())));
                true
            }
        }
    }

    fn finish_close(&mut self) {
        self.finish(InstanceExit::Terminated);
    }

    fn finish(&mut self, exit: InstanceExit) {
        self.input.shutdown();
        for pending in self.pending.drain().map(|(_, pending)| pending) {
            pending.frames.shutdown();
            self.router.revoke(pending.connection);
            pending.accepted.send(false);
        }
        for (&id, active) in &self.active {
            self.router.revoke(active.connection);
            active.handle.shared.snapshot.write().active = false;
            active.handle.shared.frames.read().shutdown();
            let _ = self.events.send(SurfaceEvent::Removed { id });
        }
        self.router.shutdown();
        self.ui.shutdown();
        self.router.wake();
        self.active.clear();
        self.public_surfaces.clear();
        if let Some(process) = self.process.take() {
            let _ = process.terminate_tree_and_wait();
        }
        self.state.replace(InstanceState::Exited(exit.clone()));
        if let Some(reply) = self.closing_reply.take() {
            reply.send(exit);
        }
    }
}

impl Drop for ActorState {
    fn drop(&mut self) {
        self.input.shutdown();
        self.router.shutdown();
        self.ui.shutdown();
        if let Some(process) = self.process.take() {
            let _ = process.terminate_tree_and_wait();
        }
        self.state.shutdown();
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

    fn config() -> EmulatorConfig {
        EmulatorConfig {
            session: "runtime-test".to_owned(),
            command: vec!["emulator.exe".to_owned()],
            artifact_root: PathBuf::from("."),
            hooks: vec![HookBackend::D3d12],
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
            stone_database: None,
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
