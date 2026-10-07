#![forbid(unsafe_code)]

mod source_registry;

use parking_lot::{Mutex, RwLock};
use polymer_input::{
    BackendError, BackendHandle, BackendId, BatteryState, Calibration, Capabilities,
    DeviceDescriptor, DeviceId, DeviceRole, DeviceState, Feedback, GamepadReport, InputBackend,
    PointerFlags, PointerKind, PointerReport, PointerSample, SensorReport,
};
use sdl3::{
    EventSubsystem,
    event::{Event, EventType},
    gamepad::{Axis, Button, Gamepad},
    sensor::SensorType,
};
use source_registry::{DeviceFingerprint, DeviceIdentity, SourceRegistry};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const EMBEDDED_GAMECONTROLLER_DB: &str =
    include_str!("../SDL_GameControllerDB/gamecontrollerdb.txt");

#[derive(Clone, Debug)]
pub struct Sdl3Config {
    pub gamecontroller_database: Option<PathBuf>,
    pub poll_interval: Duration,
}

impl Default for Sdl3Config {
    fn default() -> Self {
        Self {
            gamecontroller_database: None,
            poll_interval: Duration::from_millis(1),
        }
    }
}

enum Command {
    Rumble {
        device: DeviceId,
        low: u16,
        high: u16,
        duration_ms: u32,
    },
}

pub struct Sdl3Backend {
    config: Sdl3Config,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
    commands: Mutex<Option<mpsc::Sender<Command>>>,
    calibrations: Arc<RwLock<HashMap<DeviceId, Calibration>>>,
}

impl Sdl3Backend {
    pub fn new(config: Sdl3Config) -> Result<Self, BackendError> {
        if config.poll_interval.is_zero() {
            return Err(BackendError::Failed(
                "SDL poll interval must be non-zero".to_owned(),
            ));
        }
        Ok(Self {
            config,
            stop: Arc::new(AtomicBool::new(false)),
            thread: Mutex::new(None),
            commands: Mutex::new(None),
            calibrations: Arc::new(RwLock::new(HashMap::new())),
        })
    }
}

impl InputBackend for Sdl3Backend {
    fn id(&self) -> BackendId {
        BackendId::SDL3
    }

    fn start(&self, handle: BackendHandle) -> Result<(), BackendError> {
        if self.thread.lock().is_some() {
            return Err(BackendError::Failed(
                "SDL3 backend is already running".to_owned(),
            ));
        }
        self.stop.store(false, Ordering::Release);
        let (commands, command_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let config = self.config.clone();
        let stop = self.stop.clone();
        let calibrations = self.calibrations.clone();
        let worker = thread::Builder::new()
            .name("polymer-hook-input-sdl3".to_owned())
            .spawn(move || Worker::run(config, stop, calibrations, command_rx, handle, started_tx))
            .map_err(|error| BackendError::Failed(error.to_string()))?;
        match started_rx.recv() {
            Ok(Ok(())) => {
                *self.commands.lock() = Some(commands);
                *self.thread.lock() = Some(worker);
                Ok(())
            }
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(error) => {
                let _ = worker.join();
                Err(BackendError::Failed(error.to_string()))
            }
        }
    }

    fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        self.commands.lock().take();
        if let Some(worker) = self.thread.lock().take() {
            let _ = worker.join();
        }
    }

    fn apply_feedback(&self, device: &DeviceId, feedback: &Feedback) -> Result<(), BackendError> {
        match feedback {
            Feedback::Rumble {
                low_frequency,
                high_frequency,
                duration_ms,
            } => self
                .commands
                .lock()
                .as_ref()
                .ok_or_else(|| BackendError::Unavailable("SDL3 backend is not running".to_owned()))?
                .send(Command::Rumble {
                    device: device.clone(),
                    low: *low_frequency,
                    high: *high_frequency,
                    duration_ms: *duration_ms,
                })
                .map_err(|error| BackendError::Failed(error.to_string())),
            Feedback::Calibrate(calibration) => {
                self.calibrations
                    .write()
                    .insert(device.clone(), calibration.clone());
                Ok(())
            }
            _ => Err(BackendError::Unsupported),
        }
    }
}

