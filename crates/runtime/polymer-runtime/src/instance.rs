use crate::{
    GraphicsHook, Hooks,
    capture::{self, ConsumerPreference, FrameRouter},
};
use crossfire::{MRx, MTx, mpmc, oneshot::TxOneshot};
use dashmap::{DashMap, DashSet};
use parking_lot::{Mutex, RwLock};
use polymer_input::{HostInputEvent, HostPointerInput};
use polymer_ipc::StreamReady;
use polymer_ipc::input::host::InputIpcListener;
pub use polymer_platform::{AudioState, HotkeyChord, NormalizedVolume, ResizeDimensions};
use polymer_platform::{Guest, GuestInput, GuestWindowId, ProcessExitReceiver};

use polymer_protocol::{
    ExternalImageKind, FrameReady,
    input::{PointerPhase, PointerSource},
};
use std::{
    collections::HashSet,
    fmt,
    path::PathBuf,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use watchable::{TimeoutError, Watchable, Watcher};

type SurfaceEventSender = MTx<mpmc::List<SurfaceEvent>>;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TrackedSurfaceId(pub u32);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackedSurfaceConfig {
    pub id: TrackedSurfaceId,
    pub render_target_index: u32,
    pub window_pattern: Option<String>,
    pub rotation: i32,
}

#[derive(Clone, Debug)]
pub struct EmulatorLaunchConfig {
    pub session: String,
    pub command: String,
    pub working_directory: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub arguments: Vec<String>,
    pub artifact_root: PathBuf,
    pub hooks: Hooks,
    pub consumer: ConsumerPreference,
    pub isolate: bool,
    pub tracked_surfaces: Vec<TrackedSurfaceConfig>,
    pub tracking_enabled: bool,
    pub skip_surfaces: u32,
}

impl EmulatorLaunchConfig {
    pub fn render_configuration(&self) -> crate::RenderConfiguration {
        crate::RenderConfiguration {
            surfaces: self
                .tracked_surfaces
                .iter()
                .map(|surface| crate::SurfaceRequest {
                    index: surface.render_target_index,
                    pattern: surface.window_pattern.clone(),
                    rotation: surface.rotation,
                })
                .collect(),
        }
    }

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
        if self.hooks.graphics.is_empty() {
            return Err(LaunchError::InvalidConfig(
                "at least one graphics hook is required".to_owned(),
            ));
        }
        self.render_configuration()
            .validate()
            .map_err(LaunchError::InvalidConfig)?;
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

#[derive(Clone)]
pub struct EmulatorActionEndpoint {
    lifecycle: Arc<InstanceLifecycle>,
}

impl EmulatorActionEndpoint {
    pub fn activate_keyboard(&self, code: &str, modifiers: &[String]) -> Result<(), String> {
        let surface = self
            .lifecycle
            .surfaces
            .active
            .iter()
            .min_by_key(|surface| surface.key().0)
            .map(|surface| surface.value().handle.clone())
            .ok_or_else(|| "emulator action has no active target surface".to_owned())?;
        surface
            .send_hotkey(&HotkeyChord {
                modifiers: modifiers.to_vec(),
                key: code.to_owned(),
            })
            .map_err(|error| error.to_string())
    }

    pub fn activate_bindings(&self, player: u8, keys: &[String]) -> Result<(), String> {
        let mut buttons = 0u32;
        for key in keys {
            let key = key
                .parse::<consolespec::input::BindingKey>()
                .map_err(|error| error.to_string())?;
            let Some(polymer_controllers::model::Control::Button(button)) =
                polymer_controllers::model::Control::from_key(key)
            else {
                return Err(format!("action binding '{key}' is not a gamepad button"));
            };
            buttons |= 1 << button;
        }
        self.lifecycle
            .input
            .activate_buttons(polymer_input::PlayerIndex::new(player), buttons)
    }
}

#[derive(Clone, Debug)]
struct SurfaceSnapshot {
    generation: u64,
    protocol_surface_id: u64,
    target_window: i64,
    backend: GraphicsHook,
    transport_dimensions: ResizeDimensions,
    active: bool,
}

struct SurfaceShared {
    snapshot: RwLock<SurfaceSnapshot>,
    guest: Weak<Mutex<Option<Box<dyn Guest>>>>,
    frames: RwLock<Watchable<Option<FrameReady>>>,
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

    pub fn backend(&self) -> GraphicsHook {
        self.snapshot().backend
    }

    pub fn target_window(&self) -> Option<GuestWindowId> {
        let window = self.snapshot().target_window;
        (window != 0).then_some(GuestWindowId(window))
    }

    pub fn transport_dimensions(&self) -> ResizeDimensions {
        self.snapshot().transport_dimensions
    }

    pub fn window_dimensions(&self) -> Result<ResizeDimensions, SurfaceError> {
        let window = self.target_window().ok_or(SurfaceError::Unavailable)?;
        let guest = self
            .shared
            .guest
            .upgrade()
            .ok_or(SurfaceError::Unavailable)?;
        let guard = guest.lock();
        guard
            .as_ref()
            .ok_or(SurfaceError::Unavailable)?
            .window_dimensions(window)
            .map_err(SurfaceError::Io)
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
        let width = f64::from(update.coordinate_width.max(1));
        let height = f64::from(update.coordinate_height.max(1));
        let guest = self
            .shared
            .guest
            .upgrade()
            .ok_or(SurfaceError::Unavailable)?;
        let guard = guest.lock();
        let guest = guard.as_ref().ok_or(SurfaceError::Unavailable)?;
        guest
            .send_input(GuestInput::Host(HostInputEvent::Pointer(
                HostPointerInput {
                    surface_id: snapshot.protocol_surface_id,
                    target_window: snapshot.target_window,
                    source: update.source as u8,
                    phase: update.phase as u8,
                    pointer_id: update.pointer_id,
                    x: update.x,
                    y: update.y,
                    coordinate_width: update.coordinate_width,
                    coordinate_height: update.coordinate_height,
                    button: update.button,
                    position: [f64::from(update.x) / width, f64::from(update.y) / height],
                    delta: [0.0; 2],
                    buttons: update.buttons,
                    wheel_horizontal: update.wheel_horizontal,
                    wheel_vertical: update.wheel_vertical,
                    timestamp_ms: update.timestamp,
                },
            )))
            .map_err(SurfaceError::Io)?;
        if update.wheel_horizontal != 0 || update.wheel_vertical != 0 {
            guest
                .send_input(GuestInput::Host(HostInputEvent::Wheel {
                    horizontal: update.wheel_horizontal,
                    vertical: update.wheel_vertical,
                }))
                .map_err(SurfaceError::Io)?;
        }
        Ok(())
    }

    pub fn send_hotkey(&self, chord: &HotkeyChord) -> Result<(), SurfaceError> {
        let snapshot = self.snapshot();
        self.validate_active_generation(&snapshot, snapshot.generation)?;
        let guest = self
            .shared
            .guest
            .upgrade()
            .ok_or(SurfaceError::Unavailable)?;
        let guard = guest.lock();
        guard
            .as_ref()
            .ok_or(SurfaceError::Unavailable)?
            .send_input(GuestInput::Hotkey {
                window: GuestWindowId(snapshot.target_window),
                chord: chord.clone(),
            })
            .map_err(SurfaceError::Io)
    }

    pub fn resize_window(&self, dimensions: ResizeDimensions) -> Result<(), SurfaceError> {
        if dimensions.width == 0 || dimensions.height == 0 {
            return Err(SurfaceError::InvalidDimensions);
        }
        let snapshot = self.snapshot();
        self.validate_active_generation(&snapshot, snapshot.generation)?;
        let guest = self
            .shared
            .guest
            .upgrade()
            .ok_or(SurfaceError::Unavailable)?;
        let guard = guest.lock();
        guard
            .as_ref()
            .ok_or(SurfaceError::Unavailable)?
            .resize_window(GuestWindowId(snapshot.target_window), dimensions)
            .map_err(SurfaceError::Io)
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

pub type SurfaceEventReceiver = MRx<mpmc::List<SurfaceEvent>>;

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
    #[cfg(target_os = "macos")]
    pub fn metal_resources(&self) -> Option<polymer_ipc::xpc::MetalResources> {
        self.manager
            .upgrade()?
            .router()?
            .metal_resources(self.connection)
    }

    pub fn buffer_release_sender(&self) -> Option<crate::capture::BufferReleaseSender> {
        self.manager
            .upgrade()?
            .router()
            .map(|router| router.buffer_release_sender(self.connection))
    }
    pub const fn id(&self) -> TrackedSurfaceId {
        self.surface.id()
    }

    pub fn backend(&self) -> GraphicsHook {
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
}

impl EmulatorInstance {
    pub fn launch(
        config: EmulatorLaunchConfig,
        filesystem: &polymer_vfs::MountedFilesystem,
    ) -> Result<Self, LaunchError> {
        Self::launch_with_waker(config, filesystem, || {})
    }

    pub fn launch_with_waker(
        config: EmulatorLaunchConfig,
        filesystem: &polymer_vfs::MountedFilesystem,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, LaunchError> {
        Self::launch_with_input(config, filesystem, wake, |_| Ok(()))
    }

    pub fn launch_with_input(
        config: EmulatorLaunchConfig,
        filesystem: &polymer_vfs::MountedFilesystem,
        wake: impl Fn() + Send + Sync + 'static,
        configure_input: impl FnOnce(InputIpcListener) -> Result<(), String>,
    ) -> Result<Self, LaunchError> {
        config.validate()?;
        let request = config.guest_request(filesystem)?;
        let guest = Arc::new(Mutex::new(None));
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let input = InputIpcListener::bind(&config.session)?;
        if let Err(error) = configure_input(input.clone()) {
            input.shutdown();
            return Err(LaunchError::InvalidConfig(error));
        }
        let (event_sender, event_receiver) = mpmc::unbounded_blocking();
        let surfaces = Arc::new(SurfaceManager::new(
            &config,
            Arc::downgrade(&guest),
            event_sender,
        ));
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
                return Err(error.into());
            }
        };
        surfaces.install_router(router.clone());
        let mut process =
            match crate::launch::GuestPlatform::launch(&request, filesystem, input.clone()) {
                Ok(process) => process,
                Err(error) => {
                    surfaces.shutdown();
                    input.shutdown();
                    return Err(error.into());
                }
            };
        let process_id = process.id().0;
        let wait_handle = match process.subscribe_exit() {
            Ok(wait_handle) => wait_handle,
            Err(error) => {
                let _ = process.terminate_tree_and_wait();
                surfaces.shutdown();
                input.shutdown();
                return Err(error.into());
            }
        };
        *guest.lock() = Some(process);
        let state = Watchable::new(InstanceState::Starting);
        let lifecycle = Arc::new(InstanceLifecycle {
            guest,
            input: input.clone(),
            state: state.clone(),
            surfaces: surfaces.clone(),
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
            events: event_receiver,
        })
    }

    pub const fn process_id(&self) -> u32 {
        self.process_id
    }

    pub fn state(&self) -> InstanceState {
        self.lifecycle.state.get()
    }

    pub fn input(&self) -> InputIpcListener {
        self.lifecycle.input.clone()
    }

    pub fn action_endpoint(&self) -> EmulatorActionEndpoint {
        EmulatorActionEndpoint {
            lifecycle: self.lifecycle.clone(),
        }
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

    pub fn audio_state(&self) -> AudioState {
        self.lifecycle
            .guest
            .lock()
            .as_ref()
            .and_then(|guest| guest.audio_state().ok())
            .unwrap_or_default()
    }

    pub fn set_volume(&self, volume: NormalizedVolume) {
        if let Some(guest) = self.lifecycle.guest.lock().as_ref()
            && let Err(error) = guest.set_volume(volume)
        {
            tracing::warn!("polymer: setting guest volume failed: {error}");
        }
    }

    pub fn set_muted(&self, muted: bool) {
        if let Some(guest) = self.lifecycle.guest.lock().as_ref()
            && let Err(error) = guest.set_muted(muted)
        {
            tracing::warn!("polymer: setting guest mute failed: {error}");
        }
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
    hooks: Arc<[GraphicsHook]>,
    tracked_surfaces: Arc<[TrackedSurfaceConfig]>,
    guest: Weak<Mutex<Option<Box<dyn Guest>>>>,
    router: RwLock<Option<FrameRouter>>,
    events: SurfaceEventSender,
    active: DashMap<TrackedSurfaceId, ActiveSurface>,
    pending: DashMap<u64, PendingOffer>,
    topology: Mutex<()>,
    next_offer: AtomicU64,
    remaining_skips: AtomicU32,
    skipped_surfaces: DashSet<(u64, GraphicsHook)>,
    stopping: AtomicBool,
}

impl SurfaceManager {
    fn new(
        config: &EmulatorLaunchConfig,
        guest: Weak<Mutex<Option<Box<dyn Guest>>>>,
        events: SurfaceEventSender,
    ) -> Self {
        Self {
            hooks: Arc::from(config.hooks.graphics.clone()),
            tracked_surfaces: Arc::from(config.tracked_surfaces.clone()),
            guest,
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
                    guest: self.guest.clone(),
                    frames: RwLock::new(frames.clone()),
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
        let index = logical_surface_index(stream.surface_id.0)?;
        self.tracked_surfaces
            .iter()
            .find(|surface| {
                surface.render_target_index == index
                    && !self.active.contains_key(&surface.id)
                    && !self
                        .pending
                        .iter()
                        .any(|pending| pending.value().surface_id == surface.id)
            })
            .map(|surface| surface.id)
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

struct InstanceLifecycle {
    guest: Arc<Mutex<Option<Box<dyn Guest>>>>,
    input: InputIpcListener,
    state: Watchable<InstanceState>,
    surfaces: Arc<SurfaceManager>,
    finishing: AtomicBool,
    monitor: Mutex<Option<JoinHandle<()>>>,
}

impl InstanceLifecycle {
    fn start_monitor(self: &Arc<Self>, wait: ProcessExitReceiver) -> std::io::Result<()> {
        let lifecycle = Arc::downgrade(self);
        let worker = thread::Builder::new()
            .name("polymer-process-wait".to_owned())
            .spawn(move || {
                let exit = match wait.recv() {
                    Ok(Ok(exit)) => {
                        tracing::info!(?exit, "polymer: emulator process exited");
                        InstanceExit::Natural
                    }
                    Ok(Err(error)) => InstanceExit::Failed(Arc::from(error.to_string())),
                    Err(error) => InstanceExit::Failed(Arc::from(error.to_string())),
                };
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
        if let Some(mut process) = self.guest.lock().take() {
            let _ = process.terminate_tree_and_wait();
        }
        self.surfaces.shutdown();
        self.input.shutdown();
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

fn logical_surface_index(surface_id: u64) -> Option<u32> {
    const TAG: u64 = 0x5347_0000_0000_0000;
    const MASK: u64 = 0xffff_0000_0000_0000;
    (surface_id & MASK == TAG)
        .then_some(surface_id & !MASK)
        .and_then(|index| u32::try_from(index).ok())
}

fn stream_backend(kind: ExternalImageKind) -> GraphicsHook {
    match kind {
        ExternalImageKind::IOSurface => GraphicsHook::Metal,
        ExternalImageKind::D3d11D3d12Resource => GraphicsHook::D3d11,
        ExternalImageKind::D3d12Resource | ExternalImageKind::D3d12Heap => GraphicsHook::D3d12,
        ExternalImageKind::OpenGlD3d12Resource => GraphicsHook::OpenGl,
        ExternalImageKind::VulkanOpaqueWin32 | ExternalImageKind::VulkanD3d12Resource => {
            GraphicsHook::Vulkan
        }
    }
}
