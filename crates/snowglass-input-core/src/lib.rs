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