impl Drop for Sdl3Backend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Pad {
    gamepad: Gamepad,
    instance: sdl3::joystick::JoystickId,
    device: DeviceId,
    descriptor: DeviceDescriptor,
    active: bool,
    awaiting_button_release: bool,
    state: DeviceState,
    touch: Option<[f32; 2]>,
    contact: bool,
}

struct Worker {
    _sdl: sdl3::Sdl,
    subsystem: sdl3::GamepadSubsystem,
    event_pump: sdl3::EventPump,
    stop: Arc<AtomicBool>,
    calibrations: Arc<RwLock<HashMap<DeviceId, Calibration>>>,
    commands: mpsc::Receiver<Command>,
    handle: BackendHandle,
    pads: Vec<Pad>,
    registry: SourceRegistry,
    next_refresh: Instant,
    poll_interval: Duration,
}

impl Worker {
    fn run(
        config: Sdl3Config,
        stop: Arc<AtomicBool>,
        calibrations: Arc<RwLock<HashMap<DeviceId, Calibration>>>,
        commands: mpsc::Receiver<Command>,
        handle: BackendHandle,
        started: mpsc::SyncSender<Result<(), BackendError>>,
    ) {
        let mut worker = match Self::new(config, stop, calibrations, commands, handle) {
            Ok(worker) => worker,
            Err(error) => {
                let _ = started.send(Err(error));
                return;
            }
        };
        worker.subsystem.update();
        worker.refresh(false);
        let _ = started.send(Ok(()));
        while !worker.stop.load(Ordering::Acquire) {
            worker.poll_events();
            worker.subsystem.update();
            worker.refresh(true);
            worker.drain_commands();
            worker.publish();
            let interval = if worker.pads.is_empty() {
                worker
                    .next_refresh
                    .saturating_duration_since(Instant::now())
                    .max(worker.poll_interval)
            } else {
                worker.poll_interval
            };
            thread::sleep(interval);
        }
        for pad in worker.pads.drain(..) {
            if pad.active {
                worker.handle.disconnected(&pad.device);
            }
        }
    }

    fn new(
        config: Sdl3Config,
        stop: Arc<AtomicBool>,
        calibrations: Arc<RwLock<HashMap<DeviceId, Calibration>>>,
        commands: mpsc::Receiver<Command>,
        handle: BackendHandle,
    ) -> Result<Self, BackendError> {
        let sdl = sdl3::init().map_err(|error| BackendError::Unavailable(error.to_string()))?;
        let subsystem = sdl
            .gamepad()
            .map_err(|error| BackendError::Unavailable(error.to_string()))?;
        for event in [
            EventType::JoyAxisMotion,
            EventType::JoyHatMotion,
            EventType::JoyButtonDown,
            EventType::JoyButtonUp,
            EventType::ControllerAxisMotion,
            EventType::ControllerButtonDown,
            EventType::ControllerButtonUp,
            EventType::ControllerSensorUpdated,
        ] {
            EventSubsystem::set_event_enabled(event, false);
        }
        if let Some(path) = &config.gamecontroller_database {
            subsystem
                .load_mappings(path)
                .map_err(|error| BackendError::Failed(error.to_string()))?;
        } else {
            subsystem
                .load_mappings_from_read(&mut EMBEDDED_GAMECONTROLLER_DB.as_bytes())
                .map_err(|error| BackendError::Failed(error.to_string()))?;
        }
        let event_pump = sdl
            .event_pump()
            .map_err(|error| BackendError::Unavailable(error.to_string()))?;
        Ok(Self {
            _sdl: sdl,
            subsystem,
            event_pump,
            stop,
            calibrations,
            commands,
            handle,
            pads: Vec::new(),
            registry: SourceRegistry::default(),
            next_refresh: Instant::now(),
            poll_interval: config.poll_interval,
        })
    }

