use super::DecodedInput;
use crate::decoder::{MotionFrame, TimedMotionVector};
use std::time::{Duration, Instant};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, byteorder::little_endian::U32};

mod continuation;
mod extended;
mod orientation;
mod precision;

use continuation::ContinuationMotion;
use extended::ExtendedMotion;
use orientation::OrientationTracker;
use precision::PrecisionMotion;

const NORMAL_MOTION_LENGTH: usize = 40;
const NORMAL_TIMING_CLASS: u8 = 0x0e;
const NORMAL_ORIENTATION_AGE_TICKS: u16 = 12;
const SENSOR_TICKS_PER_SECOND: f64 = 800.0;
const MAX_CONTINUOUS_INTERVAL_TICKS: u16 = 100;
const MAX_PRECISION_DELAY_TICKS: u16 = 8;
const PRECISION_ACCELERATION_OFFSET_TICKS: f64 = 1.5;
pub(super) const GRAVITY_METERS_PER_SECOND_SQUARED: f32 = 9.80665;

#[derive(Clone, Copy)]
pub(super) struct NativeMotionLayout {
    motion_length_offset: usize,
    motion_offset: usize,
}

impl NativeMotionLayout {
    pub const FULL_REPORT: Self = Self {
        motion_length_offset: 0x0e,
        motion_offset: 0x0f,
    };

    pub const HALF_REPORT: Self = Self {
        motion_length_offset: 0x0f,
        motion_offset: 0x10,
    };
}

#[derive(Default)]
pub(super) struct NativeMotionDecoder {
    orientation_tracker: OrientationTracker,
    last_motion: Option<MotionFrame>,
    last_normal_timing: Option<NormalTiming>,
}

impl NativeMotionDecoder {
    pub fn update_input(
        &mut self,
        report: &[u8],
        received_at: Instant,
        layout: NativeMotionLayout,
        input: &mut DecodedInput,
    ) {
        self.retain_motion(input);
        let Some(block) = NativeMotionBlock::parse(report, layout) else {
            return;
        };
        if let Some(packet) = PackedMotion::parse(block.header, block.body) {
            self.update_normal(packet, received_at, input);
        } else if let Some(packet) = ExtendedMotion::parse(block.header, block.body) {
            self.update_extended(packet, received_at, input);
        } else if let Some(packet) = PrecisionMotion::parse(block.header, block.body) {
            self.update_precision(packet, input);
        } else if let Some(packet) = ContinuationMotion::parse(block.header, block.body) {
            self.update_continuation(packet, received_at, input);
        }
    }

    fn update_normal(
        &mut self,
        packet: PackedMotion<'_>,
        received_at: Instant,
        input: &mut DecodedInput,
    ) {
        let previous_timing = self.last_normal_timing;
        let continuous_interval =
            previous_timing.and_then(|timing| timing.interval_to(packet.header.sensor_timestamp()));
        if previous_timing.is_some() && continuous_interval.is_none() {
            self.reset_session(input);
        }
        let interval_ticks = continuous_interval.unwrap_or_else(|| packet.header.elapsed_ticks());
        let motion = packet.decode(received_at, interval_ticks);
        self.publish_normal_motion(motion, packet.header, received_at, input);
    }

    fn update_extended(
        &mut self,
        packet: ExtendedMotion<'_>,
        received_at: Instant,
        input: &mut DecodedInput,
    ) {
        let previous_timing = self.last_normal_timing;
        let continuous_interval =
            previous_timing.and_then(|timing| timing.interval_to(packet.header.sensor_timestamp()));
        if previous_timing.is_some() && continuous_interval.is_none() {
            self.reset_session(input);
        }
        let interval_ticks = continuous_interval.unwrap_or_else(|| packet.header.elapsed_ticks());
        let mut motion = packet.decode(received_at, interval_ticks);
        if let Some(previous) = self.last_motion {
            motion.temperature_celsius = previous.temperature_celsius;
        }
        self.publish_normal_motion(motion, packet.header, received_at, input);
    }

