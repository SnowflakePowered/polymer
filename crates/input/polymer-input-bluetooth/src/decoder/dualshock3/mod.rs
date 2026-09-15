use super::{BluetoothControllerDevice, DecodedInput, GamepadState, set_button};
use polymer_input::{Calibration, Capabilities, DeviceRole, GamepadButton};
use std::time::Instant;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, byteorder::big_endian::U16};

#[derive(Default)]
pub struct DualShock3Decoder {
    calibration: Calibration,
    motion: Option<polymer_bluetooth::devices::sony::dualshock3::motion::MotionState>,
}

/// Native HID report, including its ID but excluding the transport's HIDP header.
/// Layout: DsHidMini include/DsHidMini/Ds3Types.h (DS3_RAW_INPUT_REPORT).
#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct NativeReport {
    id: u8,
    status: u8,
    buttons: [u8; 4],
    sticks: [u8; 4],
    _reserved1: [u8; 4],
    pressure: [u8; 12],
    _reserved2: [u8; 4],
    battery: u8,
    _reserved3: [u8; 10],
    accelerometer: [U16; 3],
    gyro: U16,
}

impl NativeReport {
    fn state(&self) -> GamepadState {
        use GamepadButton::*;
        let sticks = self.sticks.map(|value| {
            let centered = i32::from(value) - 128;
            (centered * 32767 / 127).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
        });
        let mut state = GamepadState {
            left_stick: [sticks[0], sticks[1]],
            right_stick: [sticks[2], sticks[3]],
            left_trigger: u16::from(self.pressure[4]) * 257,
            right_trigger: u16::from(self.pressure[5]) * 257,
            ..Default::default()
        };
        let buttons = u32::from_le_bytes(self.buttons);
        // Native pressure bytes may retain small residual values after release.
        // The digital contacts are authoritative, as in RPCS3's DS3 handler.
        if buttons & (1 << 8) == 0 {
            state.left_trigger = 0;
        }
        if buttons & (1 << 9) == 0 {
            state.right_trigger = 0;
        }
        for (bit, button) in [
            (0, Select),
            (1, LeftStick),
            (2, RightStick),
            (3, Start),
            (4, DpadUp),
            (5, DpadRight),
            (6, DpadDown),
            (7, DpadLeft),
            (10, L),
            (11, R),
            (12, Y),
            (13, B),
            (14, A),
            (15, X),
            (16, Guide),
        ] {
            set_button(&mut state, buttons & (1 << bit) != 0, button);
        }
        for (offset, button) in [
            (0, DpadUp),
            (1, DpadRight),
            (2, DpadDown),
            (3, DpadLeft),
            (6, L),
            (7, R),
            (8, Y),
            (9, B),
            (10, A),
            (11, X),
        ] {
            state.pressure.mask |= button.mask();
            state.pressure.values[button as usize] = if state.buttons & button.mask() != 0 {
                self.pressure[offset]
            } else {
                0
            };
        }
        // SDL's native PS3 convention: right/up/toward player, m/s²,
        // 113 counts/g with nominal ADC center 511. Do not apply the
        // byte swapping/X inversion used by Windows' synthetic SXS reports.
        let acceleration = self
            .accelerometer
            .map(|raw| (f32::from(raw.get()) - 511.0) * (9.80665 / 113.0));
        state.accelerometer = [acceleration[0], -acceleration[2], -acceleration[1]];
        // DS3 has yaw only: 123 counts per 90 degrees/s (RPCS3).
        // Polymer's sensor API uses radians/s; unavailable pitch/roll stay zero.
        state.gyroscope = [
            0.0,
            (f32::from(self.gyro.get()) - 512.0) * (std::f32::consts::FRAC_PI_2 / 123.0),
            0.0,
        ];
        state
    }
}

impl BluetoothControllerDevice for DualShock3Decoder {
    fn set_ds3_motion(
        &mut self,
        motion: Option<polymer_bluetooth::devices::sony::dualshock3::motion::MotionState>,
    ) {
        self.motion = motion;
    }
    fn decode(&mut self, report: &[u8], _received_at: Instant) -> Option<DecodedInput> {
        let report = NativeReport::ref_from_bytes(report).ok()?;
        if report.id != 1 || report.status == 0xff {
            return None;
        }
        // Coarse battery categories, not an exact percentage. Charging reports
        // omit the remaining charge; 0xef means charging has completed.
        let battery_level = match report.battery {
            0..=5 => Some([0, 1, 25, 50, 75, 100][usize::from(report.battery)]),
            0xef => Some(100),
            _ => None,
        };
        let mut state = report.state();
        if let Some(calibration) = self.motion.as_ref().and_then(|motion| motion.snapshot()) {
            let acceleration: [f32; 3] = std::array::from_fn(|index| {
                (f32::from(report.accelerometer[index].get())
                    - f32::from(calibration.acceleration_zero[index]))
                    * 9.80665
                    / f32::from(calibration.acceleration_span[index])
            });
            state.accelerometer = [acceleration[0], -acceleration[2], -acceleration[1]];
            // Keep the native SDL yaw convention. Sony's synthetic SXS report
            // flips this sign; the raw report has not undergone that transform.
            state.gyroscope[1] = if calibration.settling {
                0.0
            } else {
                (f32::from(report.gyro.get()) - calibration.gyro_zero)
                    * (std::f32::consts::FRAC_PI_2 / 123.0)
            };
        }
        Some(DecodedInput {
            state,
            role: DeviceRole::Whole,
            battery_level,
            charging: report.battery == 0xee,
            pointer_source: None,
            motion: None,
        })
    }

    fn calibration(&self) -> &Calibration {
        &self.calibration
    }
    fn calibration_mut(&mut self) -> &mut Calibration {
        &mut self.calibration
    }
    fn additional_capabilities(&self) -> Capabilities {
        Capabilities::ACCELEROMETER
            | Capabilities::GYROSCOPE
            | Capabilities::BUTTON_PRESSURE
            | Capabilities::CALIBRATE_TRIGGER
            | Capabilities::CALIBRATE_MOTION_BIAS
            | Capabilities::CALIBRATE_MOTION_SCALE
    }
}

const _: () = assert!(std::mem::size_of::<NativeReport>() == 49);
