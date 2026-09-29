use super::*;

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
    pub(super) lifecycle: Arc<InstanceLifecycle>,
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
pub(super) struct SurfaceSnapshot {
    pub(super) generation: u64,
    pub(super) protocol_surface_id: u64,
    pub(super) target_window: i64,
    pub(super) backend: GraphicsHook,
    pub(super) transport_dimensions: ResizeDimensions,
    pub(super) active: bool,
}

pub(super) struct SurfaceShared {
    pub(super) snapshot: RwLock<SurfaceSnapshot>,
    pub(super) guest: Weak<Mutex<Option<Box<dyn Guest>>>>,
    pub(super) frames: RwLock<Watchable<Option<FrameReady>>>,
}

#[derive(Clone)]
pub struct TrackedSurfaceHandle {
    pub(super) id: TrackedSurfaceId,
    pub(super) shared: Arc<SurfaceShared>,
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

    pub(super) fn snapshot(&self) -> SurfaceSnapshot {
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
    pub(super) offer: u64,
    pub(super) connection: u64,
    pub(super) stream: StreamReady,
    pub(super) surface: TrackedSurfaceHandle,
    pub(super) replacing: Option<u64>,
    pub(super) manager: Weak<SurfaceManager>,
    pub(super) resolved: bool,
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
