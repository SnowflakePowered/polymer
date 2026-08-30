use super::{NativeMotionHeader, PackedMotionBody};
use crate::decoder::{MotionFrame, TimedMotionVector};
use std::time::Instant;

const TIMING_CLASS: u8 = 0x0f;
const ACCELEROMETER_EARLIER_AGE_TICKS: u16 = 18;
const GYROSCOPE_EARLIER_AGE_TICKS: u16 = 10;
const ACCELEROMETER_MIDDLE_AGE_TICKS: u16 = 8;
const GYROSCOPE_TERMINAL_AGE_TICKS: u16 = 2;

pub(super) struct ExtendedMotion<'a> {
    pub header: &'a NativeMotionHeader,
    body: PackedMotionBody<'a>,
}

impl<'a> ExtendedMotion<'a> {
    pub fn parse(header: &'a NativeMotionHeader, body: &'a [u8]) -> Option<Self> {
        (PackedMotionBody::has_full_length(body) && header.timing_class() == TIMING_CLASS)
            .then_some(Self {
                header,
                body: PackedMotionBody::new(body),
            })
    }

    pub fn decode(&self, received_at: Instant, sample_span_ticks: u16) -> MotionFrame {
        let mode = self.body.read_unsigned(0, 2);
        let accelerometer_range_shift = if mode & 1 == 0 { 2 } else { 0 };
        let gyroscope_range_multiplier = if mode & 2 == 0 { 4.0 } else { 1.0 };
        let accelerometer = [
            self.accelerometer_sample(
                68,
                14,
                14,
                accelerometer_range_shift,
                self.header
                    .sampled_at_in_span(received_at, 0, ACCELEROMETER_EARLIER_AGE_TICKS),
            ),
            self.accelerometer_sample(
                158,
                13,
                13,
                1 + accelerometer_range_shift,
                self.header
                    .sampled_at_in_span(received_at, 0, ACCELEROMETER_MIDDLE_AGE_TICKS),
            ),
            TimedMotionVector {
                value: PackedMotionBody::accelerometer(self.body.raw_vector(
                    245,
                    14,
                    14,
                    accelerometer_range_shift,
                )),
                sampled_at: received_at,
            },
        ];
        let gyroscope = [
            self.gyroscope_sample(
                110,
                gyroscope_range_multiplier,
                self.header
                    .sampled_at_in_span(received_at, 0, GYROSCOPE_EARLIER_AGE_TICKS),
            ),
            TimedMotionVector {
                value: self.gyroscope(197, gyroscope_range_multiplier),
                sampled_at: self.header.sampled_at_in_span(
                    received_at,
                    0,
                    GYROSCOPE_TERMINAL_AGE_TICKS,
                ),
            },
        ];
        MotionFrame {
            orientation: self.body.orientation(),
            orientation_sampled_at: self.header.sampled_at_in_span(
                received_at,
                Self::orientation_tick(sample_span_ticks),
                sample_span_ticks,
            ),
            accelerometer,
            gyroscope,
            filtered_accelerometer: None,
            raw_samples_updated: true,
            temperature_celsius: f32::NAN,
            sensor_timestamp: self.header.sensor_timestamp(),
            elapsed_ticks: sample_span_ticks,
        }
    }

    fn accelerometer_sample(
        &self,
        offset: usize,
        width: usize,
        stride: usize,
        shift: u32,
        sampled_at: Instant,
    ) -> TimedMotionVector {
        TimedMotionVector {
            value: PackedMotionBody::accelerometer(
                self.body.raw_vector(offset, width, stride, shift),
            ),
            sampled_at,
        }
    }

    fn gyroscope_sample(
        &self,
        offset: usize,
        range_multiplier: f32,
        sampled_at: Instant,
    ) -> TimedMotionVector {
        TimedMotionVector {
            value: self.gyroscope(offset, range_multiplier),
            sampled_at,
        }
    }

    fn gyroscope(&self, offset: usize, range_multiplier: f32) -> [f32; 3] {
        let fixed_point = self.body.raw_vector(offset, 16, 16, 0);
        PackedMotionBody::gyroscope(fixed_point.map(|value| value as f32 / 4.0 * range_multiplier))
    }

    fn orientation_tick(sample_span_ticks: u16) -> u16 {
        let report_intervals = sample_span_ticks.saturating_add(6) / 12;
        match report_intervals {
            0 | 1 => 0,
            intervals => 4 + 8 * intervals.saturating_sub(2),
        }
        .min(sample_span_ticks)
    }
}
