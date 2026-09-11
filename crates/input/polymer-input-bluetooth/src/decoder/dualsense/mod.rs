use super::{BluetoothControllerDevice, DecodedInput, GamepadState, PointerSource};
use polymer_bluetooth::devices::sony::dualsense::protocol::InputReport;
use polymer_input::{Calibration, Capabilities, DeviceRole, GamepadButton};
use std::time::Instant;

#[derive(Default)]
pub struct DualSenseDecoder {
    calibration: Calibration,
    sensors: Option<polymer_bluetooth::devices::SensorCalibration>,
    last_timestamp: Option<u32>,
    elapsed_ticks: u64,
    primary_touch: Option<u8>,
    last_pointer: [i16; 2],
}

mod sensors;

impl DualSenseDecoder {
    fn state(report: &InputReport) -> GamepadState {
        use GamepadButton::*;
        let sticks = report
            .sticks
            .map(|v| ((i32::from(v) - 128) * 32767 / 127).clamp(-32768, 32767) as i16);
        let mut state = GamepadState {
            left_stick: [sticks[0], sticks[1]],
            right_stick: [sticks[2], sticks[3]],
            left_trigger: u16::from(report.triggers[0]) * 257,
            right_trigger: u16::from(report.triggers[1]) * 257,
            ..Default::default()
        };
        let bits = u32::from_le_bytes(report.buttons);
        for (bit, button) in [
            (4, X),
            (5, A),
            (6, B),
            (7, Y),
            (8, L),
            (9, R),
            (12, Select),
            (13, Start),
            (14, LeftStick),
            (15, RightStick),
            (16, Guide),
            (17, Touchpad),
            (18, Misc1),
        ] {
            if bits & (1 << bit) != 0 {
                state.buttons |= button.mask();
            }
        }
        let hats = [
            DpadUp.mask(),
            DpadUp.mask() | DpadRight.mask(),
            DpadRight.mask(),
            DpadRight.mask() | DpadDown.mask(),
            DpadDown.mask(),
            DpadDown.mask() | DpadLeft.mask(),
            DpadLeft.mask(),
            DpadLeft.mask() | DpadUp.mask(),
        ];
        state.buttons |= hats.get((bits & 15) as usize).copied().unwrap_or(0);
        state
    }
}

impl BluetoothControllerDevice for DualSenseDecoder {
    fn set_sensor_calibration(
        &mut self,
        calibration: Option<polymer_bluetooth::devices::SensorCalibration>,
    ) {
        self.sensors = calibration;
    }
    fn additional_capabilities(&self) -> Capabilities {
        Capabilities::ACCELEROMETER
            | Capabilities::GYROSCOPE
            | Capabilities::TOUCHPAD
            | Capabilities::POINTER_SURFACE
            | Capabilities::POINTER_CONTACT
            | Capabilities::CALIBRATE_MOTION_BIAS
            | Capabilities::CALIBRATE_MOTION_SCALE
    }
    fn decode(&mut self, data: &[u8], _: Instant) -> Option<DecodedInput> {
        let report = InputReport::parse(data)?;
        let mut state = Self::state(report);
        self.read_sensors(report, &mut state);
        let charge = report.status[0] >> 4;
        let battery_level = match charge {
            0 | 1 => Some(((report.status[0] & 15) * 10 + 5).min(100)),
            2 => Some(100),
            _ => None,
        };
        Some(DecodedInput {
            state,
            role: DeviceRole::Whole,
            battery_level,
            charging: charge == 1,
            pointer_source: Some(PointerSource::Touchpad),
            motion: None,
        })
    }
    fn calibration(&self) -> &Calibration {
        &self.calibration
    }
    fn calibration_mut(&mut self) -> &mut Calibration {
        &mut self.calibration
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic protocol vector; checksum independently generated with Python zlib.
    const INPUT: &str = "31000080ff80ff80002131070000000000000000000000000000000000000000000000000000000000000000000000000000000000001700000000000000000000000000000000000000413c75dd";

    #[test]
    fn bluetooth_vector_decodes_controls_and_rejects_corruption() {
        let mut data: Vec<u8> = INPUT
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let mut decoder = DualSenseDecoder::default();
        let input = decoder.decode(&data, Instant::now()).unwrap();
        use GamepadButton::*;
        assert_eq!(
            input.state.buttons,
            A.mask()
                | L.mask()
                | Select.mask()
                | Start.mask()
                | Guide.mask()
                | Touchpad.mask()
                | Misc1.mask()
                | DpadUp.mask()
                | DpadRight.mask()
        );
        assert_eq!(input.state.left_stick, [i16::MIN, 0]);
        assert_eq!(input.state.right_stick, [i16::MAX, 0]);
        assert_eq!(input.state.left_trigger, u16::MAX);
        assert_eq!(input.state.right_trigger, 32896);
        assert_eq!(input.battery_level, Some(75));
        assert!(input.charging);
        assert!(decoder.decode(&data[..77], Instant::now()).is_none());
        data[9] ^= 1;
        assert!(decoder.decode(&data, Instant::now()).is_none());
        data[9] ^= 1;
        data[0] = 1;
        assert!(decoder.decode(&data, Instant::now()).is_none());
    }
}
