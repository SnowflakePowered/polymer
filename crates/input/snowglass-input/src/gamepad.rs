//! API-neutral virtual gamepad topology and forwarded state.

/// SDL's standard gamepad layout has six axes. Snowglass appends two raw
/// joystick axes carrying an absolute pointer position.
pub const GAMEPAD_AXIS_COUNT: usize = 8;
/// SDL2 and SDL3 both define 21 buttons in their standard gamepad layout.
pub const GAMEPAD_BUTTON_COUNT: usize = 21;

/// A stable Snowglass player slot. This is deliberately not an SDL device
/// index or instance ID; providers map it to their own API-specific identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct GamepadSlot(u8);

impl GamepadSlot {
    pub const fn new(value: u8) -> Option<Self> {
        Some(Self(value))
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Stone-labelled controls carried on the wire. Output providers, rather than
/// producers, decide where these labels live in their native layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum GamepadButton {
    A = 0,
    B = 1,
    X = 2,
    Y = 3,
    Select = 4,
    Guide = 5,
    Start = 6,
    LeftStick = 7,
    RightStick = 8,
    L = 9,
    R = 10,
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

/// Complete Stone-semantic state for one virtual gamepad. Button bits use
/// [`GamepadButton`] labels; they are not physical or SDL positions.
///
/// Sticks use SDL's signed 16-bit range. Triggers use an unsigned range so
/// zero has an unambiguous meaning on the wire; SDL providers translate it to
/// SDL's signed trigger axis (`-32768..=32767`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GamepadState {
    pub buttons: u32,
    pub left_stick: [i16; 2],
    pub right_stick: [i16; 2],
    pub left_trigger: u16,
    pub right_trigger: u16,
    /// Absolute pointer coordinates in SDL's signed axis range. These are raw
    /// joystick axes 6 and 7 rather than standard SDL gamepad axes.
    pub pointer: [i16; 2],
    /// SDL sensor coordinates and units: accelerometer in m/s².
    pub accelerometer: [f32; 3],
    /// SDL sensor coordinates and units: gyroscope in radians/second.
    pub gyroscope: [f32; 3],
}

impl GamepadState {
    pub const fn button(self, button: GamepadButton) -> bool {
        self.buttons & button.mask() != 0
    }

    /// Axes in Snowglass joystick order: LX, LY, RX, RY, LT, RT, pointer X,
    /// pointer Y. The first six retain SDL's standard gamepad ordering.
    pub const fn sdl_axes(self) -> [i16; GAMEPAD_AXIS_COUNT] {
        [
            self.left_stick[0],
            self.left_stick[1],
            self.right_stick[0],
            self.right_stick[1],
            trigger_to_sdl(self.left_trigger),
            trigger_to_sdl(self.right_trigger),
            self.pointer[0],
            self.pointer[1],
        ]
    }
}

/// A sequenced snapshot forwarded from the viewer to an injected provider.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GamepadUpdate {
    pub slot: GamepadSlot,
    pub sequence: u32,
    pub connected: bool,
    pub state: GamepadState,
}

/// A coalescing, per-slot handoff. Publishing replaces an unread snapshot for
/// that slot, so input consumers can never work through stale gamepad states.
pub struct LatestGamepadMailbox {
    slots: Mutex<Vec<Option<StampedGamepadUpdate>>>,
    wake_tx: mpsc::SyncSender<()>,
    wake_rx: Mutex<mpsc::Receiver<()>>,
}

#[derive(Clone, Copy, Debug)]
pub struct StampedGamepadUpdate {
    pub update: GamepadUpdate,
    pub sampled_at: Instant,
}

impl LatestGamepadMailbox {
    pub fn new(slot_count: usize) -> Self {
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        Self {
            slots: Mutex::new(vec![None; slot_count.clamp(1, u8::MAX as usize + 1)]),
            wake_tx,
            wake_rx: Mutex::new(wake_rx),
        }
    }

    pub fn publish(&self, update: GamepadUpdate) {
        let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let Some(slot) = slots.get_mut(update.slot.index()) else {
            return;
        };
        *slot = Some(StampedGamepadUpdate {
            update,
            sampled_at: Instant::now(),
        });
        drop(slots);
        let _ = self.wake_tx.try_send(());
    }

    pub fn wait_and_drain(&self) -> Vec<StampedGamepadUpdate> {
        if self
            .wake_rx
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .recv()
            .is_err()
        {
            return Vec::new();
        }
        self.slots
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter_mut()
            .filter_map(Option::take)
            .collect()
    }
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
        assert_eq!(GamepadSlot::new(255).map(GamepadSlot::index), Some(255));
    }

    #[test]
    fn trigger_range_maps_to_sdl_axis_range() {
        assert_eq!(trigger_to_sdl(0), i16::MIN);
        assert_eq!(trigger_to_sdl(32_768), 0);
        assert_eq!(trigger_to_sdl(u16::MAX), i16::MAX);
    }

    #[test]
    fn pointer_uses_extra_raw_joystick_axes() {
        let state = GamepadState {
            pointer: [i16::MIN, i16::MAX],
            ..GamepadState::default()
        };
        assert_eq!(&state.sdl_axes()[6..], &[i16::MIN, i16::MAX]);
    }

    #[test]
    fn mailbox_coalesces_unread_snapshots_per_slot() {
        let mailbox = LatestGamepadMailbox::new(2);
        for sequence in [1, 2, 3] {
            mailbox.publish(GamepadUpdate {
                slot: GamepadSlot::new(0).unwrap(),
                sequence,
                connected: true,
                state: GamepadState {
                    buttons: sequence,
                    ..GamepadState::default()
                },
            });
        }
        mailbox.publish(GamepadUpdate {
            slot: GamepadSlot::new(1).unwrap(),
            sequence: 7,
            connected: true,
            state: GamepadState::default(),
        });
        let updates = mailbox.wait_and_drain();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].update.sequence, 3);
        assert_eq!(updates[1].update.sequence, 7);
    }
}
use std::{
    sync::{Mutex, mpsc},
    time::Instant,
};
