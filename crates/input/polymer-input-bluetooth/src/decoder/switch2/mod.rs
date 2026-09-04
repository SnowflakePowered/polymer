use super::{
    BluetoothControllerDevice, DecodedInput, GamepadState, PointerSource, normalize_stick,
    set_button,
};
use polymer_input::{Calibration, Capabilities, DeviceRole, GamepadButton};
use std::time::Instant;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

mod gamecube;
mod motion;

pub use gamecube::NsoGameCubeDecoder;
use motion::{NativeMotionDecoder, NativeMotionLayout};

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct ReportHeader {
    _report_id: u8,
    status: u8,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct HalfReport {
    _report_id: u8,
    _status: u8,
    buttons: [u8; 2],
    _reserved_0: u8,
    stick: [u8; 3],
    _reserved_1: u8,
    optical_dx: [u8; 2],
    optical_dy: [u8; 2],
    _reserved_2: u8,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct FullReport {
    _report_id: u8,
    _status: u8,
    buttons: [u8; 3],
    left_stick: [u8; 3],
    right_stick: [u8; 3],
    _reserved: u8,
    left_trigger: u8,
    right_trigger: u8,
}

trait Switch2ReportModel {
    const ROLE: DeviceRole;

    fn decode_controls(&mut self, report: &[u8], state: &mut GamepadState) -> Option<()>;

    fn pointer_source(
        &mut self,
        _report: &[u8],
        _state: &mut GamepadState,
    ) -> Option<PointerSource> {
        None
    }

    fn decode_switch2(&mut self, report: &[u8]) -> Option<DecodedInput> {
        let (header, _) = ReportHeader::ref_from_prefix(report).ok()?;
        let mut state = GamepadState::default();
        self.decode_controls(report, &mut state)?;
        let pointer_source = self.pointer_source(report, &mut state);
        Some(DecodedInput {
            state,
            role: Self::ROLE,
            battery_level: Some((header.status >> 2) & 0x0f),
            charging: header.status & 0x02 != 0,
            pointer_source,
            motion: None,
        })
    }

    fn decode_stick(data: &[u8; 3]) -> [i16; 2] {
        let x = u16::from(data[0]) | (u16::from(data[1] & 0x0f) << 8);
        let y = u16::from(data[1] >> 4) | (u16::from(data[2]) << 4);
        [normalize_stick(x), -normalize_stick(y)]
    }

    fn map_full_buttons(state: &mut GamepadState, buttons: u32) {
        for (mask, button) in [
            (1 << 1, GamepadButton::A),
            (1, GamepadButton::B),
            (1 << 3, GamepadButton::X),
            (1 << 2, GamepadButton::Y),
            (1 << 6, GamepadButton::Start),
            (1 << 14, GamepadButton::Select),
            (1 << 16, GamepadButton::Guide),
            (1 << 7, GamepadButton::RightStick),
            (1 << 15, GamepadButton::LeftStick),
            (1 << 4, GamepadButton::R),
            (1 << 12, GamepadButton::L),
            (1 << 11, GamepadButton::DpadUp),
            (1 << 8, GamepadButton::DpadDown),
            (1 << 10, GamepadButton::DpadLeft),
            (1 << 9, GamepadButton::DpadRight),
            (1 << 17, GamepadButton::Misc1),
        ] {
            set_button(state, buttons & mask != 0, button);
        }
        set_button(state, buttons & (1 << 5) != 0, GamepadButton::RightPaddle1);
        set_button(state, buttons & (1 << 13) != 0, GamepadButton::LeftPaddle1);
    }
}

#[derive(Default)]
pub struct JoyCon2LeftDecoder {
    motion_decoder: NativeMotionDecoder,
    calibration: Calibration,
}

impl Switch2ReportModel for JoyCon2LeftDecoder {
    const ROLE: DeviceRole = DeviceRole::LeftHalf;

    fn decode_controls(&mut self, report: &[u8], state: &mut GamepadState) -> Option<()> {
        let (report, _) = HalfReport::ref_from_prefix(report).ok()?;
        let buttons = u16::from_le_bytes(report.buttons);
        for (mask, button) in [
            (0x0080, GamepadButton::LeftStick),
            (0x0040, GamepadButton::Select),
            (0x0020, GamepadButton::LeftPaddle1),
            (0x0010, GamepadButton::L),
            (0x0008, GamepadButton::DpadUp),
            (0x0004, GamepadButton::DpadLeft),
            (0x0002, GamepadButton::DpadRight),
            (0x0001, GamepadButton::DpadDown),
            (0x8000, GamepadButton::LeftPaddle2),
            (0x4000, GamepadButton::RightPaddle2),
            (0x0100, GamepadButton::Misc1),
        ] {
            set_button(state, buttons & mask != 0, button);
        }
        state.left_stick = Self::decode_stick(&report.stick);
        Some(())
    }

    fn pointer_source(&mut self, report: &[u8], state: &mut GamepadState) -> Option<PointerSource> {
        self.optical_position(report, state)
    }
}

impl JoyCon2LeftDecoder {
    fn optical_position(
        &mut self,
        report: &[u8],
        state: &mut GamepadState,
    ) -> Option<PointerSource> {
        let (report, _) = HalfReport::ref_from_prefix(report).ok()?;
        let dx = i16::from_le_bytes(report.optical_dx);
        let dy = i16::from_le_bytes(report.optical_dy);
        state.pointer = [dx, dy];
        Some(PointerSource::Optical)
    }
}

impl BluetoothControllerDevice for JoyCon2LeftDecoder {
    fn decode(&mut self, report: &[u8], received_at: Instant) -> Option<DecodedInput> {
        let mut input = self.decode_switch2(report)?;
        self.motion_decoder.update_input(
            report,
            received_at,
            NativeMotionLayout::HALF_REPORT,
            &mut input,
        );
        Some(input)
    }

    fn calibration(&self) -> &Calibration {
        &self.calibration
    }

    fn calibration_mut(&mut self) -> &mut Calibration {
        &mut self.calibration
    }

    fn role(&self) -> DeviceRole {
        <Self as Switch2ReportModel>::ROLE
    }

    fn additional_capabilities(&self) -> Capabilities {
        Capabilities::POINTER_RELATIVE
    }
}

#[derive(Default)]
pub struct JoyCon2RightDecoder {
    motion_decoder: NativeMotionDecoder,
    calibration: Calibration,
}

impl Switch2ReportModel for JoyCon2RightDecoder {
    const ROLE: DeviceRole = DeviceRole::RightHalf;

    fn decode_controls(&mut self, report: &[u8], state: &mut GamepadState) -> Option<()> {
        let (report, _) = HalfReport::ref_from_prefix(report).ok()?;
        let buttons = u16::from_le_bytes(report.buttons);
        for (mask, button) in [
            (0x0080, GamepadButton::RightStick),
            (0x0040, GamepadButton::Start),
            (0x0020, GamepadButton::RightPaddle1),
            (0x0010, GamepadButton::R),
            (0x0008, GamepadButton::X),
            (0x0004, GamepadButton::Y),
            (0x0002, GamepadButton::A),
            (0x0001, GamepadButton::B),
            (0x8000, GamepadButton::RightPaddle2),
            (0x4000, GamepadButton::LeftPaddle2),
            (0x1000, GamepadButton::Misc1),
            (0x0100, GamepadButton::Guide),
        ] {
            set_button(state, buttons & mask != 0, button);
        }
        state.right_stick = Self::decode_stick(&report.stick);
        Some(())
    }

    fn pointer_source(&mut self, report: &[u8], state: &mut GamepadState) -> Option<PointerSource> {
        let (report, _) = HalfReport::ref_from_prefix(report).ok()?;
        let dx = i16::from_le_bytes(report.optical_dx);
        let dy = i16::from_le_bytes(report.optical_dy);
        state.pointer = [dx, dy];
        Some(PointerSource::Optical)
    }
}

impl BluetoothControllerDevice for JoyCon2RightDecoder {
    fn decode(&mut self, report: &[u8], received_at: Instant) -> Option<DecodedInput> {
        let mut input = self.decode_switch2(report)?;
        self.motion_decoder.update_input(
            report,
            received_at,
            NativeMotionLayout::HALF_REPORT,
            &mut input,
        );
        Some(input)
    }

    fn calibration(&self) -> &Calibration {
        &self.calibration
    }

    fn calibration_mut(&mut self) -> &mut Calibration {
        &mut self.calibration
    }

    fn role(&self) -> DeviceRole {
        <Self as Switch2ReportModel>::ROLE
    }

    fn additional_capabilities(&self) -> Capabilities {
        Capabilities::POINTER_RELATIVE
    }
}

#[derive(Default)]
pub struct Switch2ProDecoder {
    motion_decoder: NativeMotionDecoder,
    calibration: Calibration,
}

impl Switch2ReportModel for Switch2ProDecoder {
    const ROLE: DeviceRole = DeviceRole::Whole;

    fn decode_controls(&mut self, report: &[u8], state: &mut GamepadState) -> Option<()> {
        let (report, _) = FullReport::ref_from_prefix(report).ok()?;
        let buttons =
            u32::from_le_bytes([report.buttons[0], report.buttons[1], report.buttons[2], 0]);
        Self::map_full_buttons(state, buttons);
        state.left_stick = Self::decode_stick(&report.left_stick);
        state.right_stick = Self::decode_stick(&report.right_stick);
        Some(())
    }
}

impl BluetoothControllerDevice for Switch2ProDecoder {
    fn decode(&mut self, report: &[u8], received_at: Instant) -> Option<DecodedInput> {
        let mut input = self.decode_switch2(report)?;
        self.motion_decoder.update_input(
            report,
            received_at,
            NativeMotionLayout::FULL_REPORT,
            &mut input,
        );
        Some(input)
    }

    fn calibration(&self) -> &Calibration {
        &self.calibration
    }

    fn calibration_mut(&mut self) -> &mut Calibration {
        &mut self.calibration
    }

    fn role(&self) -> DeviceRole {
        <Self as Switch2ReportModel>::ROLE
    }
}
