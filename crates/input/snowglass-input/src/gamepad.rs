//! API-neutral virtual gamepad topology and forwarded state.

/// Maximum number of stable player slots exposed by an input provider.
pub const MAX_GAMEPADS: usize = 4;
/// SDL's standard gamepad layout has six axes.
pub const GAMEPAD_AXIS_COUNT: usize = 6;
/// SDL2 and SDL3 both define 21 buttons in their standard gamepad layout.
pub const GAMEPAD_BUTTON_COUNT: usize = 21;

/// A stable Snowglass player slot. This is deliberately not an SDL device
/// index or instance ID; providers map it to their own API-specific identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct GamepadSlot(u8);

impl GamepadSlot {
    pub const fn new(value: u8) -> Option<Self> {
        if (value as usize) < MAX_GAMEPADS {
            Some(Self(value))
        } else {
            None
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Positional button numbering shared by SDL2's controller API and SDL3's
/// gamepad API. The face names avoid assuming a particular label set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum GamepadButton {
    South = 0,
    East = 1,
    West = 2,
    North = 3,
    Back = 4,
    Guide = 5,
    Start = 6,
    LeftStick = 7,
    RightStick = 8,
    LeftShoulder = 9,
    RightShoulder = 10,
    DpadUp = 11,
    DpadDown = 12,
    DpadLeft = 13,
    DpadRight = 14,
    Misc1 = 15,
    RightPaddle1 = 16,
    LeftPaddle1 = 17,
    RightPaddle2 = 18,
    LeftPaddle2 = 19,
    Touchpad = 20,
}

impl GamepadButton {
    pub const fn mask(self) -> u32 {
        1 << self as u8
    }
}

/// Complete current state for one virtual gamepad.
///
/// Sticks use SDL's signed 16-bit range. Triggers use an unsigned range so
/// zero has an unambiguous meaning on the wire; SDL providers translate it to
/// SDL's signed trigger axis (`-32768..=32767`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GamepadState {
    pub buttons: u32,
    pub left_stick: [i16; 2],
    pub right_stick: [i16; 2],
    pub left_trigger: u16,
    pub right_trigger: u16,
}

impl GamepadState {
    pub const fn button(self, button: GamepadButton) -> bool {
        self.buttons & button.mask() != 0
    }

    /// Axes in SDL's standard gamepad order: LX, LY, RX, RY, LT, RT.
    pub const fn sdl_axes(self) -> [i16; GAMEPAD_AXIS_COUNT] {
        [
            self.left_stick[0],
            self.left_stick[1],
            self.right_stick[0],
            self.right_stick[1],
            trigger_to_sdl(self.left_trigger),
            trigger_to_sdl(self.right_trigger),
        ]
    }
}

/// A sequenced snapshot forwarded from the viewer to an injected provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GamepadUpdate {
    pub slot: GamepadSlot,
    pub sequence: u32,
    pub connected: bool,
    pub state: GamepadState,
}

const fn trigger_to_sdl(value: u16) -> i16 {
    (value as i32 - 32_768) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_bounded() {
        assert_eq!(GamepadSlot::new(0).map(GamepadSlot::index), Some(0));
        assert_eq!(GamepadSlot::new(3).map(GamepadSlot::index), Some(3));
        assert_eq!(GamepadSlot::new(4), None);
    }

    #[test]
    fn trigger_range_maps_to_sdl_axis_range() {
        assert_eq!(trigger_to_sdl(0), i16::MIN);
        assert_eq!(trigger_to_sdl(32_768), 0);
        assert_eq!(trigger_to_sdl(u16::MAX), i16::MAX);
    }
}
