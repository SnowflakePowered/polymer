use crate::{
    Calibration, CalibrationCapture, CalibrationKind, DeviceDescriptor, DeviceId, DeviceState,
    Feedback, HdRumbleFormat, HdRumbleStream, HubError, InputHub, PlayerIndex, RawControls,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ListenerId(pub(crate) u64);

pub enum InputEvent<'a> {
    Connected {
        index: Option<PlayerIndex>,
        device: &'a DeviceDescriptor,
    },
    Disconnected {
        index: Option<PlayerIndex>,
        device: &'a DeviceId,
    },
    Assigned {
        device: &'a DeviceId,
        from: Option<PlayerIndex>,
        to: Option<PlayerIndex>,
    },
    State {
        index: PlayerIndex,
        sequence: u32,
        device: &'a DeviceId,
        state: &'a DeviceState,
    },
    RawHid {
        index: PlayerIndex,
        device: &'a DeviceId,
        report: &'a [u8],
    },
}

pub trait InputListener: Send + Sync + 'static {
    fn on_input(&self, event: InputEvent<'_>);

    fn wants_raw_hid(&self, _device: &DeviceDescriptor) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct KeyCode(&'static str);

impl KeyCode {
    pub fn new(value: &str) -> Self {
        Self(Self::known(value).unwrap_or("Unidentified"))
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }

    pub fn wire(self) -> u16 {
        Self::CODES
            .iter()
            .position(|code| *code == self.0)
            .unwrap_or(0) as u16
    }

    pub fn from_wire(value: u16) -> Self {
        Self(
            Self::CODES
                .get(usize::from(value))
                .copied()
                .unwrap_or("Unidentified"),
        )
    }

    fn known(value: &str) -> Option<&'static str> {
        Self::CODES.iter().copied().find(|code| *code == value)
    }

    const CODES: [&'static str; 28] = [
        "Unidentified",
        "KeyA",
        "KeyC",
        "KeyD",
        "KeyE",
        "KeyF",
        "KeyH",
        "KeyI",
        "KeyJ",
        "KeyK",
        "KeyL",
        "KeyQ",
        "KeyS",
        "KeyV",
        "KeyW",
        "KeyX",
        "KeyZ",
        "Digit1",
        "Digit3",
        "Enter",
        "Backspace",
        "Home",
        "ArrowUp",
        "ArrowRight",
        "ArrowDown",
        "ArrowLeft",
        "ShiftLeft",
        "ControlLeft",
    ];
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostPointerInput {
    pub surface_id: u64,
    pub target_window: i64,
    pub source: u8,
    pub phase: u8,
    pub pointer_id: u32,
    pub x: i32,
    pub y: i32,
    pub coordinate_width: u32,
    pub coordinate_height: u32,
    pub button: u32,
    pub position: [f64; 2],
    pub delta: [f64; 2],
    pub buttons: u32,
    pub wheel_horizontal: i32,
    pub wheel_vertical: i32,
    pub timestamp_ms: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HostInputEvent {
    Key {
        code: KeyCode,
        scan_code: u16,
        virtual_key: u16,
        pressed: bool,
        repeat: bool,
        extended: bool,
        target_window: i64,
        timestamp_ms: u32,
        sequence: u32,
    },
    Pointer(HostPointerInput),
    MouseButton {
        button: u8,
        pressed: bool,
        timestamp_ms: u32,
    },
    MouseMotion {
        dx: f64,
        dy: f64,
    },
    Wheel {
        horizontal: i32,
        vertical: i32,
    },
    FocusLost,
}

pub trait HostInputListener: Send + Sync + 'static {
    fn on_host_input(&self, event: &HostInputEvent);
}

pub struct ListenerHandle {
    pub(crate) hub: InputHub,
    pub(crate) id: ListenerId,
    pub(crate) attached: bool,
}

impl ListenerHandle {
    pub fn id(&self) -> ListenerId {
        self.id
    }

    pub fn devices(&self) -> Vec<(Option<PlayerIndex>, DeviceDescriptor)> {
        self.hub.devices()
    }

    pub fn hub(&self) -> &InputHub {
        &self.hub
    }

    pub fn state(&self, index: PlayerIndex) -> Option<DeviceState> {
        self.hub.state(index)
    }

    pub fn raw_controls(&self, index: PlayerIndex) -> Option<RawControls> {
        self.state(index).map(Into::into)
    }

    pub fn feedback(&self, index: PlayerIndex, feedback: Feedback) -> Result<(), HubError> {
        self.hub.feedback(self.id, index, feedback)
    }

    pub fn open_hd_rumble(
        &self,
        index: PlayerIndex,
        format: HdRumbleFormat,
    ) -> Result<HdRumbleStream, HubError> {
        self.hub.open_hd_rumble(self.id, index, format)
    }

    pub fn send_raw_hid(&self, index: PlayerIndex, report: &[u8]) -> Result<(), HubError> {
        self.hub.send_raw_hid(index, report)
    }

    pub fn set_raw_hid_capture(&self, index: PlayerIndex, enabled: bool) -> Result<(), HubError> {
        self.hub.set_raw_hid_capture(index, enabled)
    }

    pub fn calibration(&self, device: &DeviceId) -> Calibration {
        self.hub.calibration(device)
    }

    pub fn calibrate(&self, device: &DeviceId, calibration: &Calibration) -> Result<(), HubError> {
        self.hub.calibrate(device, calibration)
    }

    pub fn reset_calibration(&self, device: &DeviceId) -> Result<(), HubError> {
        self.hub.reset_calibration(device)
    }

    pub fn begin_calibration(
        &self,
        device: &DeviceId,
        kind: CalibrationKind,
    ) -> Result<CalibrationCapture, HubError> {
        self.hub.begin_calibration(device, kind)
    }

    pub fn detach(mut self) {
        self.hub.remove_listener(self.id);
        self.attached = false;
    }
}

impl Drop for ListenerHandle {
    fn drop(&mut self) {
        if self.attached {
            self.hub.remove_listener(self.id);
        }
    }
}

pub struct HostListenerHandle {
    pub(crate) hub: InputHub,
    pub(crate) id: ListenerId,
    pub(crate) attached: bool,
}

impl HostListenerHandle {
    pub fn detach(mut self) {
        self.hub.remove_host_listener(self.id);
        self.attached = false;
    }
}

impl Drop for HostListenerHandle {
    fn drop(&mut self) {
        if self.attached {
            self.hub.remove_host_listener(self.id);
        }
    }
}

pub(crate) type Listener = Arc<dyn InputListener>;
pub(crate) type HostListener = Arc<dyn HostInputListener>;
