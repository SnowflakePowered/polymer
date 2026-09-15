use super::{
    BluetoothControllerDevice, DecodedInput, FullReport, GamepadState, Switch2ReportModel,
};
use crate::decoder::switch2::motion::{NativeMotionDecoder, NativeMotionLayout};
use polymer_input::{Calibration, DeviceRole};
use std::time::Instant;
use zerocopy::FromBytes;

#[derive(Default)]
pub struct NsoGameCubeDecoder {
    motion_decoder: NativeMotionDecoder,
    calibration: Calibration,
}

impl Switch2ReportModel for NsoGameCubeDecoder {
    const ROLE: DeviceRole = DeviceRole::Whole;

    fn decode_controls(&mut self, report: &[u8], state: &mut GamepadState) -> Option<()> {
        let (report, _) = FullReport::ref_from_prefix(report).ok()?;
        let buttons =
            u32::from_le_bytes([report.buttons[0], report.buttons[1], report.buttons[2], 0]);
        Self::map_full_buttons(state, buttons);
        state.left_stick = Self::decode_stick(&report.left_stick);
        state.right_stick = Self::decode_stick(&report.right_stick);
        state.left_trigger = u16::from(report.left_trigger) * 257;
        state.right_trigger = u16::from(report.right_trigger) * 257;
        Some(())
    }
}

impl BluetoothControllerDevice for NsoGameCubeDecoder {
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
