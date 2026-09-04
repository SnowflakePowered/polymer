use parking_lot::RwLock;
use polymer_input::{
    BatteryState, DeviceState, GamepadReport, PointerFlags, PointerKind, PointerReport,
    SensorReport,
};
use polymer_ipc::input::{InputMessage, WireDeviceState};

#[derive(Default)]
pub struct InputState {
    slots: [RwLock<Option<DeviceState>>; 8],
    sequences: [RwLock<u32>; 8],
}

impl InputState {
    pub fn receive(&self, event: InputMessage) {
        match event {
            InputMessage::DeviceConnected(value) if usize::from(value.index) < 8 => {
                *self.slots[usize::from(value.index)].write() = Some(DeviceState::default());
            }
            InputMessage::DeviceDisconnected { index } if usize::from(index) < 8 => {
                *self.slots[usize::from(index)].write() = None;
            }
            InputMessage::DeviceState(value) if usize::from(value.index) < 8 => self.update(value),
            _ => {}
        }
    }

    pub fn get(&self, player: u8) -> Option<DeviceState> {
        self.slots
            .get(usize::from(player))
            .and_then(|state| *state.read())
    }

    fn update(&self, value: WireDeviceState) {
        let index = usize::from(value.index);
        let mut sequence = self.sequences[index].write();
        if value.sequence.wrapping_sub(*sequence) >= 1 << 31 {
            return;
        }
        *sequence = value.sequence;
        *self.slots[index].write() = Some(DeviceState {
            gamepad: GamepadReport {
                buttons: value.buttons,
                left_stick: value.left_stick,
                right_stick: value.right_stick,
                left_trigger: value.left_trigger,
                right_trigger: value.right_trigger,
            },
            sensor: SensorReport {
                accelerometer: value.accelerometer,
                gyroscope: value.gyroscope,
                orientation: value.orientation,
                has_orientation: value.has_orientation,
                sampled_at_ns: value.sampled_at_ns,
            },
            pointer: PointerReport {
                position: value.pointer_position,
                delta: value.pointer_delta,
                flags: PointerFlags::from_bits_retain(value.pointer_flags),
                kind: PointerKind::from_wire(value.pointer_kind),
            },
            battery: BatteryState {
                level: value.battery_level,
                charging: value.charging,
            },
        });
    }
}