    fn publish_normal_motion(
        &mut self,
        mut motion: MotionFrame,
        header: &NativeMotionHeader,
        received_at: Instant,
        input: &mut DecodedInput,
    ) {
        motion.orientation = self.orientation_tracker.update(motion);
        input.state.accelerometer = motion.accelerometer[2].value;
        input.state.gyroscope = motion.gyroscope[1].value;
        input.motion = Some(motion);
        self.last_motion = Some(motion);
        self.last_normal_timing = Some(NormalTiming {
            terminal_at: received_at,
            sensor_timestamp: header.sensor_timestamp(),
            interval_ticks: motion.elapsed_ticks,
        });
    }

    fn update_precision(&mut self, packet: PrecisionMotion<'_>, input: &mut DecodedInput) {
        let (Some(mut motion), Some(timing)) = (self.last_motion, self.last_normal_timing) else {
            return;
        };
        if !timing.precedes_precision(packet.header.sensor_timestamp()) {
            self.reset_session(input);
            return;
        }
        let filtered_accelerometer = TimedMotionVector {
            value: packet.filtered_accelerometer(),
            sampled_at: timing.precision_acceleration_sampled_at(),
        };
        motion.orientation = packet.orientation();
        motion.orientation_sampled_at = timing.precision_orientation_sampled_at();
        motion.filtered_accelerometer = Some(filtered_accelerometer);
        motion.raw_samples_updated = false;
        motion.temperature_celsius = packet.temperature_celsius();
        motion.sensor_timestamp = packet.header.sensor_timestamp();
        motion.elapsed_ticks = packet.header.elapsed_ticks();
        motion.orientation = self.orientation_tracker.update(motion);
        input.state.accelerometer = filtered_accelerometer.value;
        input.state.gyroscope = motion.gyroscope[1].value;
        input.motion = Some(motion);
        self.last_motion = Some(motion);
    }

    fn update_continuation(
        &mut self,
        packet: ContinuationMotion<'_>,
        received_at: Instant,
        input: &mut DecodedInput,
    ) {
        let (Some(previous), Some(timing)) = (self.last_motion, self.last_normal_timing) else {
            return;
        };
        let Some(interval_ticks) = timing.interval_to(packet.header.sensor_timestamp()) else {
            self.reset_session(input);
            return;
        };
        let mut motion = packet.decode(previous, received_at, interval_ticks, timing.terminal_at);
        motion.orientation = self.orientation_tracker.update(motion);
        input.state.accelerometer = motion.accelerometer[2].value;
        input.state.gyroscope = motion.gyroscope[1].value;
        input.motion = Some(motion);
        self.last_motion = Some(motion);
        self.last_normal_timing = Some(NormalTiming {
            terminal_at: received_at,
            sensor_timestamp: packet.header.sensor_timestamp(),
            interval_ticks,
        });
    }

    fn retain_motion(&self, input: &mut DecodedInput) {
        let Some(motion) = self.last_motion else {
            return;
        };
        input.state.accelerometer = motion
            .filtered_accelerometer
            .map(|sample| sample.value)
            .unwrap_or(motion.accelerometer[2].value);
        input.state.gyroscope = motion.gyroscope[1].value;
        input.motion = Some(motion);
    }

    fn reset_session(&mut self, input: &mut DecodedInput) {
        self.orientation_tracker = OrientationTracker::default();
        self.last_motion = None;
        self.last_normal_timing = None;
        input.state.accelerometer = [0.0; 3];
        input.state.gyroscope = [0.0; 3];
        input.motion = None;
    }
}

#[derive(Clone, Copy)]
struct NormalTiming {
    terminal_at: Instant,
    sensor_timestamp: u16,
    interval_ticks: u16,
}

