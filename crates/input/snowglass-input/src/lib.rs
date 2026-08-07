//! Normalized input representation shared by capture and forwarding backends.

#![forbid(unsafe_code)]

pub mod gamepad;
pub mod hub;
pub mod layout;
pub mod mouse;
pub mod projection;
pub mod provider;

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

/// One physical keyboard transition to post from inside the target process.
///
/// `target_window` is a Win32 `HWND`. The target-side Win32 input runtime
/// validates that it belongs to the current process before posting the
/// corresponding `WM_KEYDOWN` or `WM_KEYUP` message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawKeyInput {
    pub target_window: i64,
    pub virtual_key: u32,
    pub scan_code: u16,
    pub extended: bool,
    pub pressed: bool,
    pub repeat: bool,
    pub timestamp: u32,
    pub sequence: u32,
}

impl RawKeyInput {
    /// Converts this transition to the equivalent DirectInput keyboard record.
    ///
    /// Callers choose the delivery backend by sending either the original raw
    /// key event or this record over input IPC.
    pub fn direct_input_data(self) -> DirectInputData {
        DirectInputData {
            device: DirectInputDevice::Keyboard,
            offset: u32::from(self.scan_code) | u32::from(self.extended) * 0x80,
            data: if self.pressed { 0x80 } else { 0 },
            timestamp: self.timestamp,
            sequence: self.sequence,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_key_can_be_projected_to_direct_input() {
        let key = RawKeyInput {
            target_window: 123,
            virtual_key: 0x70,
            scan_code: 0x3b,
            extended: true,
            pressed: true,
            repeat: false,
            timestamp: 45,
            sequence: 67,
        };
        assert_eq!(
            key.direct_input_data(),
            DirectInputData {
                device: DirectInputDevice::Keyboard,
                offset: 0xbb,
                data: 0x80,
                timestamp: 45,
                sequence: 67,
            }
        );
    }
}
