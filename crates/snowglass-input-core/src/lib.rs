//! Normalized input representation shared by capture and forwarding backends.

#![forbid(unsafe_code)]

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputEvent {
    Key { scan_code: u16, pressed: bool },
    MouseButton { button: u8, pressed: bool },
    MouseMotion { x: i32, y: i32, relative: bool },
    MouseWheel { horizontal: i16, vertical: i16 },
    ControllerState(ControllerState),
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ControllerState {
    pub buttons: u32,
    pub left_trigger: f32,
    pub right_trigger: f32,
    pub left_stick: [f32; 2],
    pub right_stick: [f32; 2],
}

/// DirectInput's buffered device classes. These correspond to the system
/// keyboard and mouse GUIDs rather than a particular target-side COM object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum DirectInputDevice {
    Keyboard = 1,
    Mouse = 2,
}

/// One buffered `DIDEVICEOBJECTDATA` record. `application_data` is intentionally
/// absent: it is assigned by the consuming application's action map and must be
/// reconstructed by the target-side DirectInput wrapper, not the capture side.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectInputData {
    pub device: DirectInputDevice,
    pub offset: u32,
    pub data: u32,
    pub timestamp: u32,
    pub sequence: u32,
}
