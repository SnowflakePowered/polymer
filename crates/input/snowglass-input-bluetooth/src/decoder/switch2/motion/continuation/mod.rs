use super::{NativeMotionHeader, PackedMotionBody};
use crate::decoder::{MotionFrame, TimedMotionVector};
use std::time::Instant;

const TIMING_CLASS: u8 = 0x0d;

pub(super) struct ContinuationMotion<'a> {
    pub header: &'a NativeMotionHeader,
    body: PackedMotionBody<'a>,
}

impl<'a> ContinuationMotion<'a> {
    pub fn parse(header: &'a NativeMotionHeader, body: &'a [u8]) -> Option<Self> {
        (PackedMotionBody::has_full_length(body) && header.timing_class() == TIMING_CLASS)
            .then_some(Self {
                header,
                body: PackedMotionBody::new(body),
            })
    }

    pub fn decode(
        &self,
        previous: MotionFrame,
        received_at: Instant,
        interval_ticks: u16,
        previous_terminal_at: Instant,
    ) -> MotionFrame {
        let mode = self.body.read_unsigned(0, 2);
        let accelerometer_range_shift = if mode & 1 == 0 { 2 } else { 0 };
        let gyroscope_range_shift = if mode & 2 == 0 { 2 } else { 0 };
        let middle_tick = interval_ticks.div_ceil(2);
        let accelerometer = [
            TimedMotionVector {
                value: previous.accelerometer[2].value,
                sampled_at: previous_terminal_at,
            },
            TimedMotionVector {
                value: PackedMotionBody::fractional_accelerometer(
                    self.fractional_vector(82, accelerometer_range_shift),
                ),
                sampled_at: self.header.sampled_at_in_span(
                    received_at,
                    middle_tick,
                    interval_ticks,
                ),
            },
            TimedMotionVector {
                value: PackedMotionBody::fractional_accelerometer(
                    self.fractional_vector(214, accelerometer_range_shift),
                ),
                sampled_at: received_at,
            },
        ];
        let gyroscope = [
            TimedMotionVector {
                value: previous.gyroscope[1].value,
                sampled_at: previous_terminal_at,
            },
            TimedMotionVector {
                value: PackedMotionBody::gyroscope(
                    self.fractional_vector(148, gyroscope_range_shift),
                ),
                sampled_at: received_at,
            },
        ];
        MotionFrame {
            orientation: self.orientation(),
            orientation_sampled_at: received_at,
            accelerometer,
            gyroscope,
            filtered_accelerometer: None,
            raw_samples_updated: true,
            temperature_celsius: self.body.read_signed(272, 16) as f32 / 128.0 + 25.0,
            sensor_timestamp: self.header.sensor_timestamp(),
            elapsed_ticks: interval_ticks,
        }
    }

    fn orientation(&self) -> [f32; 4] {
        let face = self.body.read_unsigned(2, 2) as usize;
        let mut orientation = [0.0_f32; 4];
        orientation[face] = 1.0;
        for (coordinate, (fraction_offset, coarse_offset)) in
            [(5, 7), (28, 30), (51, 53)].into_iter().enumerate()
        {
            let raw = (self.body.read_unsigned(coarse_offset, 21) << 2)
                | self.body.read_unsigned(fraction_offset, 2);
            orientation[(face + coordinate + 1) & 3] =
                (raw as i32 - 0x40_0000) as f32 / 0x40_0000 as f32;
        }
        let norm = orientation
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        orientation.map(|value| value / norm)
    }

    fn fractional_vector(&self, coarse_offset: usize, range_shift: u32) -> [f32; 3] {
        std::array::from_fn(|axis| {
            let lane_offset = axis * 22;
            let coarse = self.body.read_signed(coarse_offset + lane_offset, 14) as f32;
            let fraction =
                self.body.read_unsigned(coarse_offset - 8 + lane_offset, 8) as f32 / 256.0;
            (coarse + fraction) * (1_u32 << range_shift) as f32
        })
    }
}