impl NormalTiming {
    fn interval_to(self, sensor_timestamp: u16) -> Option<u16> {
        let interval = sensor_timestamp.wrapping_sub(self.sensor_timestamp) & 0x0fff;
        (interval > 0 && interval <= MAX_CONTINUOUS_INTERVAL_TICKS).then_some(interval)
    }

    fn precedes_precision(self, sensor_timestamp: u16) -> bool {
        let delay = sensor_timestamp.wrapping_sub(self.sensor_timestamp) & 0x0fff;
        delay > 0 && delay <= MAX_PRECISION_DELAY_TICKS
    }

    fn precision_orientation_sampled_at(self) -> Instant {
        self.terminal_at
            .checked_sub(Duration::from_secs_f64(
                f64::from(self.interval_ticks) / 3.0 / SENSOR_TICKS_PER_SECOND,
            ))
            .unwrap_or(self.terminal_at)
    }

    fn precision_acceleration_sampled_at(self) -> Instant {
        self.terminal_at
            .checked_add(Duration::from_secs_f64(
                PRECISION_ACCELERATION_OFFSET_TICKS / SENSOR_TICKS_PER_SECOND,
            ))
            .unwrap_or(self.terminal_at)
    }
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct NativeMotionHeader {
    value: U32,
}

impl NativeMotionHeader {
    fn sensor_timestamp(&self) -> u16 {
        (self.value.get() & 0x0fff) as u16
    }

    fn elapsed_ticks(&self) -> u16 {
        ((self.value.get() >> 12) & 0x0fff) as u16
    }

    fn timing_class(&self) -> u8 {
        ((self.value.get() >> 24) & 0x0f) as u8
    }

    fn sampled_at_in_span(&self, received_at: Instant, tick: u16, span_ticks: u16) -> Instant {
        let ticks_before_terminal = span_ticks.saturating_sub(tick);
        received_at
            .checked_sub(Duration::from_secs_f64(
                f64::from(ticks_before_terminal) / SENSOR_TICKS_PER_SECOND,
            ))
            .unwrap_or(received_at)
    }
}

struct NativeMotionBlock<'a> {
    header: &'a NativeMotionHeader,
    body: &'a [u8],
}

impl<'a> NativeMotionBlock<'a> {
    fn parse(report: &'a [u8], layout: NativeMotionLayout) -> Option<Self> {
        let motion_length = usize::from(*report.get(layout.motion_length_offset)?);
        let motion_end = layout.motion_offset.checked_add(motion_length)?;
        let motion = report.get(layout.motion_offset..motion_end)?;
        let (header, body) = NativeMotionHeader::ref_from_prefix(motion).ok()?;
        Some(Self { header, body })
    }
}

pub(super) struct PackedMotionBody<'a> {
    bytes: &'a [u8],
}

impl<'a> PackedMotionBody<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub fn has_full_length(bytes: &[u8]) -> bool {
        bytes.len() + size_of::<NativeMotionHeader>() == NORMAL_MOTION_LENGTH
    }

    pub fn read_unsigned(&self, offset: usize, width: usize) -> u32 {
        let mut value = 0_u32;
        for bit in 0..width {
            let source = offset + bit;
            value |= u32::from((self.bytes[source / 8] >> (source & 7)) & 1) << bit;
        }
        value
    }

    pub fn read_signed(&self, offset: usize, width: usize) -> i32 {
        let shift = u32::BITS as usize - width;
        ((self.read_unsigned(offset, width) << shift) as i32) >> shift
    }

    pub fn raw_vector(&self, offset: usize, width: usize, stride: usize, shift: u32) -> [i32; 3] {
        std::array::from_fn(|axis| self.read_signed(offset + axis * stride, width) << shift)
    }

    pub fn accelerometer(raw: [i32; 3]) -> [f32; 3] {
        Self::fractional_accelerometer(raw.map(|value| value as f32))
    }

    pub fn fractional_accelerometer(raw: [f32; 3]) -> [f32; 3] {
        raw.map(|value| value / 4096.0 * GRAVITY_METERS_PER_SECOND_SQUARED)
    }

    pub fn gyroscope(raw: [f32; 3]) -> [f32; 3] {
        let scale = std::f32::consts::PI / 180.0 / 16.4;
        raw.map(|value| value * scale)
    }

    pub fn orientation(&self) -> [f32; 4] {
        let face = self.read_unsigned(2, 2) as usize;
        let mut orientation = [0.0_f32; 4];
        orientation[face] = 1.0;
        for (coordinate, offset) in [5, 26, 47].into_iter().enumerate() {
            let raw = self.read_unsigned(offset, 21) as i32;
            orientation[(face + coordinate + 1) & 3] = (raw - 0x10_0000) as f32 / 0x10_0000 as f32;
        }
        let norm = orientation
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        orientation.map(|value| value / norm)
    }
}

