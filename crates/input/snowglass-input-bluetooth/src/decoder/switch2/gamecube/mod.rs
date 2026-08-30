use super::{DecodedInput, FullReport, GamepadState, ReportDecoder, Switch2ReportModel};
use crate::decoder::switch2::motion::{NativeMotionDecoder, NativeMotionLayout};
use snowglass_input::DeviceRole;
use std::time::Instant;
use zerocopy::FromBytes;

#[derive(Default)]
pub struct NsoGameCubeDecoder {
    motion_decoder: NativeMotionDecoder,
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

impl ReportDecoder for NsoGameCubeDecoder {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::switch2::motion::GRAVITY_METERS_PER_SECOND_SQUARED;
    use std::time::Duration;

    const NORMAL_REPORT: [u8; 63] = [
        0xcf, 0x1c, 0x00, 0x00, 0x00, 0x8e, 0xb8, 0x7d, 0xfd, 0x57, 0x81, 0x38, 0x1d, 0x1c, 0x28,
        0x64, 0xd3, 0x00, 0x0e, 0x03, 0x28, 0x0f, 0x46, 0x34, 0xbd, 0xf5, 0x9e, 0xd7, 0x01, 0xb0,
        0xf9, 0xc8, 0x4f, 0x01, 0xc0, 0xff, 0xf8, 0xbf, 0x01, 0xd8, 0xfc, 0xf1, 0x83, 0x00, 0xc8,
        0xff, 0xf2, 0x3f, 0x07, 0xd0, 0xe6, 0x23, 0x3f, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00,
    ];

    #[test]
    fn captured_native_packet_decodes_motion_units_and_timing() {
        let received_at = Instant::now();
        let input = NsoGameCubeDecoder::default()
            .decode(&NORMAL_REPORT, received_at)
            .unwrap();
        let motion = input.motion.unwrap();
        let norm = motion
            .orientation
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();

        assert!((norm - 1.0).abs() < 0.000_01);
        assert_eq!(motion.elapsed_ticks, 13);
        assert!((motion.temperature_celsius - 25.5).abs() < f32::EPSILON);
        assert_eq!(
            received_at.duration_since(motion.accelerometer[0].sampled_at),
            Duration::from_micros(11_250)
        );
        assert_eq!(
            received_at.duration_since(motion.accelerometer[1].sampled_at),
            Duration::from_millis(5)
        );
        assert_eq!(
            received_at.duration_since(motion.orientation_sampled_at),
            Duration::from_millis(15)
        );
        assert!(input.state.accelerometer[2] > 9.0);
        assert!(motion.raw_samples_updated);
        assert!(motion.filtered_accelerometer.is_none());
    }

    #[test]
    fn controls_remain_available_without_a_normal_motion_block() {
        let mut report = [0_u8; 63];
        report[1] = 0x24;
        report[5..8].copy_from_slice(&[0, 8, 128]);
        report[8..11].copy_from_slice(&[0, 8, 128]);
        report[12] = 64;
        report[13] = 255;
        let input = NsoGameCubeDecoder::default()
            .decode(&report, Instant::now())
            .unwrap();

        assert_eq!(input.state.left_trigger, 16_448);
        assert_eq!(input.state.right_trigger, u16::MAX);
        assert!(input.motion.is_none());
    }

    #[test]
    fn captured_expanded_range_packet_restores_common_report_scale() {
        let report = [
            0xfc, 0x1c, 0x00, 0x00, 0x00, 0x58, 0xd8, 0x7e, 0xf8, 0x87, 0x81, 0x38, 0x20, 0x1e,
            0x28, 0xd9, 0xd0, 0x00, 0x0e, 0xa8, 0x71, 0x67, 0x86, 0x08, 0x3b, 0x73, 0x4a, 0x9c,
            0x2b, 0xa4, 0xea, 0x28, 0x7d, 0xca, 0xad, 0xf4, 0x2d, 0x7e, 0x2a, 0xc0, 0xf3, 0x82,
            0x3f, 0x02, 0xa7, 0xf5, 0x84, 0x7c, 0xa7, 0xf0, 0x8e, 0x9b, 0xfb, 0x40, 0x01, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let input = NsoGameCubeDecoder::default()
            .decode(&report, Instant::now())
            .unwrap();
        let expected_acceleration = [2676.0, -7236.0, -1128.0]
            .map(|value| value / 4096.0 * GRAVITY_METERS_PER_SECOND_SQUARED);
        let expected_gyroscope =
            [-16244.0, -2652.0, -3568.0].map(|value| value * std::f32::consts::PI / 180.0 / 16.4);

        for (actual, expected) in input
            .state
            .accelerometer
            .into_iter()
            .zip(expected_acceleration)
        {
            assert!((actual - expected).abs() < 0.000_01);
        }
        for (actual, expected) in input.state.gyroscope.into_iter().zip(expected_gyroscope) {
            assert!((actual - expected).abs() < 0.000_01);
        }
        assert_eq!(input.motion.unwrap().temperature_celsius, 27.5);
    }
}