    fn poll_events(&mut self) {
        for event in self.event_pump.poll_iter().take(128) {
            let (which, touchpad, finger, position, contact) = match event {
                Event::ControllerTouchpadDown {
                    which,
                    touchpad,
                    finger,
                    x,
                    y,
                    ..
                }
                | Event::ControllerTouchpadMotion {
                    which,
                    touchpad,
                    finger,
                    x,
                    y,
                    ..
                } => (which, touchpad, finger, [x, y], true),
                Event::ControllerTouchpadUp {
                    which,
                    touchpad,
                    finger,
                    x,
                    y,
                    ..
                } => (which, touchpad, finger, [x, y], false),
                _ => continue,
            };
            if touchpad != 0 || finger != 0 {
                continue;
            }
            if let Some(pad) = self.pads.iter_mut().find(|pad| pad.instance.0 == which) {
                pad.touch = Some(position);
                pad.contact = contact;
            }
        }
    }

    fn refresh(&mut self, activate_new: bool) {
        if Instant::now() < self.next_refresh {
            return;
        }
        self.next_refresh = Instant::now() + Duration::from_millis(100);
        let Ok(ids) = self.subsystem.gamepads() else {
            return;
        };
        let mut live = ids.iter().map(|id| id.0).collect::<HashSet<_>>();
        live.extend(
            self.pads
                .iter()
                .filter(|pad| pad.gamepad.connected())
                .map(|pad| pad.instance.0),
        );
        self.registry.observe_connections(&live);
        let mut removed = Vec::new();
        self.pads.retain(|pad| {
            let connected = pad.gamepad.connected();
            if !connected && pad.active {
                removed.push(pad.device.clone());
            }
            connected
        });
        for device in removed {
            self.handle.disconnected(&device);
        }
        for id in ids {
            if self.pads.iter().any(|pad| pad.instance == id) {
                continue;
            }
            self.open(id, activate_new);
        }
    }

    fn open(&mut self, id: sdl3::joystick::JoystickId, active: bool) {
        let Ok(gamepad) = self.subsystem.open(id) else {
            return;
        };
        let name = gamepad
            .name()
            .unwrap_or_else(|| "Unknown gamepad".to_owned());
        let fingerprint = DeviceFingerprint {
            name: name.clone(),
            guid: self
                .subsystem
                .guid_for_id(id)
                .to_string()
                .to_ascii_lowercase(),
            vendor: self.subsystem.vendor_for_id(id),
            product: self.subsystem.product_for_id(id),
            serial: gamepad.serial_number(),
            path: gamepad.path(),
        };
        let identity = DeviceIdentity {
            source_id: self.registry.resolve(id.0, fingerprint.clone()),
            vendor: fingerprint.vendor,
            product: fingerprint.product,
            serial: fingerprint.serial,
        };
        let device = DeviceId::new(BackendId::SDL3, &identity.source_id);
        let touch = gamepad.touchpads_count() != 0;
        let capabilities = Capabilities::RUMBLE
            | Capabilities::CALIBRATE_DEAD_ZONE
            | if touch {
                Capabilities::TOUCHPAD
                    | Capabilities::POINTER_SURFACE
                    | Capabilities::POINTER_CONTACT
            } else {
                Capabilities::default()
            };
        let _ = gamepad.sensor_set_enabled(SensorType::Accelerometer, true);
        let _ = gamepad.sensor_set_enabled(SensorType::Gyroscope, true);
        let descriptor = DeviceDescriptor {
            id: device.clone(),
            name: Arc::from(name),
            profile: None,
            vendor_id: identity.vendor,
            product_id: identity.product,
            serial: identity
                .serial
                .as_deref()
                .and_then(DeviceDescriptor::serial_uuid),
            role: DeviceRole::Whole,
            capabilities,
        };
        if active {
            self.handle.connected(descriptor.clone());
        }
        self.pads.push(Pad {
            gamepad,
            instance: id,
            device,
            descriptor,
            active,
            awaiting_button_release: false,
            state: DeviceState::default(),
            touch: None,
            contact: false,
        });
    }