struct PackedMotion<'a> {
    header: &'a NativeMotionHeader,
    body: PackedMotionBody<'a>,
}

impl<'a> PackedMotion<'a> {
    fn parse(header: &'a NativeMotionHeader, body: &'a [u8]) -> Option<Self> {
        (PackedMotionBody::has_full_length(body) && header.timing_class() == NORMAL_TIMING_CLASS)
            .then_some(Self {
                header,
                body: PackedMotionBody::new(body),
            })
    }

    fn decode(&self, received_at: Instant, sample_span_ticks: u16) -> MotionFrame {
        let mode = self.body.read_unsigned(0, 2);
        let accelerometer_range_shift = if mode & 1 == 0 { 2 } else { 0 };
        let gyroscope_range_shift = if mode & 2 == 0 { 2 } else { 0 };
        let first_tick = (sample_span_ticks + 1) / 3;
        let second_tick = sample_span_ticks.saturating_mul(2).div_ceil(3);
        let accelerometer = [
            TimedMotionVector {
                value: PackedMotionBody::accelerometer(self.body.raw_vector(
                    68,
                    14,
                    14,
                    accelerometer_range_shift,
                )),
                sampled_at: self.header.sampled_at_in_span(
                    received_at,
                    first_tick,
                    sample_span_ticks,
                ),
            },
            TimedMotionVector {
                value: PackedMotionBody::accelerometer(self.body.raw_vector(
                    149,
                    13,
                    13,
                    1 + accelerometer_range_shift,
                )),
                sampled_at: self.header.sampled_at_in_span(
                    received_at,
                    second_tick,
                    sample_span_ticks,
                ),
            },
            TimedMotionVector {
                value: PackedMotionBody::accelerometer(self.body.raw_vector(
                    230,
                    14,
                    14,
                    accelerometer_range_shift,
                )),
                sampled_at: received_at,
            },
        ];
        let gyroscope = [
            TimedMotionVector {
                value: PackedMotionBody::gyroscope(
                    self.body
                        .raw_vector(110, 13, 13, 1 + gyroscope_range_shift)
                        .map(|value| value as f32),
                ),
                sampled_at: self.header.sampled_at_in_span(
                    received_at,
                    second_tick,
                    sample_span_ticks,
                ),
            },
            TimedMotionVector {
                value: PackedMotionBody::gyroscope(
                    self.body
                        .raw_vector(188, 14, 14, gyroscope_range_shift)
                        .map(|value| value as f32),
                ),
                sampled_at: received_at,
            },
        ];
        MotionFrame {
            orientation: self.body.orientation(),
            orientation_sampled_at: self.header.sampled_at_in_span(
                received_at,
                0,
                NORMAL_ORIENTATION_AGE_TICKS,
            ),
            accelerometer,
            gyroscope,
            filtered_accelerometer: None,
            raw_samples_updated: true,
            temperature_celsius: self.body.read_signed(272, 16) as f32 / 128.0 + 25.0,
            sensor_timestamp: self.header.sensor_timestamp(),
            elapsed_ticks: sample_span_ticks,
        }
    }
}
