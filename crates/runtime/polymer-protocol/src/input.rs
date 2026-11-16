#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum PointerSource {
    Mouse = 1,
    Touch = 2,
}

impl TryFrom<u32> for PointerSource {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Mouse),
            2 => Ok(Self::Touch),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum PointerPhase {
    Move = 1,
    ButtonDown = 2,
    ButtonUp = 3,
    Wheel = 4,
    Cancel = 5,
}

impl TryFrom<u32> for PointerPhase {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Move),
            2 => Ok(Self::ButtonDown),
            3 => Ok(Self::ButtonUp),
            4 => Ok(Self::Wheel),
            5 => Ok(Self::Cancel),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PointerInput {
    pub surface_id: u64,
    pub target_window: i64,
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
    pub sequence: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum DirectInputDevice {
    Keyboard = 1,
    Mouse = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectInputData {
    pub device: DirectInputDevice,
    pub offset: u32,
    pub data: u32,
    pub timestamp: u32,
    pub sequence: u32,
}

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
    pub fn direct_input_data(self) -> DirectInputData {
        DirectInputData {
            device: DirectInputDevice::Keyboard,
            offset: u32::from(self.scan_code) | (u32::from(self.extended) * 0x80),
            data: if self.pressed { 0x80 } else { 0 },
            timestamp: self.timestamp,
            sequence: self.sequence,
        }
    }
}