    fn drain_commands(&mut self) {
        while let Ok(command) = self.commands.try_recv() {
            match command {
                Command::Rumble {
                    device,
                    low,
                    high,
                    duration_ms,
                } => {
                    if let Some(pad) = self.pads.iter_mut().find(|pad| pad.device == device) {
                        let _ = pad.gamepad.set_rumble(low, high, duration_ms);
                    }
                }
            }
        }
    }

    fn publish(&mut self) {
        for pad in &mut self.pads {
            let mut state = Self::state(pad);
            if let Some(calibration) = self.calibrations.read().get(&pad.device) {
                calibration.correct(&mut state);
                calibration.shape(&mut state);
            }
            if !pad.active && state.gamepad.buttons != 0 {
                pad.active = true;
                pad.awaiting_button_release = true;
                pad.state = state;
                self.handle.connected(pad.descriptor.clone());
                continue;
            }
            if pad.awaiting_button_release {
                pad.state = state;
                if state.gamepad.buttons == 0 {
                    pad.awaiting_button_release = false;
                }
                continue;
            }
            if state != pad.state {
                pad.state = state;
                if pad.active {
                    self.handle.publish(&pad.device, &state);
                }
            }
            if !pad.active {
                continue;
            }
            let pointer =
                pad.touch
                    .map_or(PointerSample::None, |position| PointerSample::Surface {
                        position,
                        contact: pad.contact,
                    });
            self.handle.publish_pointer(&pad.device, &pointer);
        }
    }

    fn state(pad: &Pad) -> DeviceState {
        DeviceState {
            gamepad: GamepadReport {
                buttons: BUTTONS
                    .iter()
                    .enumerate()
                    .filter(|(_, button)| pad.gamepad.button(**button))
                    .fold(0, |mask, (index, _)| mask | (1 << index)),
                left_stick: [pad.gamepad.axis(Axis::LeftX), pad.gamepad.axis(Axis::LeftY)],
                right_stick: [
                    pad.gamepad.axis(Axis::RightX),
                    pad.gamepad.axis(Axis::RightY),
                ],
                left_trigger: trigger(pad.gamepad.axis(Axis::TriggerLeft)),
                right_trigger: trigger(pad.gamepad.axis(Axis::TriggerRight)),
                pressure: Default::default(),
            },
            sensor: Self::sensors(&pad.gamepad),
            pointer: pad
                .touch
                .map_or(PointerReport::default(), |position| PointerReport {
                    position: normalized(position),
                    delta: [0; 2],
                    flags: PointerFlags::DEVICE
                        | PointerFlags::NATIVE
                        | if pad.contact {
                            PointerFlags::CONTACT
                        } else {
                            PointerFlags::default()
                        },
                    kind: PointerKind::Touchpad,
                }),
            battery: BatteryState::default(),
        }
    }

    fn sensors(gamepad: &Gamepad) -> SensorReport {
        let mut sensor = SensorReport::default();
        let _ = gamepad.sensor_get_data(SensorType::Accelerometer, &mut sensor.accelerometer);
        let _ = gamepad.sensor_get_data(SensorType::Gyroscope, &mut sensor.gyroscope);
        sensor
    }
}

const BUTTONS: [Button; 21] = [
    Button::South,
    Button::East,
    Button::West,
    Button::North,
    Button::Back,
    Button::Guide,
    Button::Start,
    Button::LeftStick,
    Button::RightStick,
    Button::LeftShoulder,
    Button::RightShoulder,
    Button::DPadUp,
    Button::DPadDown,
    Button::DPadLeft,
    Button::DPadRight,
    Button::Misc1,
    Button::RightPaddle1,
    Button::LeftPaddle1,
    Button::RightPaddle2,
    Button::LeftPaddle2,
    Button::Touchpad,
];

fn trigger(value: i16) -> u16 {
    let value = u32::from(value.max(0) as u16);
    ((value * u32::from(u16::MAX)) / i16::MAX as u32) as u16
}

fn normalized(position: [f32; 2]) -> [i16; 2] {
    position.map(|value| {
        ((value.clamp(0.0, 1.0) * 65535.0).round() as i32 - 32768)
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
    })
}
