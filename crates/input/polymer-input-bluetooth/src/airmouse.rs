//! Wii MotionPlus pointer integration owned by the input provider.

use std::time::Instant;

use crate::decoder::wiimote::{Accelerometer, Gyroscope};

const RAW_GYRO_CENTER: f32 = 8192.0;
const SLOW_RAW_PER_RADIAN_PER_SECOND: f32 = 923.5;
const FAST_RAW_PER_RADIAN_PER_SECOND: f32 = 207.8;
const POINTER_GAIN: f32 = 0.78;
const MAX_TIMESTEP_SECONDS: f32 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerPosition {
    pub x: f32,
    pub y: f32,
}

impl PointerPosition {
    pub const CENTER: Self = Self { x: 0.5, y: 0.5 };

    pub fn clamped(self) -> Self {
        Self {
            x: self.x.clamp(0.0, 1.0),
            y: self.y.clamp(0.0, 1.0),
        }
    }
}

#[derive(Debug)]
pub struct AirMouse {
    position: PointerPosition,
    horizontal_angle: f32,
    vertical_angle: f32,
    gyro_bias: [f32; 3],
    gravity_reference_pitch: Option<f32>,
    last_accelerometer: Option<Accelerometer>,
    last_update: Option<Instant>,
}

impl Default for AirMouse {
    fn default() -> Self {
        Self {
            position: PointerPosition::CENTER,
            horizontal_angle: 0.0,
            vertical_angle: 0.0,
            gyro_bias: [RAW_GYRO_CENTER; 3],
            gravity_reference_pitch: None,
            last_accelerometer: None,
            last_update: None,
        }
    }
}

impl AirMouse {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_pointer_position(
        &mut self,
        accelerometer: Accelerometer,
        gyroscope: Gyroscope,
        timestamp: Instant,
    ) -> PointerPosition {
        self.last_accelerometer = Some(accelerometer);
        let accel_pitch = accelerometer_pitch(accelerometer);
        self.gravity_reference_pitch.get_or_insert(accel_pitch);
        let dt = self.last_update.replace(timestamp).map_or(0.0, |previous| {
            timestamp
                .saturating_duration_since(previous)
                .as_secs_f32()
                .min(MAX_TIMESTEP_SECONDS)
        });
        if dt > 0.0 {
            let pitch_rate = raw_rate(gyroscope.pitch, self.gyro_bias[0], gyroscope.pitch_slow);
            let roll_rate = raw_rate(gyroscope.roll, self.gyro_bias[1], gyroscope.roll_slow);
            let yaw_rate = raw_rate(gyroscope.yaw, self.gyro_bias[2], gyroscope.yaw_slow);
            self.horizontal_angle -= yaw_rate * dt;
            self.vertical_angle += pitch_rate * dt;
            self.update_stationary_bias(
                accelerometer,
                gyroscope,
                [pitch_rate, roll_rate, yaw_rate],
            );
            self.update_position();
        }
        self.position
    }

    pub fn center(&mut self) {
        self.position = PointerPosition::CENTER;
        self.horizontal_angle = 0.0;
        self.vertical_angle = 0.0;
        self.last_update = None;
        if let Some(accelerometer) = self.last_accelerometer {
            self.gravity_reference_pitch = Some(accelerometer_pitch(accelerometer));
        }
    }

    pub fn recalibrate(&mut self, accelerometer: Accelerometer, gyroscope: Gyroscope) {
        self.gyro_bias = [
            f32::from(gyroscope.pitch),
            f32::from(gyroscope.roll),
            f32::from(gyroscope.yaw),
        ];
        self.last_accelerometer = Some(accelerometer);
        self.gravity_reference_pitch = Some(accelerometer_pitch(accelerometer));
        self.last_update = None;
        self.center();
    }

    pub fn track_absolute_position(&mut self, position: PointerPosition, timestamp: Instant) {
        self.position = position.clamped();
        self.horizontal_angle = (self.position.x - 0.5) / POINTER_GAIN;
        self.vertical_angle = (self.position.y - 0.5) / POINTER_GAIN;
        self.last_update = Some(timestamp);
    }

    fn update_position(&mut self) {
        self.position = PointerPosition {
            x: 0.5 + self.horizontal_angle * POINTER_GAIN,
            y: 0.5 + self.vertical_angle * POINTER_GAIN,
        }
        .clamped();
        self.horizontal_angle = (self.position.x - 0.5) / POINTER_GAIN;
        self.vertical_angle = (self.position.y - 0.5) / POINTER_GAIN;
    }

    fn update_stationary_bias(
        &mut self,
        accelerometer: Accelerometer,
        gyroscope: Gyroscope,
        rates: [f32; 3],
    ) {
        let Some(reference_pitch) = self.gravity_reference_pitch else {
            return;
        };
        let gravity_is_steady =
            wrap_angle(accelerometer_pitch(accelerometer) - reference_pitch).abs() < 0.025;
        let gyro_is_steady = rates.into_iter().all(|rate| rate.abs() < 0.035);
        if gravity_is_steady && gyro_is_steady {
            const BIAS_BLEND: f32 = 0.002;
            for (bias, sample) in
                self.gyro_bias
                    .iter_mut()
                    .zip([gyroscope.pitch, gyroscope.roll, gyroscope.yaw])
            {
                *bias += (f32::from(sample) - *bias) * BIAS_BLEND;
            }
        }
    }
}

fn raw_rate(raw: u16, bias: f32, slow: bool) -> f32 {
    (f32::from(raw) - bias)
        / if slow {
            SLOW_RAW_PER_RADIAN_PER_SECOND
        } else {
            FAST_RAW_PER_RADIAN_PER_SECOND
        }
}

fn accelerometer_pitch(accelerometer: Accelerometer) -> f32 {
    let x = f32::from(accelerometer.x) - 512.0;
    let y = f32::from(accelerometer.y) - 512.0;
    let z = f32::from(accelerometer.z) - 512.0;
    (-x).atan2((y * y + z * z).sqrt())
}

fn wrap_angle(angle: f32) -> f32 {
    (angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}
