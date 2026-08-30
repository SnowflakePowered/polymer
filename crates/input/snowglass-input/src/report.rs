use crate::BatteryState;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum GamepadButton {
    A,
    B,
    X,
    Y,
    Select,
    Guide,
    Start,
    LeftStick,
    RightStick,
    L,
    R,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    Misc1,
    RightPaddle1,
    LeftPaddle1,
    RightPaddle2,
    LeftPaddle2,
    Touchpad,
}

impl GamepadButton {
    pub const ALL: [Self; 21] = [
        Self::A,
        Self::B,
        Self::X,
        Self::Y,
        Self::Select,
        Self::Guide,
        Self::Start,
        Self::LeftStick,
        Self::RightStick,
        Self::L,
        Self::R,
        Self::DpadUp,
        Self::DpadDown,
        Self::DpadLeft,
        Self::DpadRight,
        Self::Misc1,
        Self::RightPaddle1,
        Self::LeftPaddle1,
        Self::RightPaddle2,
        Self::LeftPaddle2,
        Self::Touchpad,
    ];

    pub const fn mask(self) -> u32 {
        1 << self as u8
    }

    pub fn from_wire(index: u8) -> Option<Self> {
        Self::ALL.get(index as usize).copied()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct GamepadReport {
    pub buttons: u32,
    pub left_stick: [i16; 2],
    pub right_stick: [i16; 2],
    pub left_trigger: u16,
    pub right_trigger: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct SensorReport {
    pub accelerometer: [f32; 3],
    pub gyroscope: [f32; 3],
    pub orientation: [f32; 4],
    pub has_orientation: bool,
    pub sampled_at_ns: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(u8)]
pub enum PointerSample {
    #[default]
    None,
    Relative {
        delta: [f32; 2],
        counts_per_unit: f32,
    },
    Angular {
        position: [f32; 2],
        field_of_view: [f32; 2],
        confidence: f32,
    },
    Surface {
        position: [f32; 2],
        contact: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct PointerFlags(u8);

impl PointerFlags {
    pub const PROJECTED: Self = Self(1 << 0);
    pub const DEVICE: Self = Self(1 << 1);
    pub const CONTACT: Self = Self(1 << 2);
    pub const NATIVE: Self = Self(1 << 3);
    pub const HOST: Self = Self(1 << 4);

    pub const fn from_bits_retain(bits: u8) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl std::ops::BitOr for PointerFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum PointerKind {
    #[default]
    None,
    Mouse,
    IrCamera,
    AirMouse,
    Optical,
    Touchpad,
}

impl PointerKind {
    pub const fn from_wire(value: u8) -> Self {
        match value {
            1 => Self::Mouse,
            2 => Self::IrCamera,
            3 => Self::AirMouse,
            4 => Self::Optical,
            5 => Self::Touchpad,
            _ => Self::None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct PointerReport {
    pub position: [i16; 2],
    pub delta: [i16; 2],
    pub flags: PointerFlags,
    pub kind: PointerKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct DeviceState {
    pub gamepad: GamepadReport,
    pub sensor: SensorReport,
    pub pointer: PointerReport,
    pub battery: BatteryState,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RawControls {
    pub buttons: u32,
    pub axes: [i16; 8],
}

impl From<DeviceState> for RawControls {
    fn from(state: DeviceState) -> Self {
        Self {
            buttons: state.gamepad.buttons,
            axes: [
                state.gamepad.left_stick[0],
                state.gamepad.left_stick[1],
                state.gamepad.right_stick[0],
                state.gamepad.right_stick[1],
                state.gamepad.left_trigger as i16,
                state.gamepad.right_trigger as i16,
                state.pointer.position[0],
                state.pointer.position[1],
            ],
        }
    }
}

const _: () = assert!(std::mem::size_of::<GamepadButton>() == 1);
const _: () = assert!(std::mem::size_of::<DeviceState>() <= 96);
