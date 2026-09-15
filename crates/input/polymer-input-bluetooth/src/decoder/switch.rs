use super::{BluetoothControllerDevice, DecodedInput, GamepadState, normalize_stick, set_button};
use polymer_input::{Calibration, DeviceRole, GamepadButton};
use std::time::Instant;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct NativeReport {
    report_id: u8,
    _timer: u8,
    status: u8,
    buttons: [u8; 3],
    left_stick: [u8; 3],
    right_stick: [u8; 3],
    _vibration: u8,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct FullNativeReport {
    report_id: u8,
    _timer: u8,
    _status: u8,
    _buttons: [u8; 3],
    _left_stick: [u8; 3],
    _right_stick: [u8; 3],
    _vibration_and_imu: [u8; 25],
    latest_imu: [u8; 12],
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct SimpleReport {
    report_id: u8,
    buttons: [u8; 2],
    _hat: u8,
}

trait SwitchReportModel {
    const HAS_LEFT_STICK: bool;
    const HAS_RIGHT_STICK: bool;
    const ROLE: DeviceRole;

    fn decode_report(report: &[u8]) -> Option<DecodedInput> {
        let mut state = GamepadState::default();
        let (buttons, battery_level, charging) = match report.first().copied()? {
            0x21 | 0x30 => {
                let (report, _) = NativeReport::ref_from_prefix(report).ok()?;
                (
                    u32::from_le_bytes([
                        report.buttons[0],
                        report.buttons[1],
                        report.buttons[2],
                        0,
                    ]),
                    Some(report.status >> 5),
                    report.status & 0x10 != 0,
                )
            }
            0x3f => {
                let (report, _) = SimpleReport::ref_from_prefix(report).ok()?;
                (u32::from(u16::from_le_bytes(report.buttons)), None, false)
            }
            _ => return None,
        };
        for (mask, button) in [
            (1 << 3, GamepadButton::A),
            (1 << 2, GamepadButton::B),
            (1 << 1, GamepadButton::X),
            (1, GamepadButton::Y),
            (1 << 6, GamepadButton::R),
            (1 << 7, GamepadButton::RightPaddle1),
            (1 << 22, GamepadButton::L),
            (1 << 23, GamepadButton::LeftPaddle1),
            (1 << 9, GamepadButton::Start),
            (1 << 8, GamepadButton::Select),
            (1 << 12, GamepadButton::Guide),
            (1 << 13, GamepadButton::Misc1),
            (1 << 19, GamepadButton::DpadLeft),
            (1 << 18, GamepadButton::DpadRight),
            (1 << 17, GamepadButton::DpadUp),
            (1 << 16, GamepadButton::DpadDown),
            (1 << 11, GamepadButton::LeftStick),
            (1 << 10, GamepadButton::RightStick),
        ] {
            set_button(&mut state, buttons & mask != 0, button);
        }
        if matches!(report[0], 0x21 | 0x30) {
            let full_report = FullNativeReport::ref_from_prefix(report)
                .ok()
                .map(|(report, _)| report);
            let (report, _) = NativeReport::ref_from_prefix(report).ok()?;
            if Self::HAS_LEFT_STICK {
                state.left_stick = Self::decode_stick(&report.left_stick);
            }
            if Self::HAS_RIGHT_STICK {
                state.right_stick = Self::decode_stick(&report.right_stick);
            }
            // 0x21 contains subcommand reply data in place of IMU samples.
            if let Some(full_report) = full_report.filter(|_| report.report_id == 0x30) {
                let data = &full_report.latest_imu;
                let raw = |offset| i16::from_le_bytes([data[offset], data[offset + 1]]);
                const GRAVITY: f32 = 9.80665;
                state.accelerometer = [
                    f32::from(raw(0)) / 4096.0 * GRAVITY,
                    f32::from(raw(2)) / 4096.0 * GRAVITY,
                    f32::from(raw(4)) / 4096.0 * GRAVITY,
                ];
                let gyro_scale = std::f32::consts::PI / 180.0 / 16.4;
                state.gyroscope = [
                    f32::from(raw(6)) * gyro_scale,
                    f32::from(raw(8)) * gyro_scale,
                    f32::from(raw(10)) * gyro_scale,
                ];
            }
        }
        Some(DecodedInput {
            state,
            role: Self::ROLE,
            battery_level,
            charging,
            pointer_source: None,
            motion: None,
        })
    }

    fn decode_stick(data: &[u8; 3]) -> [i16; 2] {
        let x = u16::from(data[0]) | (u16::from(data[1] & 0x0f) << 8);
        let y = u16::from(data[1] >> 4) | (u16::from(data[2]) << 4);
        [normalize_stick(x), normalize_stick(y).saturating_neg()]
    }
}

macro_rules! switch_decoder {
    ($decoder:ident, $left:literal, $right:literal, $role:expr) => {
        #[derive(Default)]
        pub struct $decoder {
            calibration: Calibration,
        }

        impl SwitchReportModel for $decoder {
            const HAS_LEFT_STICK: bool = $left;
            const HAS_RIGHT_STICK: bool = $right;
            const ROLE: DeviceRole = $role;
        }

        impl BluetoothControllerDevice for $decoder {
            fn decode(&mut self, report: &[u8], _received_at: Instant) -> Option<DecodedInput> {
                Self::decode_report(report)
            }

            fn calibration(&self) -> &Calibration {
                &self.calibration
            }

            fn calibration_mut(&mut self) -> &mut Calibration {
                &mut self.calibration
            }

            fn role(&self) -> DeviceRole {
                <Self as SwitchReportModel>::ROLE
            }
        }
    };
}

switch_decoder!(SwitchProDecoder, true, true, DeviceRole::Whole);
switch_decoder!(JoyConLeftDecoder, true, false, DeviceRole::LeftHalf);
switch_decoder!(JoyConRightDecoder, false, true, DeviceRole::RightHalf);
switch_decoder!(NsoNesDecoder, false, false, DeviceRole::Whole);
switch_decoder!(NsoSnesDecoder, false, false, DeviceRole::Whole);
switch_decoder!(NsoN64Decoder, true, false, DeviceRole::Whole);
switch_decoder!(NsoGenesisDecoder, false, false, DeviceRole::Whole);
