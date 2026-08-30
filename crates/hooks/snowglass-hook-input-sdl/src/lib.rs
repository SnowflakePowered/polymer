use parking_lot::{Mutex, RwLock};
use snowglass_hook_input::HookInput;
use snowglass_input::{Feedback, GamepadReport};
use snowglass_input_layout::{
    layout::{InputLayout, SdlOutputLayout},
    projection::Projection,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};

pub const GAMEPAD_AXIS_COUNT: usize = 8;
pub const GAMEPAD_BUTTON_COUNT: usize = 21;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GamepadSlot(u8);

impl GamepadSlot {
    pub const fn new(value: u8) -> Option<Self> {
        if value < 8 { Some(Self(value)) } else { None }
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GamepadState {
    pub buttons: u32,
    pub left_stick: [i16; 2],
    pub right_stick: [i16; 2],
    pub left_trigger: u16,
    pub right_trigger: u16,
    pub pointer: [i16; 2],
    pub accelerometer: [f32; 3],
    pub gyroscope: [f32; 3],
}

impl GamepadState {
    pub const fn sdl_axes(self) -> [i16; GAMEPAD_AXIS_COUNT] {
        [
            self.left_stick[0],
            self.left_stick[1],
            self.right_stick[0],
            self.right_stick[1],
            (self.left_trigger as i32 - 32768) as i16,
            (self.right_trigger as i32 - 32768) as i16,
            self.pointer[0],
            self.pointer[1],
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GamepadFrame {
    pub slot: GamepadSlot,
    pub sequence: u32,
    pub connected: bool,
    pub state: GamepadState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionState {
    pub slot: GamepadSlot,
    pub connected: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GamepadFeedback {
    pub slot: GamepadSlot,
    pub low_frequency: u16,
    pub high_frequency: u16,
    pub duration_ms: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackendError(pub &'static str);

pub trait SdlBackend {
    fn rebind(&mut self, layouts: &[SdlOutputLayout]) -> Result<(), BackendError>;
    fn ensure_attached(&mut self, count: usize) -> Result<(), BackendError>;
    fn set_connected(&mut self, connection: ConnectionState) -> Result<(), BackendError>;
    fn publish(&mut self, frame: GamepadFrame) -> Result<(), BackendError>;
}

pub struct Runtime {
    receiver_started: AtomicBool,
    input: RwLock<Option<Arc<HookInput>>>,
    revision: Mutex<Option<u32>>,
    published: Mutex<Vec<Option<GamepadState>>>,
    connected: Mutex<Vec<Option<bool>>>,
    sequence: AtomicU32,
}

impl Runtime {
    pub fn new() -> Self {
        Self {
            receiver_started: AtomicBool::new(false),
            input: RwLock::new(None),
            revision: Mutex::new(None),
            published: Mutex::new(Vec::new()),
            connected: Mutex::new(Vec::new()),
            sequence: AtomicU32::new(0),
        }
    }

    pub fn start_receiver(self: &Arc<Self>, socket_name: String) {
        if self.receiver_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let session = socket_name.strip_suffix("-input").unwrap_or(&socket_name);
        *self.input.write() = Some(HookInput::start(session));
    }

    pub fn prepare_enumeration(&self, backend: &mut impl SdlBackend) -> Result<(), BackendError> {
        let count = self.synchronize_topology(backend)?;
        backend.ensure_attached(count)?;
        let input = self.input.read().clone();
        for index in 0..count {
            backend.set_connected(ConnectionState {
                slot: GamepadSlot(index as u8),
                connected: input
                    .as_ref()
                    .is_some_and(|input| input.raw(index as u8).is_some()),
            })?;
        }
        Ok(())
    }

    pub fn send_feedback(&self, feedback: GamepadFeedback) -> bool {
        self.input.read().as_ref().is_some_and(|input| {
            input.send_feedback(
                feedback.slot.get(),
                Feedback::Rumble {
                    low_frequency: feedback.low_frequency,
                    high_frequency: feedback.high_frequency,
                    duration_ms: feedback.duration_ms,
                },
            )
        })
    }

    pub fn topology_pad_count(&self) -> usize {
        self.input
            .read()
            .as_ref()
            .map_or(0, |input| input.topology().pad_count())
    }

    pub fn flush(&self, backend: &mut impl SdlBackend) -> bool {
        let Ok(count) = self.synchronize_topology(backend) else {
            return false;
        };
        if backend.ensure_attached(count).is_err() {
            return false;
        }
        let Some(input) = self.input.read().clone() else {
            return false;
        };
        let mut published = self.published.lock();
        let mut connections = self.connected.lock();
        let mut forwarded = false;
        for index in 0..count {
            let raw = input.raw(index as u8);
            let connected = raw.is_some();
            if connections[index] != Some(connected)
                && backend
                    .set_connected(ConnectionState {
                        slot: GamepadSlot(index as u8),
                        connected,
                    })
                    .is_ok()
            {
                connections[index] = Some(connected);
                forwarded = true;
            }
            let state = raw
                .map(|raw| Self::state(input.mapped(index as u8).unwrap_or(raw.gamepad), raw))
                .unwrap_or_default();
            if published[index] == Some(state) {
                continue;
            }
            let frame = GamepadFrame {
                slot: GamepadSlot(index as u8),
                sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
                connected,
                state,
            };
            if backend.publish(frame).is_ok() {
                published[index] = Some(state);
                forwarded = true;
            }
        }
        forwarded
    }

    fn synchronize_topology(&self, backend: &mut impl SdlBackend) -> Result<usize, BackendError> {
        let Some(input) = self.input.read().clone() else {
            return Ok(0);
        };
        let topology = input.topology();
        let revision = topology.revision();
        if *self.revision.lock() == Some(revision) {
            return Ok(topology.pad_count());
        }
        let projection =
            Projection::embedded().map_err(|_| BackendError("SDL projection is invalid"))?;
        let layouts = topology
            .layouts()
            .into_iter()
            .map(|id| {
                InputLayout::get(id)
                    .map(|layout| layout.sdl_output(&projection))
                    .ok_or(BackendError("input topology contains an unknown layout"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        drop(topology);
        backend.rebind(&layouts)?;
        *self.published.lock() = vec![None; layouts.len()];
        *self.connected.lock() = vec![None; layouts.len()];
        *self.revision.lock() = Some(revision);
        Ok(layouts.len())
    }

    fn state(gamepad: GamepadReport, raw: snowglass_input::DeviceState) -> GamepadState {
        GamepadState {
            buttons: gamepad.buttons,
            left_stick: gamepad.left_stick,
            right_stick: gamepad.right_stick,
            left_trigger: gamepad.left_trigger,
            right_trigger: gamepad.right_trigger,
            pointer: raw.pointer.position,
            accelerometer: raw.sensor.accelerometer,
            gyroscope: raw.sensor.gyroscope,
        }
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}
